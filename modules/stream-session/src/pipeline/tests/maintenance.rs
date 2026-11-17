use super::*;
use amaterasu_module_sdk::stream_lifecycle::{IssueDisposition, StreamReport};

fn advance(
    pipeline: &mut Pipeline,
    record: &mut Record,
    job: &mut Job,
    now: u64,
    send: &mut Send<'_>,
) -> bool {
    pipeline.advance(
        record,
        job,
        now,
        send,
        &mut NativeOperations {
            factory: &mut |_, _, _| panic!("maintenance must not allocate a native plane"),
            probe: &mut |_, _, _| panic!("maintenance must not negotiate a new media format"),
        },
    )
}

fn active() -> (Harness, u64) {
    let mut host = Harness::new();
    let id = host.response(1, prepare())["session"].as_u64().unwrap();
    assert_eq!(host.response(2, start(id))["state"], "active");
    host.pipeline
        .records
        .get_mut(&id)
        .unwrap()
        .config
        .rtsp_operation_id = Some("launch-maintenance".into());
    host.pipeline.job = None;
    (host, id)
}

#[test]
fn optional_maintenance_failures_never_enter_native_cleanup() {
    for step in [
        Step::ForceIdr,
        Step::FeedbackPeek,
        Step::FeedbackAck,
        Step::PollGamepadFeedback,
    ] {
        for scenario in [
            "refused",
            "malformed",
            "admission",
            "deadline",
            "lost_admission",
            "lost_result",
        ] {
            let (mut host, id) = active();
            let mut record = host.pipeline.records.remove(&id).unwrap();
            let resources = record.resources.clone();
            record.force_idr = true;
            record.feedback_ack_through = Some(7);
            record.rumble_active_until.insert(0, 99_999);
            let mut job = Job::new(id, None, step, 30);
            let mut send = |command| match command {
                Command::Request { .. } if scenario == "admission" => {
                    Ok(Response::error("unavailable", "missing optional service"))
                }
                Command::Request { .. } if scenario == "lost_admission" => Err("lost".into()),
                Command::Request { .. } => Ok(Response::Accepted { id: 91 }),
                Command::TakeResult { .. } if scenario == "lost_result" => Err("lost".into()),
                Command::TakeResult { .. } => Ok(Response::Result {
                    payload: if scenario == "refused" {
                        json!({"ok":false,"error":{"code":"unsupported"}})
                    } else {
                        if step == Step::ForceIdr {
                            json!({"ok":null})
                        } else {
                            json!({"ok":true})
                        }
                    },
                }),
                Command::Cancel { .. } | Command::ReleaseResult { .. }
                    if scenario == "lost_result" =>
                {
                    Err("lost".into())
                }
                _ => Ok(Response::Ok),
            };
            let mut finished = false;
            for tick in 1..=10 {
                let now = if scenario == "deadline" { 30 } else { tick };
                if !advance(&mut host.pipeline, &mut record, &mut job, now, &mut send) {
                    finished = true;
                    break;
                }
            }
            assert!(finished, "{step:?}/{scenario}");
            assert_eq!(record.state, State::Active, "{step:?}/{scenario}");
            assert_eq!(record.error, None);
            assert_eq!(record.cleanup_error, None);
            assert!(!job.cleanup);
            assert!(record.plane.is_some());
            assert_eq!(record.resources, resources);
            assert_eq!(
                record.rumble_active_until[&0], 99_999,
                "rumble retirement fence changed"
            );
            assert_eq!(host.plane.borrow().media_ticks_when_stopped, None);
            let reports: Vec<_> = record.issues.drain().collect();
            assert!(
                reports
                    .iter()
                    .any(|report| matches!(report, StreamReport::Issue(issue)
                if issue.disposition == IssueDisposition::DemotedWarning && issue.valid()
                    && !issue.reason.is_empty() && issue.fallback.is_some())),
                "{step:?}/{scenario}: {reports:?}"
            );
        }
    }
}

#[test]
fn uncertain_optional_commands_are_bounded_and_settle_independently() {
    let (mut host, id) = active();
    let mut record = host.pipeline.records.remove(&id).unwrap();
    let mut operations = Vec::new();
    for _ in 0..12 {
        let mut job = Job::new(id, None, Step::ForceIdr, 100);
        for now in 1..=10 {
            if !advance(
                &mut host.pipeline,
                &mut record,
                &mut job,
                now,
                &mut |command| {
                    if let Command::Request { operation_id, .. } = command {
                        operations.push(operation_id);
                    }
                    Err("lost admission".into())
                },
            ) {
                break;
            }
        }
    }
    assert_eq!(
        host.pipeline.maintenance_retirements.len(),
        super::super::maintenance::MAX_RETIREMENTS
    );
    assert_eq!(record.state, State::Active);
    // Admission recovery preserves the original operation IDs; there is no
    // unbounded retry queue and no replay after the broker retention horizon.
    let original: std::collections::BTreeSet<_> = operations.iter().cloned().collect();
    for now in [20, 21, 22, 23] {
        host.pipeline
            .progress_maintenance_retirements(now, &mut |command| {
                if let Command::Request { operation_id, .. } = command {
                    assert!(original.contains(&operation_id));
                }
                Err("still unavailable".into())
            });
    }
    host.pipeline
        .progress_maintenance_retirements(400_000, &mut |_| {
            panic!("expired admission must not replay")
        });
    host.pipeline
        .progress_maintenance_retirements(400_001, &mut |_| {
            panic!("expired admission must not replay")
        });
    assert!(host.pipeline.maintenance_retirements.is_empty());
    host.pipeline.records.insert(id, record);
    let result = host.response(3, json!({"action":"stop","session":id}));
    assert_eq!(result["state"], "stopped");
}

#[test]
fn explicit_recovery_and_required_lease_failures_remain_critical() {
    for step in [Step::ForceIdr, Step::PollGamepadFeedback] {
        let (mut host, id) = active();
        let mut record = host.pipeline.records.remove(&id).unwrap();
        let mut job = Job::new(id, None, step, 30);
        let mut send = |command| match command {
            Command::Request { .. } => Ok(Response::Accepted { id: 92 }),
            Command::TakeResult { .. } => Ok(Response::Result {
                payload: json!({"ok":false,"recovery_required":true}),
            }),
            _ => Ok(Response::Ok),
        };
        assert!(advance(
            &mut host.pipeline,
            &mut record,
            &mut job,
            1,
            &mut send
        ));
        assert!(advance(
            &mut host.pipeline,
            &mut record,
            &mut job,
            2,
            &mut send
        ));
        assert!(advance(
            &mut host.pipeline,
            &mut record,
            &mut job,
            3,
            &mut send
        ));
        assert_eq!(record.state, State::Stopping);
        assert!(job.cleanup);
        assert!(record.issues.drain().any(|report| matches!(report,
            StreamReport::Issue(issue) if issue.is_critical())));
    }
    let (mut host, id) = active();
    let record = host.pipeline.records.get_mut(&id).unwrap();
    let mut job = Job::new(id, None, Step::RenewInput, 30);
    job.fail(record, StreamFailure::critical("input_lease_expired"), 1);
    assert_eq!(record.state, State::Stopping);
    assert!(job.cleanup);
}

#[test]
fn maintenance_deadline_retains_active_video() {
    let (mut host, id) = active();
    let mut record = host.pipeline.records.remove(&id).unwrap();
    let mut job = Job::new(id, None, Step::ForceIdr, 0);
    assert!(!advance(
        &mut host.pipeline,
        &mut record,
        &mut job,
        1,
        &mut |_| Ok(Response::Ok)
    ));
    assert_eq!(record.state, State::Active);
    assert!(record.plane.is_some());
    assert!(!job.cleanup);
}

#[test]
fn noncritical_reports_cannot_drive_workflow_or_direct_record_failure_effects() {
    use super::super::record::FailureEffect;
    use amaterasu_module_sdk::stream_lifecycle::LifecycleLayer;
    for failure in [
        StreamFailure::advisory("advisory_hint"),
        StreamFailure::demoted("optional_provider_unavailable", "continue_video"),
    ] {
        let (mut host, id) = active();
        let mut record = host.pipeline.records.remove(&id).unwrap();
        let resources = record.resources.clone();
        let mut job = Job::new(id, None, Step::Open(Resource::Video), 30);
        job.fail(&mut record, failure.clone(), 1);
        assert!(!advance(
            &mut host.pipeline,
            &mut record,
            &mut job,
            2,
            &mut |_| panic!("warning submitted a provider effect")
        ));
        assert!(!job.cleanup);
        for effect in [
            FailureEffect::Stop,
            FailureEffect::CancelUnconfirmed,
            FailureEffect::ReleaseBindings,
            FailureEffect::CleanupUnconfirmed,
        ] {
            assert!(!record.raise_failure(
                failure.clone(),
                LifecycleLayer::Capture,
                "open",
                effect
            ));
            assert_eq!(record.state, State::Active);
            assert_eq!(record.resources, resources);
            assert!(record.plane.is_some());
            assert_eq!(record.error, None);
            assert_eq!(record.cleanup_error, None);
        }
        assert_eq!(host.plane.borrow().media_ticks_when_stopped, None);
    }
}
