use super::*;
use amaterasu_module_sdk::abi::{self, AbiHeader, BorrowedBytes, HostApi, ReplySink};

unsafe extern "C" fn unavailable_runtime(
    _: *mut std::ffi::c_void,
    _: BorrowedBytes,
    _: ReplySink,
) -> abi::Status {
    abi::STATUS_INTERNAL_ERROR
}

#[test]
fn managed_quiesce_waits_for_sdk_owned_stream_retirement() {
    let mut host = Harness::new();
    let id = host.response(1, prepare())["session"].as_u64().unwrap();
    assert_eq!(host.response(2, start(id))["state"], "active");
    let api = HostApi {
        header: AbiHeader::of::<HostApi>(),
        context: std::ptr::null_mut(),
        invoke: unavailable_runtime,
    };
    // SAFETY: the stateless endpoint is valid for the module's lifetime. This
    // test calls only quiescence; unload receives explicit transport failures.
    let runtime = unsafe { RuntimeClient::from_endpoint(api) };
    let mut module = crate::StreamSession {
        runtime,
        pipeline: host.pipeline,
    };
    assert!(!amaterasu_module_sdk::ManagedModule::quiesce(&mut module).unwrap());
}

fn drain(host: &mut Harness) -> bool {
    for _ in 0..600 {
        if host.pipeline.quiesce(host.now) {
            return true;
        }
        host.tick();
    }
    false
}

#[test]
fn cooperative_shutdown_drains_sdk_owned_active_and_prepared_streams() {
    let mut host = Harness::new();
    let active = host.response(1, prepare())["session"].as_u64().unwrap();
    assert_eq!(host.response(2, start(active))["state"], "active");
    let mut next = prepare();
    next["operation_id"] = json!("second-sdk-stream");
    let prepared = host.response(3, next)["session"].as_u64().unwrap();
    assert!(!host.pipeline.quiesce(host.now));
    assert!(drain(&mut host));
    assert_eq!(host.broker.live_components(), 0);
    assert_eq!(host.broker.leases(), 0);
    assert!(host.pipeline.records[&active].state.terminal());
    assert!(host.pipeline.records[&prepared].state.terminal());
    assert!(
        host.broker.state_value.as_ref().unwrap()["entries"]
            .as_object()
            .unwrap()
            .is_empty()
    );
    assert_eq!(host.response(4, prepare())["code"], "host_stopping");
    assert_eq!(
        host.response(5, json!({"action":"status","session":active}))["state"],
        "stopped"
    );
    assert_eq!(
        host.response(6, json!({"action":"stop","session":active}))["state"],
        "stopped"
    );
}

#[test]
fn cooperative_shutdown_settles_inflight_sdk_preparation_without_new_native_start() {
    let mut host = Harness::new();
    host.broker.enqueue(1, "owner", prepare());
    for _ in 0..100 {
        host.tick();
        if host.pipeline.job.as_ref().is_some_and(|job| {
            job.step == Step::Open(Resource::Media)
                && job.pending.as_ref().is_some_and(rpc::Pending::was_admitted)
        }) {
            break;
        }
    }
    assert!(
        host.pipeline
            .job
            .as_ref()
            .is_some_and(|job| job.step == Step::Open(Resource::Media))
    );
    assert!(!host.pipeline.quiesce(host.now));
    assert!(drain(&mut host));
    assert_eq!(host.broker.live_components(), 0);
    assert_eq!(host.broker.leases(), 0);
    assert!(
        !host
            .broker
            .calls
            .iter()
            .any(|call| call.service == Resource::Video.service()
                && call.payload["action"] == "open")
    );
    assert!(
        host.broker
            .commands
            .iter()
            .any(|command| matches!(command, Command::Cancel { .. }))
    );
}

#[test]
fn cooperative_shutdown_does_not_cancel_an_ownership_journal_write() {
    let mut host = Harness::new();
    host.broker.enqueue(1, "owner", prepare());
    for _ in 0..10 {
        host.tick();
        if host.pipeline.job.as_ref().is_some_and(|job| {
            job.step == Step::PersistJournal
                && job.pending.as_ref().is_some_and(rpc::Pending::was_admitted)
        }) {
            break;
        }
    }
    assert!(
        host.pipeline
            .job
            .as_ref()
            .is_some_and(|job| job.step == Step::PersistJournal)
    );
    assert!(!host.pipeline.quiesce(host.now));
    assert!(drain(&mut host));
    assert!(!host.pipeline.journal.failed);
    assert_eq!(host.broker.live_components(), 0);
    assert!(
        host.broker.state_value.as_ref().unwrap()["entries"]
            .as_object()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn cooperative_shutdown_retains_cleanup_failure_and_fixed_deadline() {
    let mut host = Harness::new();
    let id = host.response(1, prepare())["session"].as_u64().unwrap();
    assert_eq!(host.response(2, start(id))["state"], "active");
    host.broker.fail_close = Some(Resource::AudioCapture.service());
    assert_eq!(
        host.response(3, json!({"action":"stop","session":id}))["state"],
        "cleanup_failed"
    );
    let close_attempts = host
        .broker
        .calls
        .iter()
        .filter(|call| call.payload["action"] == "close")
        .count();
    assert!(!host.pipeline.quiesce(host.now));
    let deadline = host.pipeline.shutdown_deadline;
    for _ in 0..20 {
        host.tick();
        assert!(!host.pipeline.quiesce(host.now));
        assert_eq!(host.pipeline.shutdown_deadline, deadline);
    }
    assert_eq!(
        host.broker
            .calls
            .iter()
            .filter(|call| call.payload["action"] == "close")
            .count(),
        close_attempts
    );
    assert_eq!(host.pipeline.records[&id].state, State::CleanupFailed);
}

#[test]
fn cooperative_shutdown_preserves_active_native_drain_during_background_work() {
    let mut host = Harness::new();
    let id = host.response(1, prepare())["session"].as_u64().unwrap();
    assert_eq!(host.response(2, start(id))["state"], "active");
    host.pipeline.records.get_mut(&id).unwrap().force_idr = true;
    for _ in 0..20 {
        host.tick();
        if host
            .pipeline
            .job
            .as_ref()
            .is_some_and(|job| job.step == Step::ForceIdr)
        {
            break;
        }
    }
    assert!(
        host.pipeline
            .job
            .as_ref()
            .is_some_and(|job| job.step == Step::ForceIdr)
    );
    host.broker.fail_cancel = true;
    assert!(!host.pipeline.quiesce(host.now));
    assert!(
        host.pipeline.records[&id].plane.is_some(),
        "a healthy active stream must retain its native drain"
    );
    assert!(drain(&mut host));
    let drain = host
        .broker
        .calls
        .iter()
        .position(|call| {
            call.service == Resource::Video.service() && call.payload["action"] == "drain"
        })
        .unwrap();
    let close = host
        .broker
        .calls
        .iter()
        .position(|call| {
            call.service == Resource::Video.service() && call.payload["action"] == "close"
        })
        .unwrap();
    assert!(drain < close);
    assert_eq!(host.broker.live_components(), 0);
    assert_eq!(host.broker.leases(), 0);
}

#[test]
fn shutdown_deadline_retains_an_unconfirmed_ownership_write_without_cancelling_it() {
    let mut host = Harness::new();
    host.broker.enqueue(1, "owner", prepare());
    for _ in 0..10 {
        host.tick();
        if host.pipeline.job.as_ref().is_some_and(|job| {
            job.step == Step::PersistJournal
                && job.pending.as_ref().is_some_and(rpc::Pending::was_admitted)
        }) {
            break;
        }
    }
    let id = host.pipeline.job.as_ref().unwrap().session;
    assert!(!host.pipeline.quiesce(host.now));
    host.now = host.pipeline.shutdown_deadline.unwrap();
    host.tick();
    assert!(!host.pipeline.quiesce(host.now));
    assert_eq!(host.pipeline.records[&id].state, State::CleanupFailed);
    assert!(host.pipeline.records[&id].journal_uncertain.is_some());
    assert!(
        !host
            .broker
            .commands
            .iter()
            .any(|command| matches!(command, Command::Cancel { .. })),
        "expiry must retain the exact write instead of cancelling ownership persistence"
    );
    assert_eq!(host.broker.live_components(), 0);
}

#[test]
fn journal_write_timeout_is_fenced_even_without_shutdown() {
    let mut host = Harness::new();
    host.broker.enqueue(1, "owner", prepare());
    for _ in 0..10 {
        host.tick();
        if host.pipeline.job.as_ref().is_some_and(|job| {
            job.step == Step::PersistJournal
                && job.pending.as_ref().is_some_and(rpc::Pending::was_admitted)
        }) {
            break;
        }
    }
    let job = host.pipeline.job.as_ref().unwrap();
    assert_eq!(job.step, Step::PersistJournal);
    let id = job.session;
    host.now = job.deadline;
    host.tick();
    assert_eq!(host.pipeline.records[&id].state, State::CleanupFailed);
    assert!(host.pipeline.records[&id].journal_uncertain.is_some());
    assert!(
        !host
            .broker
            .commands
            .iter()
            .any(|command| matches!(command, Command::Cancel { .. }))
    );
}

#[test]
fn final_shutdown_never_releases_an_unvalidated_journal_write() {
    let mut host = Harness::new();
    host.broker.enqueue(1, "owner", prepare());
    for _ in 0..10 {
        host.tick();
        if host.pipeline.job.as_ref().is_some_and(|job| {
            job.step == Step::PersistJournal
                && job.pending.as_ref().is_some_and(rpc::Pending::was_admitted)
        }) {
            break;
        }
    }
    let job = host.pipeline.job.as_ref().unwrap();
    let id = job.session;
    let request = job.pending.as_ref().unwrap().request_id().unwrap();
    assert!(host.pipeline.retained_journal_requests().contains(&request));
    let before = host.broker.commands.len();
    assert!(
        !host
            .pipeline
            .shutdown_with(&mut |command| host.broker.send(command))
    );
    assert!(!host.broker.commands[before..].iter().any(|command| matches!(command,
        Command::TakeResult { request: current } | Command::Cancel { request: current } | Command::ReleaseResult { request: current } if *current == request)));
    host.now = host.pipeline.job.as_ref().unwrap().deadline;
    host.tick();
    assert!(host.pipeline.records[&id].journal_uncertain.is_some());
    let before = host.broker.commands.len();
    assert!(
        !host
            .pipeline
            .shutdown_with(&mut |command| host.broker.send(command))
    );
    assert!(!host.broker.commands[before..].iter().any(|command| matches!(command,
        Command::TakeResult { request: current } | Command::Cancel { request: current } | Command::ReleaseResult { request: current } if *current == request)));
    assert!(host.pipeline.retained_journal_requests().contains(&request));
}

#[test]
fn journal_release_ack_loss_keeps_validated_result_and_finishes() {
    let mut host = Harness::new();
    host.broker.enqueue(1, "owner", prepare());
    for _ in 0..10 {
        host.tick();
        if host.pipeline.job.as_ref().is_some_and(|job| {
            job.step == Step::PersistJournal
                && job.pending.as_ref().is_some_and(rpc::Pending::was_admitted)
        }) {
            break;
        }
    }
    let request = host
        .pipeline
        .job
        .as_ref()
        .unwrap()
        .pending
        .as_ref()
        .unwrap()
        .request_id()
        .unwrap();
    assert!(host.pipeline.retained_journal_requests().contains(&request));
    host.now += 1;
    host.broker.now = host.now;
    let mut lost = false;
    host.pipeline
        .tick_with(
            host.now,
            &mut |command| {
                let is_release =
                    matches!(command, Command::ReleaseResult { request: id } if id == request);
                let response = host.broker.send(command);
                if is_release && !lost {
                    lost = true;
                    Err("release acknowledgement lost".into())
                } else {
                    response
                }
            },
            &mut |_, _| panic!("journal settlement must not open a plane"),
        )
        .unwrap();
    assert!(lost);
    assert!(host.pipeline.job.as_ref().unwrap().journal_committed);
    assert!(!host.pipeline.journal.failed);
    for _ in 0..100 {
        host.tick();
        if host.broker.replies.contains_key(&1) {
            break;
        }
    }
    assert_eq!(host.broker.replies[&1]["state"], "prepared");
    assert!(!host.pipeline.journal.failed);
}

#[test]
fn late_confirmed_journal_write_runs_cleanup_before_reopening_admission() {
    let mut host = Harness::new();
    host.broker.enqueue(1, "owner", prepare());
    for _ in 0..10 {
        host.tick();
        if host.pipeline.job.as_ref().is_some_and(|job| {
            job.step == Step::PersistJournal
                && job.pending.as_ref().is_some_and(rpc::Pending::was_admitted)
        }) {
            break;
        }
    }
    let job = host.pipeline.job.as_ref().unwrap();
    let id = job.session;
    host.now = job.deadline;
    host.tick();
    assert!(host.pipeline.records[&id].journal_uncertain.is_some());
    assert!(!host.pipeline.startup_reconciled());
    host.now += 100;
    host.tick();
    assert!(host.pipeline.records[&id].journal_uncertain.is_none());
    assert!(!host.pipeline.startup_reconciled());
    for _ in 0..200 {
        host.tick();
        if host.pipeline.startup_reconciled() {
            break;
        }
    }
    assert!(host.pipeline.startup_reconciled());
    assert!(host.pipeline.journal.document.entries.is_empty());
    assert_eq!(host.pipeline.records[&id].state, State::Failed);
    assert_eq!(host.broker.live_components(), 0);
}

#[test]
fn unsent_expired_journal_step_does_not_create_an_uncertain_write() {
    let mut host = Harness::new();
    host.broker.enqueue(1, "owner", prepare());
    for _ in 0..10 {
        host.tick();
        if host
            .pipeline
            .job
            .as_ref()
            .is_some_and(|job| job.step == Step::PersistJournal)
        {
            break;
        }
    }
    let job = host.pipeline.job.as_mut().unwrap();
    let id = job.session;
    // Model the distinct state before any broker dispatch, such as a cleanup
    // workflow which just moved to PersistJournal on the previous tick.
    job.pending = None;
    job.cleanup = true;
    host.now = job.deadline;
    host.tick();
    assert!(host.pipeline.records[&id].journal_uncertain.is_none());
    assert!(!host.pipeline.journal.failed);
    assert!(!host.pipeline.recovery_blocked);
}

#[test]
fn mismatched_late_journal_revision_keeps_recovery_fenced() {
    let mut host = Harness::new();
    host.broker.enqueue(1, "owner", prepare());
    for _ in 0..10 {
        host.tick();
        if host.pipeline.job.as_ref().is_some_and(|job| {
            job.step == Step::PersistJournal
                && job.pending.as_ref().is_some_and(rpc::Pending::was_admitted)
        }) {
            break;
        }
    }
    let job = host.pipeline.job.as_ref().unwrap();
    let id = job.session;
    host.now = job.deadline;
    host.tick();
    host.pipeline
        .progress_uncertain_journal(host.now + 100, &mut |command| match command {
            Command::TakeResult { .. } => Ok(Response::Result {
                payload: json!({"ok": true, "revision": 999}),
            }),
            _ => panic!("must not release or dispatch after a mismatched revision"),
        });
    assert!(host.pipeline.records[&id].journal_uncertain.is_some());
    assert!(host.pipeline.journal.failed);
    assert!(!host.pipeline.startup_reconciled());
}

#[test]
fn validated_late_journal_result_retries_release_without_recommitting() {
    let mut host = Harness::new();
    host.broker.enqueue(1, "owner", prepare());
    for _ in 0..10 {
        host.tick();
        if host.pipeline.job.as_ref().is_some_and(|job| {
            job.step == Step::PersistJournal
                && job.pending.as_ref().is_some_and(rpc::Pending::was_admitted)
        }) {
            break;
        }
    }
    let job = host.pipeline.job.as_ref().unwrap();
    let id = job.session;
    host.now = job.deadline;
    host.tick();
    let revision = host.pipeline.journal.revision + 1;
    host.pipeline
        .progress_uncertain_journal(host.now + 100, &mut |command| match command {
            Command::TakeResult { .. } => Ok(Response::Result {
                payload: json!({"ok": true, "revision": revision}),
            }),
            Command::ReleaseResult { .. } => Err("lost release".into()),
            _ => panic!("unexpected command"),
        });
    assert!(host.pipeline.records[&id].journal_uncertain.is_some());
    assert!(host.pipeline.journal.failed);
    assert_eq!(host.pipeline.journal.revision, revision);
    host.pipeline
        .progress_uncertain_journal(host.now + 1100, &mut |command| match command {
            Command::ReleaseResult { .. } => Ok(Response::Ok),
            _ => panic!("completed result must not be fetched again"),
        });
    assert!(host.pipeline.records[&id].journal_uncertain.is_none());
    assert_eq!(host.pipeline.journal.revision, revision);
}

#[test]
fn late_journal_completion_does_not_clear_another_recovery_failure() {
    let mut host = Harness::new();
    host.broker.enqueue(1, "owner", prepare());
    for _ in 0..10 {
        host.tick();
        if host.pipeline.job.as_ref().is_some_and(|job| {
            job.step == Step::PersistJournal
                && job.pending.as_ref().is_some_and(rpc::Pending::was_admitted)
        }) {
            break;
        }
    }
    let job = host.pipeline.job.as_ref().unwrap();
    let id = job.session;
    host.now = job.deadline;
    host.tick();
    let mut other = Record::new(
        999,
        "owner".into(),
        host.pipeline.records[&id].config.clone(),
        1000,
    );
    other.state = State::CleanupFailed;
    host.pipeline.records.insert(other.id, other);
    host.pipeline.recovery_ids.insert(999);
    host.now += 100;
    host.tick();
    assert!(host.pipeline.records[&id].journal_uncertain.is_none());
    assert!(host.pipeline.recovery_blocked);
    assert!(!host.pipeline.startup_reconciled());
    for _ in 0..200 {
        host.tick();
        if host.pipeline.job.is_none() {
            break;
        }
    }
    assert_eq!(host.pipeline.records[&999].state, State::CleanupFailed);
    assert!(host.pipeline.recovery_blocked);
    assert!(!host.pipeline.journal.failed);
}

#[test]
fn late_journal_completion_does_not_retire_a_new_unpersisted_target() {
    let (_, _, configuration) = super::armed::prepared_lifecycle_for_cleanup(false);
    let mut host = Harness::new();
    host.pipeline
        .settings
        .allowed_principals
        .push("amaterasu.rtsp".into());
    let mut request = prepare();
    request["lifecycle_session_target"] =
        json!({"kind":"session","host_epoch":"epoch","session_id":"session","generation":1});
    host.broker.enqueue(1, "amaterasu.rtsp", request);
    for _ in 0..10 {
        host.tick();
        if host.pipeline.job.as_ref().is_some_and(|job| {
            job.step == Step::PersistJournal
                && job.pending.as_ref().is_some_and(rpc::Pending::was_admitted)
        }) {
            break;
        }
    }
    let job = host.pipeline.job.as_ref().unwrap();
    let id = job.session;
    host.now = job.deadline;
    host.tick();
    host.pipeline.records.get_mut(&id).unwrap().start =
        Some(serde_json::from_value(configuration).unwrap());
    host.pipeline.records.get_mut(&id).unwrap().uncertain = Some(rpc::Pending::new(
        "test",
        json!({}),
        "prior-provider".into(),
        None,
    ));
    host.pipeline.records.get_mut(&id).unwrap().uncertain_step = Some(Step::Close(Resource::Video));
    host.now += 100;
    host.tick();
    assert!(host.pipeline.records[&id].journal_uncertain.is_none());
    let job = host.pipeline.job.as_ref().expect("automatic cleanup job");
    assert_eq!(job.step, Step::PersistJournal);
    assert!(job.journal_document.is_some());
    assert!(host.pipeline.records[&id].uncertain.is_some());
    assert!(
        !host
            .pipeline
            .journal
            .matches_lifecycle_identity(&host.pipeline.records[&id])
    );
    assert!(!host.pipeline.journal.failed);
    assert!(!host.pipeline.startup_reconciled());
    for _ in 0..10 {
        host.tick();
        if host
            .pipeline
            .journal
            .matches_lifecycle_identity(&host.pipeline.records[&id])
        {
            break;
        }
        assert!(host.pipeline.records[&id].uncertain.is_some());
    }
    assert!(
        host.pipeline
            .journal
            .matches_lifecycle_identity(&host.pipeline.records[&id])
    );
    assert!(host.pipeline.job.as_ref().unwrap().pending.is_some());
}

#[test]
fn late_journal_completion_retries_another_stop_waiting_on_the_fence() {
    let mut host = Harness::new();
    host.broker.enqueue(1, "owner", prepare());
    for _ in 0..10 {
        host.tick();
        if host.pipeline.job.as_ref().is_some_and(|job| {
            job.step == Step::PersistJournal
                && job.pending.as_ref().is_some_and(rpc::Pending::was_admitted)
        }) {
            break;
        }
    }
    let job = host.pipeline.job.as_ref().unwrap();
    let id = job.session;
    host.now = job.deadline;
    host.tick();
    let mut other = Record::new(
        999,
        "owner".into(),
        host.pipeline.records[&id].config.clone(),
        1000,
    );
    other.state = State::CleanupFailed;
    other.cleanup_error = Some("lifecycle_journal_write_unconfirmed");
    host.pipeline.records.insert(other.id, other);
    host.pipeline.journal_waiting_stops.insert(999);
    host.now += 100;
    host.tick();
    assert!(host.pipeline.recovery_ids.contains(&999));
    for _ in 0..200 {
        host.tick();
        if host.pipeline.startup_reconciled() {
            break;
        }
    }
    assert!(host.pipeline.startup_reconciled());
    assert_ne!(host.pipeline.records[&999].state, State::CleanupFailed);
}

#[test]
fn deferred_journal_stop_keeps_uncertain_provider_request_owned() {
    let mut host = Harness::new();
    host.broker.enqueue(1, "owner", prepare());
    for _ in 0..10 {
        host.tick();
        if host.pipeline.job.as_ref().is_some_and(|job| {
            job.step == Step::PersistJournal
                && job.pending.as_ref().is_some_and(rpc::Pending::was_admitted)
        }) {
            break;
        }
    }
    let job = host.pipeline.job.as_ref().unwrap();
    let id = job.session;
    host.now = job.deadline;
    host.tick();
    let mut other = Record::new(
        999,
        "owner".into(),
        host.pipeline.records[&id].config.clone(),
        1000,
    );
    other.state = State::CleanupFailed;
    other.cleanup_error = Some("lifecycle_journal_write_unconfirmed");
    other.uncertain = Some(rpc::Pending::new("test", json!({}), "owned".into(), None));
    other.uncertain_step = Some(Step::Close(Resource::Video));
    host.pipeline.records.insert(other.id, other);
    host.pipeline.journal_waiting_stops.insert(999);
    host.now += 100;
    host.tick();
    assert!(host.pipeline.records[&999].uncertain.is_some());
    for _ in 0..200 {
        host.tick();
        if host
            .pipeline
            .job
            .as_ref()
            .is_some_and(|job| job.session == 999)
        {
            break;
        }
    }
    let job = host.pipeline.job.as_ref().expect("deferred stop runs");
    assert_eq!(job.session, 999);
    assert!(job.pending.is_some() || host.pipeline.records[&999].uncertain.is_some());
}

#[test]
fn unresolved_journal_admission_retains_the_original_operation() {
    let mut host = Harness::new();
    host.broker.enqueue(1, "owner", prepare());
    for _ in 0..10 {
        host.tick();
        if host
            .pipeline
            .job
            .as_ref()
            .is_some_and(|job| job.step == Step::PersistJournal && job.pending.is_some())
        {
            break;
        }
    }
    let job = host.pipeline.job.as_mut().unwrap();
    assert_eq!(job.step, Step::PersistJournal);
    let id = job.session;
    let mut pending = job.pending.take().unwrap();
    for _ in 0..3 {
        assert!(matches!(
            pending.poll(&mut |_| Err("lost admission".into()), host.now),
            rpc::Progress::Waiting
        ));
    }
    job.pending = Some(pending);
    host.tick();
    assert_eq!(host.pipeline.records[&id].state, State::CleanupFailed);
    assert!(host.pipeline.records[&id].journal_uncertain.is_some());
    assert!(host.pipeline.recovery_blocked);
}

#[test]
fn transient_journal_result_failure_does_not_cancel_the_write() {
    let mut host = Harness::new();
    host.broker.fault = Some(("amaterasu.state/1".into(), Fault::BeforeMutation));
    let response = host.response(1, prepare());
    assert_eq!(response["state"], "prepared");
    assert!(!host.pipeline.journal.failed);
    assert!(host.broker.fault.is_none());
    assert!(
        !host
            .broker
            .commands
            .iter()
            .any(|command| matches!(command, Command::Cancel { .. }))
    );
}

#[test]
fn expiring_adoption_journal_preserves_earlier_uncertain_provider_operation() {
    let (mut host, id, configuration) = super::armed::prepared_lifecycle_for_cleanup(false);
    let mut original = rpc::Pending::new(
        &Resource::Media.service(),
        json!({"action":"status","stream":host.pipeline.records[&id].resources[&Resource::Media]}),
        "original-provider-call".into(),
        None,
    );
    assert!(matches!(
        original.poll(&mut |command| host.broker.send(command), host.now),
        rpc::Progress::Waiting
    ));
    let record = host.pipeline.records.get_mut(&id).unwrap();
    record.uncertain = Some(original);
    record.uncertain_step = Some(Step::Open(Resource::Media));
    record.state = State::CleanupFailed;
    host.broker.enqueue(
        2,
        "amaterasu.rtsp",
        json!({"action":"stop","session":id,"lifecycle_configuration":configuration}),
    );
    for _ in 0..20 {
        host.tick();
        if host.pipeline.job.as_ref().is_some_and(|job| {
            job.step == Step::PersistJournal
                && job.pending.as_ref().is_some_and(rpc::Pending::was_admitted)
        }) {
            break;
        }
    }
    assert!(!host.pipeline.quiesce(host.now));
    host.now = host.pipeline.shutdown_deadline.unwrap();
    host.tick();
    assert_eq!(
        host.pipeline.records[&id].uncertain_step,
        Some(Step::Open(Resource::Media)),
        "journal expiry must not replace the earlier provider identity"
    );
    assert!(host.pipeline.records[&id].journal_uncertain.is_some());
}

#[test]
fn shutdown_announcement_closes_admission_without_starting_provider_cleanup() {
    let mut host = Harness::new();
    let id = host.response(1, prepare())["session"].as_u64().unwrap();
    assert_eq!(host.response(2, start(id))["state"], "active");
    let now = host.now;
    host.pipeline.prepare_shutdown(now, 100);
    host.pipeline.prepare_shutdown(now + 10, 1000);
    assert_eq!(host.pipeline.shutdown_announced_deadline, Some(now + 100));
    assert!(host.pipeline.shutdown_deadline.is_none());
    assert!(host.pipeline.job.is_none());
    assert_eq!(host.pipeline.records[&id].state, State::Active);
    assert!(!host.plane.borrow().stopping);
    assert_eq!(host.response(3, prepare())["code"], "host_stopping");
    assert_eq!(host.pipeline.records[&id].state, State::Active);
    assert!(!host.plane.borrow().stopping);
    assert!(!host.pipeline.quiesce(host.now));
    assert_eq!(host.pipeline.shutdown_deadline, Some(now + 100));
}

#[test]
fn stalled_readonly_active_observation_does_not_hold_native_resources_during_shutdown() {
    let mut host = Harness::new();
    let id = host.response(1, prepare())["session"].as_u64().unwrap();
    assert_eq!(host.response(2, start(id))["state"], "active");
    host.pipeline.job = Some(Job::new(id, None, Step::ObserveActiveBinding, host.now + 5));
    host.broker.fail_cancel = true;
    assert!(!host.pipeline.quiesce(host.now));
    let mut held = None;
    for _ in 0..300 {
        host.now += 1;
        host.broker.now = host.now;
        host.pipeline.tick_with(host.now, &mut |command| {
            if matches!(command, Command::TakeResult { request } if Some(request) == held) {
                return Ok(Response::Pending);
            }
            let observation = matches!(&command, Command::Request { service, payload, .. } if service == &Resource::Video.service() && payload["action"] == "observe_capture");
            let response = host.broker.send(command)?;
            if observation && let Response::Accepted { id } = response {
                held = Some(id);
            }
            Ok(response)
        }, &mut |_, _| panic!("shutdown cannot start a new native plane")).unwrap();
        host.pipeline.quiesce(host.now);
        if host.pipeline.records[&id].state.terminal() {
            break;
        }
    }
    assert_eq!(host.pipeline.records[&id].state, State::Stopped);
    assert!(host.pipeline.records[&id].uncertain.is_none());
    assert_eq!(host.pipeline.records[&id].shutdown_observations.len(), 1);
    assert_eq!(host.broker.live_components(), 0);
    assert_eq!(host.broker.leases(), 0);
    assert!(
        !host.pipeline.quiesce(host.now),
        "broker settlement remains owned even after native teardown"
    );
    host.broker.fail_cancel = false;
    assert!(drain(&mut host));
    assert!(host.pipeline.records[&id].shutdown_observations.is_empty());
}

#[test]
fn status_remains_responsive_after_an_ownership_journal_expires() {
    let mut host = Harness::new();
    host.broker.enqueue(1, "owner", prepare());
    for _ in 0..10 {
        host.tick();
        if host.pipeline.job.as_ref().is_some_and(|job| {
            job.step == Step::PersistJournal
                && job.pending.as_ref().is_some_and(rpc::Pending::was_admitted)
        }) {
            break;
        }
    }
    let id = host.pipeline.job.as_ref().unwrap().session;
    assert!(!host.pipeline.quiesce(host.now));
    host.now = host.pipeline.shutdown_deadline.unwrap();
    host.tick();
    host.broker
        .enqueue(2, "owner", json!({"action":"status","session":id}));
    for _ in 0..10 {
        host.tick();
    }
    assert_eq!(
        host.broker.replies.get(&2).map(|reply| &reply["state"]),
        Some(&json!("cleanup_failed"))
    );
}

#[test]
fn owner_stop_uses_announced_budget_before_provider_quiescence() {
    let mut host = Harness::new();
    let id = host.response(1, prepare())["session"].as_u64().unwrap();
    assert_eq!(host.response(2, start(id))["state"], "active");
    host.pipeline.prepare_shutdown(host.now, 2);
    host.now += 3;
    let stopped = host.response(3, json!({"action":"stop","session":id}));
    assert_eq!(
        stopped["state"], "cleanup_failed",
        "expired host budget must not become a fresh drain: {stopped}"
    );
    assert!(
        host.pipeline.shutdown_deadline.is_none(),
        "the provider DAG has not begun quiescence yet"
    );
    assert!(
        !host
            .broker
            .calls
            .iter()
            .any(|call| call.payload["action"] == "close")
    );
}
