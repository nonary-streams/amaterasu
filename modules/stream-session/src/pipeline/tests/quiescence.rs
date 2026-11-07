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
