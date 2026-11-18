use crate::controller::Controller;
use crate::model::{Discovery, Operation, Settings};
use crate::stream::tests::{receivers, request};
use amaterasu_module_sdk::stream_lifecycle::{
    IssueDisposition, LifecycleLayer, StreamIssue, StreamReport,
};
use amaterasu_module_sdk::{Command, Response};

fn controller() -> Controller {
    Controller::new(Settings {
        network_provider: None,
        enabled: true,
        allow_remote: false,
        allowed_principals: vec!["owner".into()],
        ..Default::default()
    })
}

fn published(controller: &mut Controller) -> Vec<StreamIssue> {
    let mut commands = Vec::new();
    controller.publish_stream_issues(&mut |command| {
        commands.push(command);
        Ok(Response::Ok)
    });
    commands
        .into_iter()
        .filter_map(|command| match command {
            Command::Publish { topic, payload } if topic == "amaterasu.stream.report/1" => {
                assert_eq!(payload["kind"], "issue");
                match serde_json::from_value(payload).unwrap() {
                    StreamReport::Issue(issue) => Some(issue),
                    StreamReport::Binding(_) => None,
                }
            }
            _ => None,
        })
        .collect()
}

#[test]
fn a_refused_open_is_reported_under_the_callers_operation() {
    let (video, audio) = receivers();
    let mut open = request(&video, &audio, false);
    open.operation_id = Some("session-1-media".into());
    open.peer_ip = "192.0.2.1".parse().unwrap();
    let mut controller = controller();
    controller.enable_stream_issue_reports();
    assert_eq!(
        controller.handle("owner", Operation::Open(open)).err(),
        Some("remote_disabled")
    );

    let issues = published(&mut controller);
    assert_eq!(issues.len(), 1);
    let issue = &issues[0];
    assert_eq!(issue.operation, "session-1-media");
    assert_eq!(issue.layer, LifecycleLayer::MediaTransport);
    assert_eq!(issue.phase, "open");
    assert_eq!(issue.code, "remote_disabled");
    assert_eq!(issue.disposition, IssueDisposition::CriticalFailure);
    assert!(issue.is_origin());
    assert!(
        issue.site.contains("media-transport/src/controller.rs:"),
        "{}",
        issue.site
    );
}

#[test]
fn a_delivery_failure_is_reported_once_at_the_line_that_raised_it() {
    let (video, audio) = receivers();
    let mut open = request(&video, &audio, false);
    open.video_port = None;
    open.audio_port = None;
    open.ping_timeout_ms = 0;
    open.discovery = Some(Box::new(Discovery {
        video_ping_payload: "video_token-1234".into(),
        audio_ping_payload: "audio_token-1234".into(),
    }));
    open.operation_id = Some("session-2-media".into());
    let mut controller = controller();
    controller.enable_stream_issue_reports();
    let stream = controller
        .handle("owner", Operation::Open(open))
        .unwrap()
        .created
        .unwrap();
    controller.tick();
    controller.tick();
    controller.close("owner", stream).unwrap();

    let issues = published(&mut controller);
    assert_eq!(issues.len(), 1, "{issues:?}");
    let issue = &issues[0];
    assert_eq!(issue.operation, "session-2-media");
    assert_eq!(issue.phase, "deliver");
    assert_eq!(issue.code, "udp_discovery_timeout");
    assert_eq!(issue.reason, "awaiting_video_and_audio_ping_timeout_ms");
    assert_eq!(
        issue.requested,
        Some(amaterasu_module_sdk::stream_warnings::WarningValue::Integer(0))
    );
    let record = serde_json::to_string(&issue).unwrap();
    assert!(!record.contains("video_token-1234"));
    assert!(!record.contains("audio_token-1234"));
    assert!(
        issue.site.contains("media-transport/src/datagrams.rs:"),
        "{}",
        issue.site
    );
}

#[test]
fn an_unsupported_batch_size_is_reported_as_demoted() {
    let (video, audio) = receivers();
    let mut open = request(&video, &audio, false);
    open.video.max_batch_size_kb = 20;
    open.operation_id = Some("session-3-media".into());
    let mut controller = controller();
    controller.enable_stream_issue_reports();
    controller.handle("owner", Operation::Open(open)).unwrap();

    let issues = published(&mut controller);
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].code, "video_batch_size_unsupported");
    assert_eq!(issues[0].disposition, IssueDisposition::DemotedWarning);
    assert_eq!(
        issues[0].fallback.as_deref(),
        Some("maximum_video_batch_size")
    );
}

#[test]
fn nothing_is_published_until_reports_are_enabled() {
    let (video, audio) = receivers();
    let mut open = request(&video, &audio, false);
    open.operation_id = Some("session-4-media".into());
    open.peer_ip = "192.0.2.1".parse().unwrap();
    let mut controller = controller();
    assert!(controller.handle("owner", Operation::Open(open)).is_err());
    assert!(published(&mut controller).is_empty());
    controller.enable_stream_issue_reports();
    assert!(published(&mut controller).is_empty(), "nothing was kept");
}
