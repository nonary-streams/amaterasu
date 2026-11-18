use super::*;
use amaterasu_module_sdk::stream_lifecycle::{
    IssueDisposition, LifecycleLayer, StreamBinding, StreamIssue, StreamReport,
};

const LAUNCH_OWNER: &str = "amaterasu.nvhttp";

fn published(commands: &[Command]) -> Vec<StreamReport> {
    commands
        .iter()
        .filter_map(|command| match command {
            Command::Publish { topic, payload } if topic == "amaterasu.stream.report/1" => {
                assert_eq!(payload["kind"] != "issue", payload["kind"] == "binding");
                Some(serde_json::from_value(payload.clone()).unwrap())
            }
            _ => None,
        })
        .collect()
}

fn issues(reports: &[StreamReport]) -> Vec<&StreamIssue> {
    reports
        .iter()
        .filter_map(|report| match report {
            StreamReport::Issue(issue) => Some(issue),
            StreamReport::Binding(_) => None,
        })
        .collect()
}

fn publish(controller: &mut Controller) -> Vec<StreamReport> {
    let mut commands = Vec::new();
    controller.publish_stream_issues(&mut |command| {
        commands.push(command);
        Ok(Response::Ok)
    });
    published(&commands)
}

fn launch_owned_record(operation: &str) -> Record {
    let mut record = record(State::Playing);
    record.owner = LAUNCH_OWNER.into();
    record.config.operation_id = operation.into();
    record.failure_scope = amaterasu_module_sdk::stream_lifecycle::StreamFailureScope::new(
        record.config.operation_id.as_str(),
    );
    record
}

fn status_job(record: &Record) -> Job {
    Job {
        ticket: record.id,
        phase: Phase::Observe,
        call: Call::for_service(
            "amaterasu.stream-session/1",
            json!({"action":"status"}),
            "rtsp-test-status".into(),
            None,
        ),
        caller: None,
        network: None,
        deadline: 1_000,
    }
}

#[test]
fn an_open_refusal_is_reported_under_the_launch_owners_stream_key() {
    let mut controller = Controller::new(Settings {
        allowed_principals: vec![LAUNCH_OWNER.into()],
        ..Settings::default()
    })
    .unwrap();
    controller.enable_stream_issue_reports();
    let mut endpoint = Endpoint::default();
    let runtime = client(&mut endpoint);
    let mut config = open();
    config.operation_id = "launch-7".into();
    controller.open(
        OpenRequest {
            request: 1,
            caller: LAUNCH_OWNER.into(),
            config,
            deadline: 5_000,
            display_preflight_deadline: None,
        },
        0,
        &runtime,
        false,
    );
    assert_eq!(endpoint.completions[0].1["code"], "disabled");

    let reports = publish(&mut controller);
    let issues = issues(&reports);
    assert_eq!(issues.len(), 1);
    let issue = issues[0];
    assert_eq!(issue.operation, "launch-7");
    assert_eq!(issue.layer, LifecycleLayer::Rtsp);
    assert_eq!(issue.phase, "open");
    assert_eq!(issue.code, "disabled");
    assert_eq!(issue.disposition, IssueDisposition::CriticalFailure);
    assert!(issue.is_origin());
    assert!(
        issue
            .site
            .replace('\\', "/")
            .contains("rtsp/src/controller/actions.rs:"),
        "{}",
        issue.site
    );
}

#[test]
fn a_ticket_binds_its_operations_to_the_launch_owners_stream() {
    let settings: Settings = serde_json::from_value(json!({
        "enabled":true,
        "bind":"127.0.0.1:0",
        "allowed_principals":[LAUNCH_OWNER]
    }))
    .unwrap();
    let mut controller = Controller::new(settings).unwrap();
    controller.enable_stream_issue_reports();
    let mut endpoint = Endpoint {
        active_request: true,
        ..Endpoint::default()
    };
    let runtime = client(&mut endpoint);
    let mut config = open();
    config.operation_id = "launch-8".into();
    controller.open(
        OpenRequest {
            request: 2,
            caller: LAUNCH_OWNER.into(),
            config,
            deadline: 5_000,
            display_preflight_deadline: None,
        },
        0,
        &runtime,
        false,
    );
    let ticket_operation = controller.records.values().next().unwrap().operation();

    let reports = publish(&mut controller);
    assert_eq!(
        reports,
        vec![StreamReport::Binding(StreamBinding::new(
            "launch-8",
            ticket_operation
        ))]
    );
}

#[test]
fn a_stream_session_failure_is_reported_as_rtsps_reaction_once() {
    let mut controller = Controller::new(Settings::default()).unwrap();
    controller.enable_stream_issue_reports();
    let mut endpoint = Endpoint::default();
    let runtime = client(&mut endpoint);
    let mut record = launch_owned_record("launch-9");
    let job = status_job(&record);
    controller.status_result(
        &mut record,
        job,
        &json!({"ok":true,"session":23,"state":"failed"}),
        10,
        &runtime,
    );
    // A later reason does not replace the failure, and is not reported.
    record.close(Some(
        amaterasu_module_sdk::stream_lifecycle::StreamFailure::requirement_failed(
            "operation_deadline",
        ),
    ));
    controller.records.insert(record.id, record);

    let reports = publish(&mut controller);
    let issues = issues(&reports);
    assert_eq!(issues.len(), 1);
    let issue = issues[0];
    assert_eq!(issue.operation, "launch-9");
    assert_eq!(issue.layer, LifecycleLayer::Rtsp);
    assert_eq!(issue.phase, "play");
    assert_eq!(issue.code, "stream_failed");
    assert_eq!(issue.caused_by, Some(LifecycleLayer::StreamSession));
    assert!(
        issue
            .site
            .replace('\\', "/")
            .contains("rtsp/src/controller/workflow.rs:"),
        "{}",
        issue.site
    );
}

#[test]
fn an_unconfirmed_cleanup_is_reported_once_per_code() {
    let mut controller = Controller::new(Settings::default()).unwrap();
    controller.enable_stream_issue_reports();
    let mut record = launch_owned_record("launch-10");
    record.state = State::Closing;
    record.fail_cleanup(
        amaterasu_module_sdk::stream_lifecycle::StreamFailure::requirement_failed(
            "invalid_stop_response",
        ),
    );
    record.fail_cleanup(
        amaterasu_module_sdk::stream_lifecycle::StreamFailure::requirement_failed(
            "invalid_stop_response",
        ),
    );
    record.fail_cleanup(
        amaterasu_module_sdk::stream_lifecycle::StreamFailure::requirement_failed(
            "cleanup_deadline",
        ),
    );
    controller.records.insert(record.id, record);

    let reports = publish(&mut controller);
    let issues = issues(&reports);
    let codes: Vec<_> = issues
        .iter()
        .map(|issue| (issue.phase.as_str(), issue.code.as_str(), issue.caused_by))
        .collect();
    assert_eq!(
        codes,
        vec![
            (
                "cleanup",
                "invalid_stop_response",
                Some(LifecycleLayer::StreamSession)
            ),
            ("cleanup", "cleanup_deadline", None),
        ]
    );
}

#[test]
fn reports_are_off_until_enabled_and_only_cover_launch_owned_tickets() {
    let mut controller = Controller::new(Settings::default()).unwrap();
    let mut owned = launch_owned_record("launch-11");
    owned.close(Some(
        amaterasu_module_sdk::stream_lifecycle::StreamFailure::requirement_failed(
            "network_disconnected",
        ),
    ));
    controller.records.insert(owned.id, owned);
    assert!(publish(&mut controller).is_empty(), "nothing is sent");
    assert!(
        controller.records[&5].issues.is_empty(),
        "nothing is kept either"
    );

    controller.enable_stream_issue_reports();
    let mut foreign = record(State::Playing);
    foreign.close(Some(
        amaterasu_module_sdk::stream_lifecycle::StreamFailure::requirement_failed(
            "network_disconnected",
        ),
    ));
    controller.records.insert(foreign.id, foreign);
    assert!(publish(&mut controller).is_empty());
    assert!(controller.records[&5].issues.is_empty());
}

#[test]
fn a_cancelled_request_is_the_callers_decision_and_not_reported() {
    let mut controller = Controller::new(Settings::default()).unwrap();
    controller.enable_stream_issue_reports();
    let mut record = launch_owned_record("launch-12");
    record.state = State::Preparing;
    record.cancel();
    controller.records.insert(record.id, record);
    assert!(publish(&mut controller).is_empty());
}
