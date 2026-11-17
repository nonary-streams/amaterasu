use super::*;
use serde_json::Value;

fn event(publisher: &str, sequence: u64, payload: Value) -> ModuleResult<Response> {
    Ok(Response::Event {
        publisher: publisher.into(),
        topic: STREAM_REPORTS.name().into(),
        sequence,
        payload,
    })
}

fn scripted(
    replies: Vec<ModuleResult<Response>>,
) -> (
    impl FnMut(Command) -> ModuleResult<Response>,
    std::rc::Rc<std::cell::RefCell<Vec<Value>>>,
) {
    let sent = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let log = sent.clone();
    let mut replies: VecDeque<_> = replies.into();
    let send = move |command: Command| {
        log.borrow_mut()
            .push(serde_json::to_value(&command).unwrap());
        replies.pop_front().unwrap_or(Ok(Response::Pending))
    };
    (send, sent)
}

#[test]
fn constructors_record_the_calling_site_and_disposition() {
    let line = line!() + 1;
    let issue = StreamIssue::critical("op-1", LifecycleLayer::Rtsp, "open_rtsp", "port_busy");
    assert_eq!(issue.site, format!("{}:{line}", file!()));
    assert!(issue.is_critical());
    assert!(issue.valid());

    let demoted = StreamIssue::demoted(
        "op-1",
        LifecycleLayer::FrameLimiter,
        "resource_plan",
        "frame_limiter_unavailable",
        "noop_frame_limiter",
    )
    .because("worker_not_ready");
    assert_eq!(demoted.disposition, IssueDisposition::DemotedWarning);
    assert_eq!(
        demoted
            .to_stream_warning("amaterasu.frame-limiter/1")
            .reason,
        "worker_not_ready"
    );
}

#[test]
fn issues_cannot_carry_unbounded_or_free_text_fields() {
    let mut issue = StreamIssue::advisory("op", LifecycleLayer::Display, "apply", "mode_kept");
    assert!(issue.valid());
    issue.reason = "a sentence with spaces".into();
    assert!(!issue.valid());
    issue.reason = "x".repeat(65);
    assert!(!issue.valid());

    let mut demoted =
        StreamIssue::demoted("op", LifecycleLayer::Display, "apply", "mode_kept", "keep");
    demoted.fallback = None;
    assert!(!demoted.valid(), "a demoted warning must name its fallback");
}

#[test]
fn reporting_publishes_on_the_stream_issue_topic_and_refuses_invalid_issues() {
    let (mut send, sent) = scripted(vec![Ok(Response::Ok)]);
    let issue = StreamIssue::critical("op", LifecycleLayer::StreamSession, "wait_video", "failed");
    issue.report_with(&mut send).unwrap();
    let published = &sent.borrow()[0];
    assert_eq!(published["op"], "publish");
    assert_eq!(published["topic"], "amaterasu.stream.report/1");
    assert_eq!(published["payload"]["kind"], "issue");
    assert_eq!(published["payload"]["code"], "failed");
    assert_eq!(published["payload"]["disposition"], "critical_failure");

    let mut invalid = issue.clone();
    invalid.code = "not valid".into();
    assert!(invalid.report_with(&mut send).is_err());
    assert_eq!(sent.borrow().len(), 1, "an invalid issue is never sent");
}

#[test]
fn an_outbox_keeps_what_it_could_not_publish_and_counts_what_it_dropped() {
    let mut outbox = StreamIssueOutbox::default();
    for index in 0..MAX_OUTBOX_ISSUES + 2 {
        outbox.push(StreamIssue::advisory(
            "op",
            LifecycleLayer::Input,
            "bind",
            format!("code_{index}"),
        ));
    }
    assert_eq!(outbox.len(), MAX_OUTBOX_ISSUES);
    assert_eq!(outbox.dropped(), 2);

    let (mut refused, _) = scripted(vec![
        Ok(Response::Ok),
        Ok(Response::error("permission_denied", "not granted")),
    ]);
    assert!(outbox.flush_with(&mut refused).is_err());
    assert_eq!(outbox.len(), MAX_OUTBOX_ISSUES - 1, "the rest waits");

    let (mut accepted, sent) = scripted((0..MAX_OUTBOX_ISSUES).map(|_| Ok(Response::Ok)).collect());
    assert_eq!(
        outbox.flush_with(&mut accepted).unwrap(),
        MAX_OUTBOX_ISSUES - 1
    );
    assert!(outbox.is_empty());
    assert_eq!(sent.borrow()[0]["payload"]["code"], "code_3");
}

#[test]
fn the_first_critical_failure_stays_the_root_cause() {
    let mut store = StreamLifecycleStore::new();
    store.open("rtsp-42");
    store.commit_at(
        1,
        "amaterasu.nvhttp",
        StreamIssue::demoted(
            "rtsp-42",
            LifecycleLayer::Nvhttp,
            "read_proton_policy",
            "proton_policy_unavailable",
            "disable_proton_dualsense_compatibility",
        ),
    );
    store.commit_at(
        2,
        "amaterasu.stream-session",
        StreamIssue::critical(
            "rtsp-42",
            LifecycleLayer::EncodeVideo,
            "wait_video_ready",
            "video_startup_failed",
        )
        .because("encoder_unavailable"),
    );
    store.commit_at(
        3,
        "amaterasu.nvhttp",
        StreamIssue::critical(
            "rtsp-42",
            LifecycleLayer::Rtsp,
            "cleanup",
            "rtsp_cleanup_unconfirmed",
        ),
    );

    let snapshot = store.snapshot("rtsp-42").unwrap();
    assert!(!snapshot.healthy);
    let root = snapshot.root_cause.unwrap();
    assert_eq!(root.publisher, "amaterasu.stream-session");
    assert_eq!(root.issue.layer, LifecycleLayer::EncodeVideo);
    assert_eq!(root.issue.reason, "encoder_unavailable");
    assert_eq!(snapshot.warnings.len(), 1);
    assert_eq!(snapshot.committed, 3);
    assert_eq!(
        snapshot
            .entries
            .iter()
            .map(|entry| entry.seq)
            .collect::<Vec<_>>(),
        [1, 2, 3]
    );
}

#[test]
fn issues_reach_their_stream_through_derived_ids_and_aliases() {
    let mut store = StreamLifecycleStore::new();
    store.open("launch-1");
    store.open("launch-10");
    store.alias("launch-1", "app-1");

    for (operation, expected) in [
        ("launch-1", Some("launch-1")),
        ("launch-1.release-history.retry.3", Some("launch-1")),
        ("launch-10-ticket", Some("launch-10")),
        ("app-1-lifecycle-prepare", Some("launch-1")),
        ("app-1", Some("launch-1")),
        ("launch-1x", None),
        ("unrelated", None),
    ] {
        assert_eq!(store.resolve(operation), expected, "{operation}");
    }

    store.commit(
        "example.plugin",
        StreamIssue::advisory("unrelated", LifecycleLayer::Other, "p", "c"),
    );
    assert_eq!(store.unattributed().count(), 1);
    assert_eq!(store.snapshot("launch-1").unwrap().committed, 0);
}

#[test]
fn the_store_forgets_the_oldest_stream_and_its_aliases() {
    let mut store = StreamLifecycleStore::new();
    for index in 0..=MAX_TRACKED_STREAMS {
        let key = format!("stream-{index}");
        store.open(&key);
        store.alias(&key, &format!("app-{index}"));
    }
    assert!(store.stream("stream-0").is_none());
    assert_eq!(store.resolve("app-0"), None);
    assert_eq!(store.resolve("app-1"), Some("stream-1"));
}

#[test]
fn transient_refusals_never_evict_an_open_stream() {
    let mut store = StreamLifecycleStore::new();
    for index in 0..MAX_TRACKED_STREAMS {
        store.open(&format!("stream-{index}"));
    }
    assert!(!store.open_transient("refused.1"));
    store.commit(
        "host",
        StreamIssue::critical("refused.1", LifecycleLayer::Nvhttp, "admission", "refused"),
    );
    assert_eq!(store.unattributed().count(), 1);
    assert!(store.stream("stream-0").is_some());
    store.end("stream-1");
    assert!(store.open_transient("refused.2"));
    assert!(store.stream("stream-0").is_some());
    assert!(store.stream("stream-1").is_none());
    assert!(store.stream("refused.2").is_some());
}

#[test]
fn a_feed_commits_events_with_the_runtime_publisher_and_marks_gaps() {
    let issue = StreamIssue::critical("launch-1-rtsp", LifecycleLayer::Rtsp, "arm", "rejected");
    let forged_invalid = json!({"kind":"issue","operation":"launch-1","layer":"rtsp","phase":"arm",
        "code":"has spaces","disposition":"critical_failure","site":"x:1"});
    let (mut send, sent) = scripted(vec![
        Ok(Response::Accepted { id: 9 }),
        event(
            "amaterasu.rtsp",
            1,
            serde_json::to_value(StreamReport::Issue(issue.clone())).unwrap(),
        ),
        event("example.plugin", 1, json!({"unexpected": true})),
        event("example.plugin", 2, forged_invalid),
        Ok(Response::ResyncRequired {
            topic: STREAM_REPORTS.name().into(),
        }),
        Ok(Response::Ok),
        Ok(Response::Accepted { id: 10 }),
        Ok(Response::Pending),
    ]);
    let mut store = StreamLifecycleStore::new();
    store.open("launch-1");
    let mut feed = StreamIssueFeed::new();
    assert_eq!(feed.poll_with(&mut send, &mut store, None, 16), 4);

    let stream = store.stream("launch-1").unwrap();
    assert_eq!(stream.root_cause().unwrap().publisher, "amaterasu.rtsp");
    assert!(stream.audit_gap());
    assert_eq!(store.gaps(), 1);
    assert_eq!(feed.malformed(), 2);
    assert_eq!(sent.borrow()[0]["op"], "subscribe");
    assert_eq!(sent.borrow()[0]["topic"], "amaterasu.stream.report/1");
}

#[test]
fn a_refused_subscription_is_retried_later_without_blocking_each_poll() {
    let (mut send, sent) = scripted(vec![Ok(Response::error("permission_denied", "no grant"))]);
    let mut store = StreamLifecycleStore::new();
    let mut feed = StreamIssueFeed::new();
    assert_eq!(feed.poll_with(&mut send, &mut store, None, 16), 0);
    assert!(feed.unavailable().is_some());
    for _ in 0..100 {
        feed.poll_with(&mut send, &mut store, None, 16);
    }
    assert_eq!(
        sent.borrow().len(),
        1,
        "no command is sent while backing off"
    );
}

#[test]
fn a_poll_reads_no_more_than_its_limit() {
    let issue = StreamIssue::advisory("launch-1", LifecycleLayer::Input, "bind", "fallback");
    let payload = serde_json::to_value(StreamReport::Issue(issue.clone())).unwrap();
    let mut replies = vec![Ok(Response::Accepted { id: 1 })];
    replies.extend((1..=5).map(|sequence| event("amaterasu.input", sequence, payload.clone())));
    let (mut send, _) = scripted(replies);
    let mut store = StreamLifecycleStore::new();
    store.open("launch-1");
    let mut feed = StreamIssueFeed::new();
    assert_eq!(feed.poll_with(&mut send, &mut store, None, 3), 3);
    assert_eq!(feed.poll_with(&mut send, &mut store, None, 3), 2);
}

#[test]
fn a_binding_files_earlier_and_later_provider_reports_under_the_stream() {
    let mut store = StreamLifecycleStore::new();
    store.open("launch-1");
    store.alias("launch-1", "app-1");
    // The video provider reports under the operation it was called with,
    // before the stream session's binding arrives.
    store.commit_at(
        1,
        "amaterasu.video",
        StreamIssue::critical(
            "ss-2a-video",
            LifecycleLayer::EncodeVideo,
            "running",
            "encoder_unavailable",
        ),
    );
    assert_eq!(store.unattributed().count(), 1);
    store.commit_at(
        2,
        "amaterasu.nvhttp",
        StreamIssue::critical(
            "launch-1",
            LifecycleLayer::Rtsp,
            "monitor_rtsp",
            "monitor_unavailable",
        ),
    );
    store.bind("launch-1", "ss-2a");
    // A binding may also name the stream through an alias.
    store.bind("app-1-lifecycle", "fresh-op-9");
    store.commit_at(
        3,
        "amaterasu.frame-limiter",
        StreamIssue::demoted(
            "fresh-op-9",
            LifecycleLayer::FrameLimiter,
            "resource_plan",
            "frame_limiter_unavailable",
            "noop_frame_limiter",
        ),
    );

    assert_eq!(store.unattributed().count(), 0);
    let stream = store.stream("launch-1").unwrap();
    let root = stream.root_cause().unwrap();
    assert_eq!(
        root.publisher, "amaterasu.video",
        "commit order picks the root cause"
    );
    assert_eq!(
        stream.entries().map(|entry| entry.seq).collect::<Vec<_>>(),
        [1, 2, 3]
    );
    assert_eq!(stream.warnings()[0].publisher, "amaterasu.frame-limiter");
}

#[test]
fn a_binding_to_an_unknown_stream_changes_nothing() {
    let mut store = StreamLifecycleStore::new();
    store.bind("never-opened", "ss-1");
    assert_eq!(store.resolve("ss-1-video"), None);
    assert!(!StreamBinding::new("launch", "has\ncontrol").valid());
    assert!(StreamBinding::new("launch", "scope/with:separators").valid());
    assert!(!StreamBinding::new("same", "same").valid());
}

#[test]
fn bindings_and_issues_travel_in_order_through_the_outbox() {
    let mut outbox = StreamIssueOutbox::default();
    outbox.push_binding(StreamBinding::new("launch-1", "ss-1"));
    outbox.push(StreamIssue::critical(
        "ss-1-video",
        LifecycleLayer::EncodeVideo,
        "open",
        "failed",
    ));
    let (mut send, sent) = scripted(vec![Ok(Response::Ok), Ok(Response::Ok)]);
    assert_eq!(outbox.flush_with(&mut send).unwrap(), 2);
    let sent = sent.borrow();
    assert_eq!(sent[0]["payload"]["kind"], "binding");
    assert_eq!(sent[0]["payload"]["alias"], "ss-1");
    assert_eq!(sent[1]["payload"]["kind"], "issue");
}

#[test]
fn a_dependency_s_own_report_outranks_a_caller_s_earlier_reaction() {
    let mut store = StreamLifecycleStore::new();
    store.open("launch-1");
    store.commit_at(
        1,
        "amaterasu.nvhttp",
        StreamIssue::critical(
            "launch-1",
            LifecycleLayer::Nvhttp,
            "open_rtsp",
            "rtsp_launch_rejected",
        )
        .caused_by(LifecycleLayer::Rtsp),
    );
    assert_eq!(
        store.stream("launch-1").unwrap().root_cause().unwrap().seq,
        1
    );
    store.commit_at(
        2,
        "amaterasu.rtsp",
        StreamIssue::critical(
            "launch-1",
            LifecycleLayer::Rtsp,
            "arm",
            "video_startup_failed",
        ),
    );
    store.commit_at(
        3,
        "amaterasu.nvhttp",
        StreamIssue::critical(
            "launch-1",
            LifecycleLayer::Nvhttp,
            "cleanup",
            "cleanup_failed",
        ),
    );
    let root = store
        .stream("launch-1")
        .unwrap()
        .root_cause()
        .unwrap()
        .clone();
    assert_eq!(root.publisher, "amaterasu.rtsp");
    assert!(root.issue.is_origin());
}

#[test]
fn a_failure_its_reporter_recovered_from_is_not_the_root_cause() {
    let mut store = StreamLifecycleStore::new();
    store.open("launch-1");
    let audio = || {
        StreamIssue::critical(
            "launch-1-audio",
            LifecycleLayer::AudioEncode,
            "encode",
            "codec_failed",
        )
    };
    store.commit_at(1, "amaterasu.audio-encode", audio());
    store.commit_at(
        2,
        "amaterasu.audio-encode",
        StreamIssue::demoted(
            "launch-1-audio",
            LifecycleLayer::AudioEncode,
            "recover",
            "codec_failed",
            "audio_encoder_reset",
        ),
    );
    assert!(
        store.stream("launch-1").unwrap().healthy(),
        "the stream continued"
    );

    store.commit_at(
        3,
        "amaterasu.rtsp",
        StreamIssue::critical("launch-1", LifecycleLayer::Rtsp, "monitor", "client_lost"),
    );
    let root = store
        .stream("launch-1")
        .unwrap()
        .root_cause()
        .unwrap()
        .clone();
    assert_eq!(root.publisher, "amaterasu.rtsp");

    // A demotion from another reporter recovers nothing.
    store.commit_at(
        4,
        "example.plugin",
        StreamIssue::demoted(
            "launch-1",
            LifecycleLayer::Rtsp,
            "monitor",
            "client_lost",
            "ignore",
        ),
    );
    assert_eq!(
        store.stream("launch-1").unwrap().root_cause().unwrap().seq,
        3
    );
}

#[test]
fn a_callers_fallback_recovers_a_providers_refusal() {
    let mut store = StreamLifecycleStore::new();
    store.open("launch-1");
    store.commit_at(
        1,
        "amaterasu.encode",
        StreamIssue::critical(
            "launch-1-encoder-attempt-0",
            LifecycleLayer::EncodeVideo,
            "nvenc.open",
            "session_limit",
        ),
    );
    store.commit_at(
        2,
        "amaterasu.video",
        StreamIssue::demoted(
            "launch-1-video",
            LifecycleLayer::EncodeVideo,
            "encoder",
            "encoder_fallback",
            "software_encoder",
        )
        .because("session_limit")
        .caused_by(LifecycleLayer::EncodeVideo),
    );
    assert!(store.stream("launch-1").unwrap().healthy());
}
