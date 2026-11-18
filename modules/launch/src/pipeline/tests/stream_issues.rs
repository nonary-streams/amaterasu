use super::*;
use amaterasu_module_sdk::stream_lifecycle::{
    IssueDisposition, LifecycleLayer, StreamIssue, StreamReport,
};

/// Run `payload` from `alice` to completion, then publish what the launch
/// recorded. Returns the reply and every issue published on the topic.
fn request_and_publish(
    harness: &mut Harness,
    pipeline: &mut Pipeline,
    payload: Value,
) -> (Value, Vec<StreamIssue>) {
    let mut publications = Vec::new();
    harness.output = None;
    harness.incoming = Some(Response::Request {
        id: 7,
        caller: "alice".into(),
        initiator: "alice".into(),
        service: SERVICE.into(),
        payload,
        remaining_ms: 30_000,
    });
    let mut reply = None;
    for _ in 0..100 {
        pipeline
            .tick(|command| match command {
                Command::Publish { .. } => {
                    publications.push(command);
                    Ok(Response::Ok)
                }
                command => harness.send(command),
            })
            .unwrap();
        reply = harness.output.take();
        if reply.is_some() {
            break;
        }
    }
    let reply = reply.expect("launch workflow did not complete");
    pipeline.publish_stream_issues(&mut |command| {
        publications.push(command);
        Ok(Response::Ok)
    });
    (reply, published(&publications))
}

fn published(commands: &[Command]) -> Vec<StreamIssue> {
    commands
        .iter()
        .filter_map(|command| match command {
            Command::Publish { topic, payload } if topic == "amaterasu.stream.report/1" => {
                assert_eq!(payload["kind"], "issue", "{payload}");
                match serde_json::from_value(payload.clone()).unwrap() {
                    StreamReport::Issue(issue) => Some(issue),
                    StreamReport::Binding(_) => None,
                }
            }
            _ => None,
        })
        .collect()
}

fn assert_reported_here(issue: &StreamIssue, operation: &str) {
    assert_eq!(issue.operation, operation);
    assert_eq!(issue.layer, LifecycleLayer::Application);
    assert_eq!(issue.caused_by, None);
    assert!(
        issue
            .site
            .replace('\\', "/")
            .contains("modules/launch/src/"),
        "reported from this module: {}",
        issue.site
    );
    assert!(issue.valid(), "{issue:?}");
}

#[test]
fn a_failed_application_launch_is_published_under_the_launch_operation() {
    let mut harness = Harness::new();
    harness.preparation_terminal = Some("launch_failed");
    let mut pipeline = pipeline();
    pipeline.enable_stream_issue_reports();

    let (failed, issues) = request_and_publish(&mut harness, &mut pipeline, launch("stream-key-7"));
    assert_eq!(failed["launch"]["state"], "failed", "{failed}");
    assert_eq!(issues.len(), 1, "{issues:?}");
    let issue = &issues[0];
    assert_reported_here(issue, "stream-key-7");
    assert_eq!(issue.disposition, IssueDisposition::CriticalFailure);
    assert_eq!(issue.phase, "await_application");
    assert_eq!(issue.code, "application_launch_failed");
    assert!(issue.site.contains("pipeline.rs:"), "{}", issue.site);
}

#[test]
fn a_launch_warning_is_published_once_as_an_advisory() {
    let mut harness = Harness::new();
    harness.application_warning = Some("proton_hook_unconfirmed");
    let mut pipeline = pipeline();
    pipeline.enable_stream_issue_reports();

    let (launched, issues) =
        request_and_publish(&mut harness, &mut pipeline, launch("stream-key-8"));
    assert_eq!(launched["launch"]["state"], "active", "{launched}");
    assert_eq!(issues.len(), 1, "{issues:?}");
    let issue = &issues[0];
    assert_reported_here(issue, "stream-key-8");
    assert_eq!(issue.disposition, IssueDisposition::AdvisoryWarning);
    assert_eq!(issue.phase, "spawn");
    assert_eq!(issue.code, "proton_hook_unavailable");
    assert_eq!(issue.reason, "proton_hook_unconfirmed");

    // The monitor sees the same warning again; it is not reported twice.
    let launch_id = launched["launch"]["launch_id"].as_u64().unwrap();
    let (_, repeated) = request_and_publish(
        &mut harness,
        &mut pipeline,
        json!({"action":"warnings","launch_id":launch_id}),
    );
    assert!(repeated.is_empty(), "{repeated:?}");
}

#[test]
fn a_failed_cleanup_is_published_beside_the_launch_failure() {
    let mut harness = Harness::new();
    harness.preparation_terminal = Some("launch_failed");
    harness.fail_automatic_cleanup_once = true;
    let mut pipeline = pipeline();
    pipeline.enable_stream_issue_reports();

    let (blocked, issues) =
        request_and_publish(&mut harness, &mut pipeline, launch("stream-key-9"));
    assert_eq!(blocked["launch"]["state"], "cleanup_failed", "{blocked}");
    let codes: Vec<_> = issues.iter().map(|issue| issue.code.as_str()).collect();
    assert_eq!(
        codes,
        ["application_launch_failed", "application_cleanup_failed"]
    );
    for issue in &issues {
        assert_reported_here(issue, "stream-key-9");
        assert_eq!(issue.disposition, IssueDisposition::CriticalFailure);
    }
}

#[test]
fn a_refused_launch_export_is_published_under_the_requested_operation() {
    let mut harness = Harness::new();
    let mut pipeline = pipeline();
    pipeline.enable_stream_issue_reports();
    let mut payload = launch_export("stream-key-10");
    payload["preflight_token"] = json!("launch.unknown.1");

    let (refused, issues) = request_and_publish(&mut harness, &mut pipeline, payload);
    assert_eq!(refused["code"], "preflight_not_found", "{refused}");
    assert_eq!(issues.len(), 1, "{issues:?}");
    assert_reported_here(&issues[0], "stream-key-10");
    assert_eq!(issues[0].phase, "admit_launch");
    assert_eq!(issues[0].code, "preflight_not_found");
}

#[test]
fn reports_are_discarded_until_enabled() {
    let mut harness = Harness::new();
    harness.preparation_terminal = Some("launch_failed");
    harness.application_warning = Some("proton_hook_unconfirmed");
    let mut pipeline = pipeline();

    let (failed, issues) =
        request_and_publish(&mut harness, &mut pipeline, launch("stream-key-11"));
    assert_eq!(failed["launch"]["state"], "failed", "{failed}");
    assert!(issues.is_empty(), "a test pipeline publishes nothing");
    assert!(
        pipeline
            .records
            .values
            .values()
            .all(|record| record.issues.is_empty()),
        "nothing is kept either"
    );
    assert!(pipeline.refusals.is_empty());
}
