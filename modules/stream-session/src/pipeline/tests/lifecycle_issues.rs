use super::*;
use amaterasu_module_sdk::stream_lifecycle::{
    IssueDisposition, LifecycleLayer, StreamIssue, StreamReport,
};

fn record_for(rtsp_operation: Option<&str>) -> Record {
    let mut payload = prepare();
    payload.as_object_mut().unwrap().remove("action");
    payload["rtsp_operation_id"] = json!(rtsp_operation);
    let config = serde_json::from_value(payload).unwrap();
    Record::new(42, "owner".into(), config, 30_000)
}

fn published(commands: &[Command]) -> Vec<StreamIssue> {
    commands
        .iter()
        .filter_map(|command| match command {
            Command::Publish { topic, payload } if topic == "amaterasu.stream.report/1" => {
                match serde_json::from_value(payload.clone()).unwrap() {
                    StreamReport::Issue(issue) => Some(issue),
                    StreamReport::Binding(_) => None,
                }
            }
            _ => None,
        })
        .collect()
}

#[test]
fn a_session_failure_is_published_under_the_launch_owners_stream_key() {
    let mut pipeline = Harness::new().pipeline;
    pipeline.enable_stream_issue_reports();
    let mut record = record_for(Some("launch-7"));
    let mut job = Job::new(42, None, Step::Open(Resource::Video), 0);
    job.fail(&mut record, "dependency_rejected", 0);
    pipeline.records.insert(42, record);

    let mut commands = Vec::new();
    pipeline.publish_stream_issues(&mut |command| {
        commands.push(command);
        Ok(Response::Ok)
    });
    let issues = published(&commands);
    assert_eq!(issues.len(), 1);
    let issue = &issues[0];
    assert_eq!(issue.operation, "launch-7");
    assert_eq!(issue.layer, LifecycleLayer::StreamSession);
    assert_eq!(issue.caused_by, Some(LifecycleLayer::EncodeVideo));
    assert_eq!(issue.phase, "open_video");
    assert_eq!(issue.code, "dependency_rejected");
    assert_eq!(issue.disposition, IssueDisposition::CriticalFailure);
    assert!(
        issue.site.contains("lifecycle_issues.rs:"),
        "{}",
        issue.site
    );
}

#[test]
fn reports_are_off_until_enabled_and_need_a_stream_key() {
    let mut pipeline = Harness::new().pipeline;
    let mut keyed = record_for(Some("launch-8"));
    Job::new(42, None, Step::WaitDrain, 0).fail(&mut keyed, "operation_deadline", 0);
    pipeline.records.insert(42, keyed);
    let mut commands = Vec::new();
    pipeline.publish_stream_issues(&mut |command| {
        commands.push(command);
        Ok(Response::Ok)
    });
    assert!(
        published(&commands).is_empty(),
        "a test pipeline sends nothing"
    );
    assert!(
        pipeline.records[&42].issues.is_empty(),
        "nothing is kept either"
    );

    let mut unkeyed = record_for(None);
    Job::new(43, None, Step::WaitDrain, 0).fail(&mut unkeyed, "operation_deadline", 0);
    assert!(unkeyed.issues.is_empty());
}
