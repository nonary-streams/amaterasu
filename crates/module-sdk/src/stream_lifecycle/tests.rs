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
        if matches!(command, Command::Log { .. }) {
            return Ok(Response::Ok);
        }
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
    assert_eq!(issue.reason, "port_busy");
    assert_eq!(issue.clone().because("").reason, "port_busy");

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
fn the_report_preserves_the_observed_native_failure_across_late_binding_and_reactions() {
    let mut store = StreamLifecycleStore::new();
    store.open("launch-1");
    let native = NativeStreamFailure {
        service: "amaterasu.rtx-hdr/1".into(),
        call: "process".into(),
        status: amaterasu_media::MEDIA_UNSUPPORTED,
        capture_generation: Some(3),
    };
    let issue = StreamIssue::critical(
        "ss-2a-video",
        LifecycleLayer::EncodeVideo,
        "running",
        "frame_processor_failed",
    )
    .caused_by(LifecycleLayer::FrameProcessing)
    .with_native_failure(native.clone());
    assert!(issue.valid());
    assert_eq!(
        serde_json::from_value::<StreamIssue>(json!(issue)).unwrap(),
        issue
    );
    assert!(
        issue
            .description()
            .contains("amaterasu.rtx-hdr/1 returned MEDIA_UNSUPPORTED (13) from process")
    );
    store.commit_at(1, "amaterasu.linux-session-video", issue);
    store.commit_at(
        2,
        "amaterasu.stream-session",
        StreamIssue::critical(
            "launch-1",
            LifecycleLayer::StreamSession,
            "media",
            "video_hdr_metadata_unavailable",
        )
        .caused_by(LifecycleLayer::EncodeVideo),
    );
    store.commit_at(
        3,
        "amaterasu.nvhttp",
        StreamIssue::critical(
            "launch-1",
            LifecycleLayer::Nvhttp,
            "monitor_rtsp",
            "rtsp_stream_failed",
        )
        .caused_by(LifecycleLayer::Rtsp),
    );
    store.bind("launch-1", "ss-2a");
    let report = store.snapshot("launch-1").unwrap();
    let root = report.root_cause.unwrap();
    assert_eq!(root.issue.caused_by, Some(LifecycleLayer::FrameProcessing));
    assert_eq!(root.issue.native_failure.as_deref(), Some(&native));
    assert_eq!(root.issue.code, "frame_processor_failed");
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
fn optional_native_details_cannot_drop_an_otherwise_valid_report() {
    let issue = StreamIssue::critical(
        "attempt",
        LifecycleLayer::EncodeVideo,
        "running",
        "video_failed",
    );
    let mut payload = json!(StreamReport::Issue(issue.clone()));
    payload["future_context"] = json!({"not_yet_understood": true});
    payload["native_failure"] = json!({
        "service": "amaterasu.rtx-hdr/1", "call": "process", "status": 13, "future_context": 2,
    });
    let StreamReport::Issue(received) = serde_json::from_value(payload.clone()).unwrap() else {
        panic!("expected an issue");
    };
    assert!(received.valid());
    assert_eq!(received.native_failure.as_ref().unwrap().status, 13);

    for invalid in [
        json!("bad detail"),
        json!({"service":"/private/path", "call":"process", "status":13}),
    ] {
        payload["native_failure"] = invalid;
        assert_eq!(
            serde_json::from_value::<StreamReport>(payload.clone()).unwrap(),
            StreamReport::Issue(issue.clone())
        );
    }
    payload["code"] = json!("invalid code with spaces");
    assert!(
        !serde_json::from_value::<StreamReport>(payload)
            .unwrap()
            .valid()
    );
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
fn recent_issues_include_ended_and_concurrent_attempts_once_in_commit_order() {
    let mut store = StreamLifecycleStore::new();
    for key in ["ended", "active", "other"] {
        store.open(key);
        store.commit(
            "rtsp",
            StreamIssue::advisory(
                key,
                LifecycleLayer::Rtsp,
                "announce",
                "optional_client_features",
            ),
        );
    }
    store.end("ended");
    store.commit(
        "nvhttp",
        StreamIssue::critical("unbound", LifecycleLayer::Nvhttp, "launch", "refused"),
    );
    let (issues, incomplete) = store.recent_issues();
    assert!(!incomplete);
    assert_eq!(
        issues
            .iter()
            .map(|entry| entry.issue.operation.as_str())
            .collect::<Vec<_>>(),
        ["unbound", "other", "active", "ended"]
    );
    for _ in 0..40 {
        store.commit(
            "rtsp",
            StreamIssue::advisory(
                "active",
                LifecycleLayer::Rtsp,
                "announce",
                "optional_client_features",
            ),
        );
    }
    let (issues, incomplete) = store.recent_issues();
    assert!(incomplete);
    assert_eq!(issues.len(), 32);
    assert_eq!(issues[0].seq, 44);
}

#[test]
fn publication_retry_after_retirement_keeps_the_issue_and_logs_once() {
    let mut active = StreamIssueOutbox::default();
    active.push(
        StreamIssue::advisory(
            "attempt-1",
            LifecycleLayer::Rtsp,
            "announce",
            "bandwidth_adjusted",
        )
        .with_values(
            Some(WarningValue::Integer(10000)),
            Some(WarningValue::Integer(7500)),
        ),
    );
    let mut lines = Vec::new();
    let mut publications = 0;
    let mut send = |command| match command {
        Command::Log { level, message, .. } => {
            assert_eq!(level, LogLevel::Warning as u8);
            lines.push(message);
            Ok(Response::error("queue_full", "busy"))
        }
        Command::Publish { .. } => {
            publications += 1;
            if publications == 1 {
                Ok(Response::error("busy", "retry"))
            } else {
                Ok(Response::Ok)
            }
        }
        _ => panic!("unexpected command"),
    };
    assert!(active.flush_with(&mut send).is_err());
    let mut retired = StreamIssueOutbox::default();
    active.transfer_to(&mut retired);
    assert_eq!(retired.flush_with(&mut send).unwrap(), 1);
    assert!(active.is_empty() && retired.is_empty());
    assert_eq!(publications, 2);
    assert_eq!(lines.len(), 1);
    assert!(lines[0].contains("10000 kbps") && lines[0].contains("7500 kbps"));
    assert!(lines[0].contains("attempt-1"));
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
fn confirmed_fallback_recovers_a_late_report_only_for_the_failed_attempt() {
    let mut store = StreamLifecycleStore::new();
    store.open("launch-1");
    store.alias("launch-1", "video-1");
    store.commit(
        "video",
        StreamIssue::demoted(
            "video-1-capture-0",
            LifecycleLayer::EncodeVideo,
            "provider_fallback_confirmed",
            "capture_fallback",
            "alternate_capture",
        )
        .caused_by(LifecycleLayer::Capture)
        .because("capture_failed"),
    );
    store.commit(
        "capture",
        StreamIssue::critical(
            "video-1-capture-0",
            LifecycleLayer::Capture,
            "open",
            "capture_failed",
        ),
    );
    assert!(store.stream("launch-1").unwrap().healthy());
    store.commit(
        "capture",
        StreamIssue::critical(
            "video-1-capture-0",
            LifecycleLayer::Capture,
            "worker",
            "provider_specific_failure",
        ),
    );
    assert!(store.stream("launch-1").unwrap().healthy());
    store.commit(
        "capture",
        StreamIssue::critical(
            "video-1-capture-1",
            LifecycleLayer::Capture,
            "open",
            "capture_failed",
        ),
    );
    assert_eq!(
        store
            .stream("launch-1")
            .unwrap()
            .root_cause()
            .unwrap()
            .issue
            .operation,
        "video-1-capture-1"
    );

    let mut store = StreamLifecycleStore::new();
    store.open("launch-1");
    store.alias("launch-1", "video-1");
    store.commit(
        "capture",
        StreamIssue::critical(
            "video-1-capture-1",
            LifecycleLayer::Capture,
            "open",
            "capture_failed",
        ),
    );
    store.commit(
        "video",
        StreamIssue::demoted(
            "video-1-capture-0",
            LifecycleLayer::EncodeVideo,
            "provider_fallback_confirmed",
            "capture_fallback",
            "alternate_capture",
        )
        .caused_by(LifecycleLayer::Capture)
        .because("capture_failed"),
    );
    assert_eq!(
        store
            .stream("launch-1")
            .unwrap()
            .root_cause()
            .unwrap()
            .issue
            .operation,
        "video-1-capture-1"
    );
}

#[test]
fn confirmed_fallback_can_recover_a_provider_reaction_without_hiding_other_layers() {
    let mut store = StreamLifecycleStore::new();
    store.open("launch-1");
    store.commit(
        "display",
        StreamIssue::critical(
            "launch-1-display",
            LifecycleLayer::Display,
            "open",
            "output_lost",
        ),
    );
    store.commit(
        "capture",
        StreamIssue::critical(
            "launch-1-capture-0",
            LifecycleLayer::Capture,
            "open",
            "capture_output_lost",
        )
        .caused_by(LifecycleLayer::Display)
        .because("output_unavailable"),
    );
    store.commit(
        "video",
        StreamIssue::demoted(
            "launch-1-capture-0",
            LifecycleLayer::EncodeVideo,
            PROVIDER_FALLBACK_CONFIRMED,
            "capture_fallback",
            "alternate_capture",
        )
        .caused_by(LifecycleLayer::Capture)
        .because("output_unavailable"),
    );
    assert_eq!(
        store
            .stream("launch-1")
            .unwrap()
            .root_cause()
            .unwrap()
            .issue
            .layer,
        LifecycleLayer::Display
    );
    store.commit(
        "display",
        StreamIssue::demoted(
            "launch-1-display",
            LifecycleLayer::Display,
            "recover",
            "output_lost",
            "restored",
        ),
    );
    assert!(store.stream("launch-1").unwrap().healthy());
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
fn late_binding_keeps_an_earlier_recovered_failure_recovered() {
    let mut store = StreamLifecycleStore::new();
    store.open("launch-1");
    store.commit_at(
        1,
        "capture",
        StreamIssue::critical(
            "pending-capture-open",
            LifecycleLayer::Capture,
            "open",
            "capture_unavailable",
        ),
    );
    store.commit_at(
        2,
        "video",
        StreamIssue::demoted(
            "launch-1",
            LifecycleLayer::EncodeVideo,
            "open",
            "capture_fallback",
            "alternate_capture",
        )
        .because("capture_unavailable")
        .caused_by(LifecycleLayer::Capture),
    );
    store.bind("launch-1", "pending-capture");
    assert!(store.stream("launch-1").unwrap().healthy());
    assert_eq!(
        store
            .stream("launch-1")
            .unwrap()
            .entries()
            .map(|entry| entry.seq)
            .collect::<Vec<_>>(),
        [1, 2]
    );
    store.commit_at(
        3,
        "capture",
        StreamIssue::critical(
            "pending-capture-open",
            LifecycleLayer::Capture,
            "open",
            "capture_unavailable",
        ),
    );
    assert_eq!(
        store.stream("launch-1").unwrap().root_cause().unwrap().seq,
        3,
        "a genuinely later failure is not covered by an earlier fallback"
    );
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
fn warnings_keep_distinct_reasons_and_publishers_without_repeating_identical_events() {
    let mut store = StreamLifecycleStore::new();
    store.open("launch-1");
    let warning = |reason: &str| {
        StreamIssue::advisory(
            "launch-1",
            LifecycleLayer::Input,
            "input_batch",
            "input_refused",
        )
        .because(reason)
    };
    store.commit("session", warning("invalid_event"));
    store.commit("session", warning("invalid_event"));
    store.commit("session", warning("unsupported_event"));
    store.commit("other", warning("invalid_event"));
    let warnings = store.stream("launch-1").unwrap().warnings();
    assert_eq!(warnings.len(), 3);
    assert_eq!(
        warnings
            .iter()
            .map(|entry| (&*entry.publisher, &*entry.issue.reason))
            .collect::<Vec<_>>(),
        [
            ("session", "invalid_event"),
            ("session", "unsupported_event"),
            ("other", "invalid_event")
        ]
    );
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
fn recovery_of_a_non_root_failure_is_not_lost_when_the_root_recovers() {
    let mut store = StreamLifecycleStore::new();
    store.open("launch-1");
    for (layer, phase, code) in [
        (LifecycleLayer::Capture, "open", "capture_failed"),
        (LifecycleLayer::StreamSession, "cleanup", "cleanup_failed"),
    ] {
        store.commit(
            "owner",
            StreamIssue::critical("launch-1", layer, phase, code),
        );
    }
    store.commit(
        "owner",
        StreamIssue::demoted(
            "launch-1",
            LifecycleLayer::StreamSession,
            "cleanup",
            "cleanup_failed",
            "cleanup_confirmed",
        ),
    );
    assert_eq!(
        store
            .stream("launch-1")
            .unwrap()
            .root_cause()
            .unwrap()
            .issue
            .code,
        "capture_failed"
    );
    store.commit(
        "owner",
        StreamIssue::demoted(
            "launch-1",
            LifecycleLayer::Capture,
            "open",
            "capture_failed",
            "alternate_capture",
        ),
    );
    assert!(store.stream("launch-1").unwrap().healthy());
    store.commit(
        "owner",
        StreamIssue::critical(
            "launch-1",
            LifecycleLayer::StreamSession,
            "cleanup",
            "cleanup_failed",
        ),
    );
    assert_eq!(
        store
            .stream("launch-1")
            .unwrap()
            .root_cause()
            .unwrap()
            .issue
            .code,
        "cleanup_failed"
    );
}

#[test]
fn one_reporter_cannot_recover_another_layer_by_reusing_its_code() {
    let mut store = StreamLifecycleStore::new();
    store.open("launch-1");
    store.commit(
        "session",
        StreamIssue::critical("launch-1", LifecycleLayer::Capture, "open", "failed"),
    );
    store.commit(
        "session",
        StreamIssue::critical("launch-1", LifecycleLayer::Audio, "open", "failed"),
    );
    store.commit(
        "session",
        StreamIssue::demoted(
            "launch-1",
            LifecycleLayer::Audio,
            "recover",
            "failed",
            "audio_reset",
        ),
    );
    assert_eq!(
        store
            .stream("launch-1")
            .unwrap()
            .root_cause()
            .unwrap()
            .issue
            .layer,
        LifecycleLayer::Capture
    );
    store.commit(
        "session",
        StreamIssue::demoted(
            "launch-1",
            LifecycleLayer::Capture,
            "recover",
            "failed",
            "capture_reset",
        ),
    );
    assert!(store.stream("launch-1").unwrap().healthy());
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
