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
fn rejected_prepare_reports_warning_only_for_an_authorized_stream_owner() {
    for authorized in [false, true] {
        let mut host = Harness::new();
        if authorized {
            host.pipeline
                .settings
                .lifecycle_admitters
                .insert("owner".into());
        }
        let mut payload = prepare();
        payload["rtsp_operation_id"] = json!("launch-7");
        let operation = serde_json::from_value(payload).unwrap();
        host.broker.enqueue(12, "owner", json!({}));
        host.pipeline.receive(
            12,
            super::super::actions::RequestCaller {
                principal: "owner".into(),
                approved_api: true,
            },
            operation,
            0,
            10_000,
            &mut |command| host.broker.send(command),
        );
        let reports: Vec<_> = host.pipeline.admission_issues.drain().collect();
        if authorized {
            assert!(matches!(&reports[..], [StreamReport::Issue(issue)]
                if issue.operation == "launch-7"
                    && issue.code == "invalid_history_activity"
                    && issue.reason == issue.code
                    && issue.disposition == IssueDisposition::AdvisoryWarning
                    && issue.valid()));
        } else {
            assert!(reports.is_empty());
        }
    }
}

#[test]
fn rejected_start_reports_warning_without_stopping_the_session() {
    let mut host = Harness::new();
    let id = host.response(1, prepare())["session"].as_u64().unwrap();
    host.pipeline
        .records
        .get_mut(&id)
        .unwrap()
        .config
        .rtsp_operation_id = Some("launch-7".into());
    let mut payload = start(id);
    payload["configuration"]["lifecycle_target"] = json!({"kind":"stream", "host_epoch":"epoch", "session_id":"session", "stream_id":"stream", "generation":1});
    let operation = serde_json::from_value(payload).unwrap();
    host.broker.enqueue(2, "owner", json!({}));
    host.pipeline.receive(
        2,
        super::super::actions::RequestCaller {
            principal: "owner".into(),
            approved_api: true,
        },
        operation,
        0,
        10_000,
        &mut |command| host.broker.send(command),
    );
    let record = host.pipeline.records.get_mut(&id).unwrap();
    assert_eq!(record.state, State::Prepared);
    let reports: Vec<_> = record.issues.drain().collect();
    assert!(
        reports
            .iter()
            .any(|report| matches!(report, StreamReport::Issue(issue)
        if issue.disposition == IssueDisposition::AdvisoryWarning
            && !issue.reason.is_empty() && issue.valid()))
    );
}

#[test]
fn repeated_refused_input_batches_produce_one_warning_and_keep_the_session_active() {
    let mut record = record_for(Some("launch-input"));
    record.state = State::Active;
    for _ in 0..2 {
        record.input_events.push_back(json!({"kind":"key","key":1}));
        record.input_inflight = 1;
        super::super::input::drop_refused_input_batch(
            &mut record,
            &json!({"ok":false,"error":{"code":"unsupported_event"}}),
        )
        .unwrap();
    }
    assert_eq!(record.state, State::Active);
    let reports: Vec<_> = record.issues.drain().collect();
    let issues: Vec<_> = reports
        .iter()
        .filter_map(|report| match report {
            StreamReport::Issue(issue) => Some(issue),
            _ => None,
        })
        .collect();
    assert!(
        matches!(&issues[..], [issue]
        if issue.code == "unsupported_event" && issue.reason == "unsupported_event"
            && issue.caused_by == Some(LifecycleLayer::Input)
            && issue.disposition == IssueDisposition::AdvisoryWarning && issue.valid()),
        "{reports:?}"
    );
    record.input_events.push_back(json!({"kind":"key","key":2}));
    record.input_inflight = 1;
    super::super::input::drop_refused_input_batch(
        &mut record,
        &json!({"ok":false,"error":{"code":"shortcut_unavailable"}}),
    )
    .unwrap();
    assert!(record.issues.drain().any(|report| matches!(report, StreamReport::Issue(issue)
        if issue.code == "shortcut_unavailable" && issue.disposition == IssueDisposition::AdvisoryWarning)));
}

#[test]
fn input_refusal_requiring_recovery_is_not_demoted() {
    let mut record = record_for(Some("launch-input"));
    record.state = State::Active;
    record.input_events.push_back(json!({"kind":"key","key":1}));
    record.input_inflight = 1;
    assert!(
        super::super::input::drop_refused_input_batch(
            &mut record,
            &json!({"ok":false,"error":{"code":"unsupported_event"},"recovery_required":true})
        )
        .is_err()
    );
    super::super::input::stop_for_input_failure(&mut record, "input_batch_failed");
    let reports: Vec<_> = record.issues.drain().collect();
    assert!(reports.iter().any(|report| matches!(report, StreamReport::Issue(issue)
        if issue.code == "input_batch_failed" && issue.disposition == IssueDisposition::CriticalFailure)));
    assert!(
        !reports
            .iter()
            .any(|report| matches!(report, StreamReport::Issue(issue)
        if issue.disposition == IssueDisposition::AdvisoryWarning))
    );
}

#[test]
fn rejected_audio_probe_fails_only_the_reconfiguration_request() {
    for scenario in ["unsupported", "admission", "conflict", "unconfirmed"] {
        let mut host = Harness::new();
        let mut request = prepare();
        request["audio"] = json!({"channels":2,"channel_mask":0,"layout":"objects","packet_ms":10,"bitrate":128000,
            "custom":{"codec":{"namespace":42,"value":99,"version":1},"sample_rate":48000,"source":"encoded",
                "stream":{"framing":{"namespace":42,"value":3,"version":1},"clock":{"namespace":1,"value":1,"version":1},
                    "layout":{"schema":{"namespace":42,"value":7,"version":1},"positions":[]},"generation":1,"encoder_delay_us":0},"options":{}}});
        let mut revised = request["audio"].clone();
        revised["custom"]["stream"]["generation"] = json!(2);
        let id = host.response(1, request)["session"].as_u64().unwrap();
        host.pipeline
            .records
            .get_mut(&id)
            .unwrap()
            .config
            .rtsp_operation_id = Some("launch-audio".into());
        host.pipeline.enable_stream_issue_reports();
        let service = Resource::Media.service();
        match scenario {
            "admission" => host.broker.reject_audio_probe_admission = Some(service),
            "conflict" => {
                host.broker.reject_audio_probe_admission = Some(service);
                host.broker.audio_probe_admission_conflict = true;
            }
            "unconfirmed" => {
                host.broker.fault = Some((format!("{service}:probe"), Fault::BeforeMutation));
                host.broker.fail_cancel = true;
            }
            _ => host.broker.unsupported_audio_probe = Some(service),
        }
        let response = host.response(
            2,
            json!({"action":"reconfigure_audio", "session":id, "audio":revised}),
        );
        let code = match scenario {
            "admission" => "audio_probe_unavailable",
            "conflict" => "audio_probe_unavailable",
            "unconfirmed" => "audio_probe_unconfirmed",
            _ => "unsupported_audio_format",
        };
        assert_eq!(response["code"], code);
        assert_eq!(host.pipeline.records[&id].state, State::Prepared);
        assert_eq!(
            host.pipeline.records[&id]
                .config
                .audio
                .custom
                .as_ref()
                .unwrap()
                .stream
                .generation,
            1
        );
        host.tick();
        let issues = published(&host.broker.commands);
        assert!(
            issues.iter().any(|issue| issue.code == code
                && issue.disposition == IssueDisposition::AdvisoryWarning),
            "{issues:?}"
        );
        assert!(!issues.iter().any(StreamIssue::is_critical), "{issues:?}");
        if matches!(scenario, "unconfirmed" | "conflict") {
            assert_eq!(host.pipeline.records[&id].shutdown_observations.len(), 1);
            host.broker.fail_cancel = false;
            host.broker.reject_audio_probe_admission = None;
            host.idle();
            assert!(host.pipeline.records[&id].shutdown_observations.is_empty());
        }
    }
}

#[test]
fn cancelling_audio_reconfiguration_keeps_the_prepared_session() {
    let mut host = Harness::new();
    let mut request = prepare();
    request["audio"] = json!({"channels":2,"channel_mask":0,"layout":"objects","packet_ms":10,"bitrate":128000,
        "custom":{"codec":{"namespace":42,"value":99,"version":1},"sample_rate":48000,"source":"encoded",
            "stream":{"framing":{"namespace":42,"value":3,"version":1},"clock":{"namespace":1,"value":1,"version":1},
                "layout":{"schema":{"namespace":42,"value":7,"version":1},"positions":[]},"generation":1,"encoder_delay_us":0},"options":{}}});
    let mut revised = request["audio"].clone();
    revised["custom"]["stream"]["generation"] = json!(2);
    let id = host.response(1, request)["session"].as_u64().unwrap();
    host.pipeline
        .records
        .get_mut(&id)
        .unwrap()
        .config
        .rtsp_operation_id = Some("launch-audio".into());
    host.pipeline.enable_stream_issue_reports();
    host.broker.enqueue(
        2,
        "owner",
        json!({"action":"reconfigure_audio", "session":id, "audio":revised}),
    );
    host.tick();
    assert!(
        host.pipeline
            .job
            .as_ref()
            .is_some_and(|job| matches!(job.step, Step::ProbeAudioReconfiguration(_)))
    );
    host.broker.cancel_parent(2);
    host.idle();
    assert_eq!(host.pipeline.records[&id].state, State::Prepared);
    assert_eq!(
        host.pipeline.records[&id]
            .config
            .audio
            .custom
            .as_ref()
            .unwrap()
            .stream
            .generation,
        1
    );
    assert!(
        !published(&host.broker.commands)
            .iter()
            .any(StreamIssue::is_critical)
    );
}

#[test]
fn expired_unsent_audio_probe_is_not_dispatched_during_settlement() {
    let mut host = Harness::new();
    let id = host.response(1, prepare())["session"].as_u64().unwrap();
    let mut job = Job::new(
        id,
        None,
        Step::ProbeAudioReconfiguration(Resource::Media),
        host.now,
    );
    job.pending = Some(rpc::Pending::new(
        "fixture/1",
        json!({"action":"probe"}),
        "unsent-probe".into(),
        None,
    ));
    host.pipeline.job = Some(job);
    host.tick();
    host.idle();
    assert_eq!(host.pipeline.records[&id].state, State::Prepared);
    assert!(host.pipeline.records[&id].shutdown_observations.is_empty());
    assert!(
        !host
            .broker
            .calls
            .iter()
            .any(|call| call.service == "fixture/1")
    );
}

#[test]
fn deferred_read_settlement_limit_refuses_new_probe_without_stopping_session() {
    let mut host = Harness::new();
    let first = host.response(1, prepare())["session"].as_u64().unwrap();
    let mut second_request = prepare();
    second_request["operation_id"] = json!("another-session");
    let id = host.response(2, second_request)["session"]
        .as_u64()
        .unwrap();
    let record = host.pipeline.records.get_mut(&first).unwrap();
    record.config.rtsp_operation_id = Some("launch-audio".into());
    for index in 0..MAX_DEFERRED_READ_SETTLEMENTS {
        record.shutdown_observations.push(rpc::Pending::new(
            "fixture/1",
            json!({}),
            format!("read-{index}"),
            None,
        ));
    }
    let operation = serde_json::from_value(json!({"action":"reconfigure_audio", "session":id,
        "audio":{"channels":2,"channel_mask":3,"layout":"stereo","packet_ms":10,"bitrate":128000}}))
    .unwrap();
    host.pipeline
        .records
        .get_mut(&id)
        .unwrap()
        .config
        .rtsp_operation_id = Some("launch-other".into());
    host.broker.enqueue(3, "owner", json!({}));
    host.pipeline.receive(
        3,
        super::super::actions::RequestCaller {
            principal: "owner".into(),
            approved_api: true,
        },
        operation,
        host.now,
        1000,
        &mut |command| host.broker.send(command),
    );
    assert_eq!(
        host.broker.replies[&3]["code"],
        "audio_probe_settlement_pending"
    );
    assert_eq!(
        host.pipeline.records[&first].shutdown_observations.len(),
        MAX_DEFERRED_READ_SETTLEMENTS
    );
    let record = host.pipeline.records.get_mut(&id).unwrap();
    assert_eq!(record.state, State::Prepared);
    assert!(record.issues.drain().any(|report| matches!(report, StreamReport::Issue(issue)
        if issue.code == "audio_probe_settlement_pending" && issue.disposition == IssueDisposition::AdvisoryWarning)));
}

#[test]
fn capacity_eviction_retains_the_owner_of_an_unsettled_read() {
    let mut pipeline = Harness::new().pipeline;
    for id in 1..=super::super::admission::MAX_RECORDS as u64 {
        let mut record = record_for(Some("launch-capacity"));
        record.id = id;
        record.state = State::Stopped;
        if id == 1 {
            record.shutdown_observations.push(rpc::Pending::new(
                "fixture/1",
                json!({}),
                "unsettled-read".into(),
                None,
            ));
        }
        pipeline.records.insert(id, record);
    }
    pipeline.keep_session_capacity();
    assert!(pipeline.records.contains_key(&1));
    assert!(pipeline.records.len() < super::super::admission::MAX_RECORDS);
}

#[test]
fn lost_read_only_probe_retires_at_the_admission_recovery_horizon() {
    let mut pipeline = Harness::new().pipeline;
    let mut record = record_for(Some("launch-probe"));
    record.state = State::Stopped;
    let mut pending = rpc::Pending::new(
        "fixture/1",
        json!({"action":"probe"}),
        "probe-1".into(),
        None,
    );
    assert!(matches!(
        pending.poll(&mut |_| Err("lost admission".into()), 10),
        rpc::Progress::Waiting
    ));
    pending.cancel();
    record.shutdown_observations.push(pending);
    pipeline.records.insert(record.id, record);
    pipeline.journal.loaded = true;
    pipeline.recovery_ready = true;
    assert!(!pipeline.package_replacement_ready());
    pipeline.progress_shutdown_observations(300_009, &mut |_| Err("still unavailable".into()));
    assert_eq!(pipeline.records[&42].shutdown_observations.len(), 1);
    pipeline.progress_shutdown_observations(300_010, &mut |_| {
        panic!("expired admission must not be replayed")
    });
    assert!(pipeline.records[&42].shutdown_observations.is_empty());
    assert!(pipeline.package_replacement_ready());
}

#[test]
fn unavailable_optional_extension_warns_and_standard_video_starts() {
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
        assert!(host.broker.replies[&1]["extension_profile"].is_null());
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
        let started = host.response_from(2, "amaterasu.rtsp", start(id));
        assert_eq!(started["state"], "active", "{scenario}: {started}");
        assert_eq!(host.plane.borrow().planned_planes, 0);
    }
}

#[test]
fn expired_preparation_reports_critical_issue_before_cleanup() {
    let mut host = Harness::new();
    let id = host.response(1, prepare())["session"].as_u64().unwrap();
    let record = host.pipeline.records.get_mut(&id).unwrap();
    record.config.rtsp_operation_id = Some("launch-7".into());
    let deadline = record.prepared_until;
    host.pipeline.pump(deadline);
    let record = host.pipeline.records.get_mut(&id).unwrap();
    assert_eq!(record.state, State::Stopping);
    assert!(
        record
            .issues
            .drain()
            .any(|report| matches!(report, StreamReport::Issue(issue)
        if issue.code == "prepare_expired"
            && issue.reason == "prepare_expired"
            && issue.disposition == IssueDisposition::CriticalFailure && issue.valid()))
    );
}

#[test]
fn audio_encoder_recovery_reports_the_actual_outcome() {
    for succeeds in [true, false] {
        let mut host = Harness::new();
        let id = host.response(1, prepare())["session"].as_u64().unwrap();
        assert_eq!(host.response(2, start(id))["state"], "active");
        host.pipeline
            .records
            .get_mut(&id)
            .unwrap()
            .config
            .rtsp_operation_id = Some("launch-audio".into());
        host.pipeline.enable_stream_issue_reports();
        host.broker.fail_audio_recover = !succeeds;
        host.plane.borrow_mut().failure = Some("audio_encoder_failed");
        host.idle();
        let issues = published(&host.broker.commands);
        if succeeds {
            assert!(
                issues
                    .iter()
                    .any(|issue| issue.code == "audio_encoder_failed"
                        && issue.disposition == IssueDisposition::DemotedWarning
                        && issue.caused_by == Some(LifecycleLayer::AudioEncode)
                        && issue.fallback.as_deref() == Some("reset_audio_encoder")),
                "{issues:?}"
            );
            assert_eq!(host.pipeline.records[&id].state, State::Active);
        } else {
            assert!(
                issues.iter().any(
                    |issue| issue.disposition == IssueDisposition::CriticalFailure
                        && issue.caused_by == Some(LifecycleLayer::AudioEncode)
                        && issue.reason == "audio_encoder_failed"
                ),
                "{issues:?}"
            );
            assert!(
                !issues
                    .iter()
                    .any(|issue| issue.code == "audio_encoder_failed"
                        && issue.disposition == IssueDisposition::DemotedWarning),
                "{issues:?}"
            );
        }
    }
}

#[test]
fn failed_cleanup_is_reported_and_demoted_only_after_confirmed_retry() {
    let mut host = Harness::new();
    let id = host.response(1, prepare())["session"].as_u64().unwrap();
    assert_eq!(host.response(2, start(id))["state"], "active");
    host.pipeline
        .records
        .get_mut(&id)
        .unwrap()
        .config
        .rtsp_operation_id = Some("launch-cleanup".into());
    host.pipeline.enable_stream_issue_reports();
    host.broker.fail_close = Some(Resource::AudioCapture.service());
    assert_eq!(
        host.response(3, json!({"action":"stop", "session":id}))["state"],
        "cleanup_failed"
    );
    host.tick();
    let before = published(&host.broker.commands);
    let failure = before
        .iter()
        .find(|issue| issue.disposition == IssueDisposition::CriticalFailure)
        .expect("cleanup failure reported")
        .code
        .clone();
    assert!(!before.iter().any(
        |issue| issue.code == failure && issue.disposition == IssueDisposition::DemotedWarning
    ));
    host.broker.fail_close = None;
    assert_eq!(
        host.response(4, json!({"action":"stop", "session":id}))["state"],
        "stopped"
    );
    host.tick();
    let after = published(&host.broker.commands);
    assert!(
        after.iter().any(|issue| issue.code == failure
            && issue.disposition == IssueDisposition::DemotedWarning
            && issue.fallback.as_deref() == Some("cleanup_confirmed")),
        "{after:?}"
    );
}

#[test]
fn cleanup_recovery_cannot_erase_an_earlier_start_failure_with_the_same_provider_code() {
    use amaterasu_module_sdk::stream_lifecycle::StreamLifecycleStore;
    let mut record = record_for(Some("launch-7"));
    record.report_failure(LifecycleLayer::Audio, "prepare", "dependency_rejected");
    let mut job = Job::new(42, None, Step::Close(Resource::AudioCapture), 100);
    job.cleanup = true;
    job.fail(&mut record, "dependency_rejected", 0);
    record.report_confirmed_cleanup();
    let mut store = StreamLifecycleStore::new();
    store.open("launch-7");
    for report in record.issues.drain() {
        if let StreamReport::Issue(issue) = report {
            store.commit("amaterasu.stream-session", issue);
        }
    }
    let root = store.stream("launch-7").unwrap().root_cause().unwrap();
    assert_eq!(root.issue.code, "dependency_rejected");
    assert_eq!(root.issue.phase, "prepare");
}

#[test]
fn rumble_delivery_failure_reports_before_stopping() {
    let mut host = Harness::new();
    let id = host.response(1, prepare())["session"].as_u64().unwrap();
    assert_eq!(host.response(2, start(id))["state"], "active");
    let record = host.pipeline.records.get_mut(&id).unwrap();
    record.config.rtsp_operation_id = Some("launch-7".into());
    record.rumble_pending.insert(
        0,
        (
            crate::plane::RumbleCommand {
                slot: 0,
                low_frequency: 10,
                high_frequency: 10,
            },
            None,
        ),
    );
    host.plane.borrow_mut().rumble_error = true;
    host.pipeline.pump(host.now);
    let record = host.pipeline.records.get_mut(&id).unwrap();
    assert_eq!(record.state, State::Stopping);
    assert!(
        record
            .issues
            .drain()
            .any(|report| matches!(report, StreamReport::Issue(issue)
        if issue.code == "rumble_delivery_failed"
            && issue.reason == issue.code
            && issue.disposition == IssueDisposition::CriticalFailure && issue.valid()))
    );
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
