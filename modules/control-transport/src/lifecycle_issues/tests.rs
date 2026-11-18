use crate::controller::Controller;
use crate::controller::tests::{configured, request};
use crate::model::Operation;
use amaterasu_module_sdk::stream_lifecycle::{
    IssueDisposition, LifecycleLayer, StreamIssue, StreamReport,
};
use amaterasu_module_sdk::{Command, Response};

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
    let mut controller = configured();
    controller.enable_stream_issue_reports();
    let mut open = request();
    open.operation_id = Some("session-1-control".into());
    open.peer_ip = "192.0.2.1".parse().unwrap();
    assert!(controller.handle("alice", Operation::Open(open)).is_err());
    // A caller this transport does not serve names no stream.
    assert!(
        controller
            .handle("mallory", Operation::Open(request()))
            .is_err()
    );

    let issues = published(&mut controller);
    assert_eq!(issues.len(), 1, "{issues:?}");
    let issue = &issues[0];
    assert_eq!(issue.operation, "session-1-control");
    assert_eq!(issue.layer, LifecycleLayer::ControlTransport);
    assert_eq!(issue.phase, "open");
    assert_eq!(issue.disposition, IssueDisposition::CriticalFailure);
    assert!(issue.is_origin());
    assert!(
        issue
            .site
            .replace('\\', "/")
            .contains("control-transport/src/controller.rs:"),
        "{}",
        issue.site
    );
}

#[test]
fn a_stream_failure_is_reported_once_at_the_line_that_raised_it() {
    let mut controller = configured();
    controller.enable_stream_issue_reports();
    let mut open = request();
    open.operation_id = Some("session-2-control".into());
    open.ping_timeout_ms = 0;
    let stream = controller
        .handle("alice", Operation::Open(open))
        .unwrap()
        .created
        .unwrap();
    controller.tick();
    controller.tick();
    controller.close("alice", stream).unwrap();

    let issues = published(&mut controller);
    assert_eq!(issues.len(), 1, "{issues:?}");
    let issue = &issues[0];
    assert_eq!(issue.operation, "session-2-control");
    assert_eq!(issue.phase, "control");
    assert_eq!(issue.code, "authentication_timeout");
    assert!(
        issue
            .site
            .replace('\\', "/")
            .contains("control-transport/src/stream.rs:"),
        "{}",
        issue.site
    );
}

#[test]
fn nothing_is_published_until_reports_are_enabled() {
    let mut controller = configured();
    let mut open = request();
    open.peer_ip = "192.0.2.1".parse().unwrap();
    assert!(controller.handle("alice", Operation::Open(open)).is_err());
    assert!(published(&mut controller).is_empty());
    controller.enable_stream_issue_reports();
    assert!(published(&mut controller).is_empty(), "nothing was kept");
}
