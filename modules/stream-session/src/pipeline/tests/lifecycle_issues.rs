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

#[test]
fn unavailable_optional_extension_is_a_warning_and_prepare_succeeds() {
    for scenario in ["source", "downstream", "unconfirmed", "deadline"] {
        let mut host = Harness::new();
        host.pipeline.settings.allowed_principals = vec!["amaterasu.rtsp".into()];
        let profile = json!({"codec":{"namespace_id":7,"value":8,"schema":1},
            "framing":{"namespace_id":7,"value":9,"schema":1},"wire_selector":4,
            "payload_type":100,"encoding_name":"EXAMPLE"});
        host.pipeline.settings.extension_route = Some(serde_json::from_value(json!({"profile":profile,
            "source_id":42,"encoder_provider":"fixture","encoder":{"clsid":"source","friendly_name":"Source","hardware_url":null},
            "video_unit_service":"example.units/1","framer_service":"example.framer/1","plan_transport_service":"example.plan/1"})).unwrap());
        host.broker.extension_probe_supported = scenario != "source";
        if scenario == "unconfirmed" {
            host.broker.fault = Some((
                "amaterasu.video/1:probe_source".into(),
                Fault::BeforeMutation,
            ));
            host.broker.fail_cancel = true;
        }
        host.extension_probe_limits = None;
        let mut request = prepare();
        request["stream_audio"] = json!(false);
        request["extension_probe"] = start(0)["configuration"]["configuration"].clone();
        host.broker.enqueue(1, "amaterasu.rtsp", request);
        for _ in 0..20 {
            host.tick();
            if !host.pipeline.records.is_empty() {
                break;
            }
        }
        let id = *host
            .pipeline
            .records
            .keys()
            .next()
            .expect("prepare record created");
        host.pipeline
            .records
            .get_mut(&id)
            .unwrap()
            .config
            .rtsp_operation_id = Some("launch-extension".into());
        host.pipeline.enable_stream_issue_reports();
        let mut forced_deadline = false;
        for _ in 0..100 {
            host.tick();
            if scenario == "deadline"
                && !forced_deadline
                && let Some(job) = host.pipeline.job.as_mut()
                && job.step == Step::ProbeExtensionSource
                && job.pending.is_some()
            {
                job.deadline = host.now + 1;
                forced_deadline = true;
            }
            if host.broker.replies.contains_key(&1) {
                break;
            }
        }
        assert!(scenario != "deadline" || forced_deadline);
        assert_eq!(host.broker.replies[&1]["state"], "prepared");
        host.tick();
        let issues = published(&host.broker.commands);
        assert!(
            issues
                .iter()
                .any(|issue| issue.code == "extension_source_unavailable"
                    && issue.disposition == IssueDisposition::DemotedWarning
                    && issue.fallback.as_deref() == Some("negotiate_standard_video")
                    && issue.operation == "launch-extension"
                    && issue.caused_by == Some(LifecycleLayer::EncodeVideo)),
            "{issues:?}"
        );
        assert!(!issues.iter().any(StreamIssue::is_critical), "{issues:?}");
        if scenario == "unconfirmed" {
            assert_eq!(host.pipeline.records[&id].shutdown_observations.len(), 1);
            host.broker.fail_cancel = false;
            host.idle();
            assert!(host.pipeline.records[&id].shutdown_observations.is_empty());
        }
    }
}
