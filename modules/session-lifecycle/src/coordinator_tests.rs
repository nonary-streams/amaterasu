use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct TestDirectory(std::path::PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "amaterasu-lifecycle-coordinator-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        Self(path)
    }

    fn journal(&self) -> std::path::PathBuf {
        self.0.join("journal.json")
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn admission_is_durable_idempotent_and_recovered_without_replaying_work() {
    let dir = TestDirectory::new();
    let mut coordinator = Coordinator::open(&dir.journal()).unwrap();
    let request = json!({"operation":"prepare", "operation_id":"op-1"});
    let first = coordinator.handle("transport", "client", request.clone());
    assert_eq!(first["status"], "pending");
    assert_eq!(
        coordinator.handle("transport", "client", request.clone()),
        first
    );
    let old_epoch = first["host_epoch"].clone();
    drop(coordinator);

    let mut coordinator = Coordinator::open(&dir.journal()).unwrap();
    let recovered = coordinator.handle("transport", "client", request);
    assert_eq!(recovered["status"], "recovery_required");
    assert_eq!(recovered["reason"], "coordinator_restarted");
    assert_eq!(recovered["session_id"], first["session_id"]);
    assert_eq!(recovered["transition_id"], first["transition_id"]);
    assert_ne!(
        coordinator.handle("transport", "client", json!({"operation":"snapshot"}))["host_epoch"],
        old_epoch
    );
}

#[test]
fn lookup_and_replay_are_bound_to_broker_caller_initiator_and_request() {
    let dir = TestDirectory::new();
    let mut coordinator = Coordinator::open(&dir.journal()).unwrap();
    let request = json!({"operation":"prepare", "operation_id":"op-1"});
    let receipt = coordinator.handle("transport", "client", request.clone());
    let lookup = json!({"operation":"lookup", "operation_id":"op-1"});
    assert_eq!(
        coordinator.handle("transport", "client", lookup.clone()),
        receipt
    );
    assert_eq!(
        coordinator.handle("other", "client", lookup.clone())["status"],
        "not_found"
    );
    assert_eq!(
        coordinator.handle("transport", "other", lookup)["status"],
        "not_found"
    );
    assert_eq!(
        coordinator.handle("other", "client", request.clone())["status"],
        "pending"
    );
    assert_eq!(
        coordinator.handle("transport", "other", request)["status"],
        "pending"
    );
    assert_eq!(
        coordinator.handle(
            "transport",
            "client",
            json!({"operation":"prepare", "operation_id":"op-1", "extra":true})
        )["reason"],
        "operation_id_conflict"
    );
}

#[test]
fn failed_journal_replacement_never_exposes_a_pending_receipt() {
    let dir = TestDirectory::new();
    let path = dir.journal();
    let mut coordinator = Coordinator::open(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    let request = json!({"operation":"prepare", "operation_id":"op-1"});
    let failed = coordinator.handle("transport", "client", request.clone());
    assert_eq!(failed["status"], "recovery_required");
    assert_eq!(failed["reason"], "journal_write_failed");
    assert_eq!(
        coordinator.handle(
            "transport",
            "client",
            json!({"operation":"lookup", "operation_id":"op-1"})
        )["status"],
        "not_found"
    );
    std::fs::remove_dir(&path).unwrap();
    assert_eq!(
        coordinator.handle("transport", "client", request)["status"],
        "recovery_required"
    );
}

#[test]
fn semantically_corrupt_journal_is_rejected_before_recovery() {
    let dir = TestDirectory::new();
    let path = dir.journal();
    let mut coordinator = Coordinator::open(&path).unwrap();
    let request = json!({"operation":"prepare", "operation_id":"op-1"});
    assert_eq!(
        coordinator.handle("transport", "client", request)["status"],
        "pending"
    );
    drop(coordinator);
    let mut journal: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    journal["receipts"][receipt_key("transport", "client", "op-1")]["response"]["operation_id"] =
        json!("different-operation");
    std::fs::write(&path, serde_json::to_vec(&journal).unwrap()).unwrap();
    assert!(
        matches!(Coordinator::open(&path), Err(error) if error.kind() == io::ErrorKind::InvalidData)
    );
}

#[test]
fn retirement_waits_do_not_starve_later_preparation() {
    let dir = TestDirectory::new();
    let mut coordinator = Coordinator::open(&dir.journal()).unwrap();
    coordinator.configured = true;
    for index in 0..5 {
        coordinator.handle(
            "transport",
            "client",
            json!({"operation":"prepare","operation_id":format!("op-{index}")}),
        );
    }
    let mut next = coordinator.shared.borrow().value.clone();
    for index in 0..4 {
        let receipt = next
            .receipts
            .get_mut(&receipt_key("transport", "client", &format!("op-{index}")))
            .unwrap();
        let work = receipt.work.as_mut().unwrap();
        work.phase = LifecyclePhase::SessionRelease;
        work.waiting_retirement = true;
    }
    save(&coordinator.shared, next).unwrap();
    for _ in 0..2 {
        coordinator
            .advance_work(&mut |_| panic!("no providers configured"))
            .unwrap();
    }
    assert_eq!(
        coordinator.handle(
            "transport",
            "client",
            json!({"operation":"lookup","operation_id":"op-4"})
        )["status"],
        "succeeded"
    );
}

#[test]
fn duplicate_end_does_not_allocate_another_cleanup_transition() {
    let dir = TestDirectory::new();
    let mut coordinator = Coordinator::open(&dir.journal()).unwrap();
    coordinator.configured = true;
    let prepared = coordinator.handle(
        "transport",
        "client",
        json!({"operation":"prepare","operation_id":"prepare"}),
    );
    coordinator.advance_work(&mut |_| unreachable!()).unwrap();
    assert_eq!(
        coordinator.handle(
            "transport",
            "client",
            json!({"operation":"end_session","operation_id":"end-1","target":prepared["target"]})
        )["status"],
        "pending"
    );
    assert_eq!(
        coordinator.handle(
            "transport",
            "client",
            json!({"operation":"end_session","operation_id":"end-2","target":prepared["target"]})
        )["status"],
        "rejected"
    );
    assert_eq!(
        coordinator
            .shared
            .borrow()
            .value
            .receipts
            .values()
            .filter(|r| r
                .work
                .as_ref()
                .is_some_and(|w| w.phase == LifecyclePhase::SessionRelease))
            .count(),
        1
    );
}

#[test]
fn missing_candidate_rolls_back_instead_of_failing_the_module_tick() {
    let dir = TestDirectory::new();
    let mut coordinator = Coordinator::open(&dir.journal()).unwrap();
    coordinator.configured = true;
    let prepared = coordinator.handle(
        "transport",
        "client",
        json!({"operation":"prepare","operation_id":"prepare"}),
    );
    coordinator.advance_work(&mut |_| unreachable!()).unwrap();
    let stream = coordinator.handle("transport", "client", json!({"operation":"prepare_stream","operation_id":"stream","target":prepared["target"],"requested_video":{"width":1920,"height":1080,"refresh_millihz":60000,"hdr":false},"consumer_requirements":["capture"]}));
    assert_eq!(stream["status"], "pending");
    coordinator.advance_work(&mut |_| unreachable!()).unwrap();
    coordinator.advance_work(&mut |_| unreachable!()).unwrap();
    let failed = coordinator.handle(
        "transport",
        "client",
        json!({"operation":"lookup","operation_id":"stream"}),
    );
    assert_eq!(failed["status"], "failed");
    assert_eq!(failed["reason"], "capture_candidate_missing");
    assert_eq!(failed["cleanup_completed"], true);
}

#[test]
fn capture_only_stream_retirement_does_not_add_an_input_requirement() {
    let dir = TestDirectory::new();
    let mut coordinator = Coordinator::open(&dir.journal()).unwrap();
    coordinator.configured = true;
    let prepared = coordinator.handle(
        "transport",
        "client",
        json!({"operation":"prepare","operation_id":"prepare"}),
    );
    coordinator.advance_work(&mut |_| unreachable!()).unwrap();
    let stream = coordinator.handle("transport", "client", json!({"operation":"prepare_stream","operation_id":"stream","target":prepared["target"],"requested_video":{"width":1920,"height":1080,"refresh_millihz":60000,"hdr":false},"consumer_requirements":["capture"]}));
    assert_eq!(
        coordinator.handle(
            "transport",
            "client",
            json!({"operation":"pause_stream","operation_id":"pause","target":stream["target"]})
        )["fenced"],
        true
    );
    let next = &coordinator.shared.borrow().value;
    let (_, id, stream_id, _) =
        operations::target_parts(&serde_json::from_value(stream["target"].clone()).unwrap())
            .map(|(e, s, t, g)| (e.to_owned(), s.to_owned(), t.map(str::to_owned), g))
            .unwrap();
    assert!(
        next.sessions[&id].streams[stream_id.as_ref().unwrap()]
            .retirements
            .is_empty()
    );
}

#[test]
fn failed_launch_requires_the_frozen_operation_and_explicit_absence_fence() {
    let dir = TestDirectory::new();
    let mut coordinator = Coordinator::open(&dir.journal()).unwrap();
    coordinator.configured = true;
    let prepared = coordinator.handle("transport", "client", json!({"operation":"prepare","operation_id":"prepare","application_ref":{"kind":"application","app_id":"game"},"application_operation_id":"ale1:epoch:launch"}));
    coordinator.advance_work(&mut |_| unreachable!()).unwrap();
    let bind = coordinator.handle("transport", "client", json!({"operation":"bind_application","operation_id":"bind","target":prepared["target"],"launch_id":1,"application_operation_id":"different"}));
    assert_eq!(bind["reason"], "application_operation_mismatch");
    coordinator.handle(
        "transport",
        "client",
        json!({"operation":"end_session","operation_id":"end","target":prepared["target"]}),
    );
    for (attempt, confirmed) in [("unconfirmed", false), ("confirmed", true)] {
        assert_eq!(coordinator.handle("transport", "client", json!({"operation":"application_retired","operation_id":attempt,"target":prepared["target"]}))["status"], "pending");
        let mut send = |command| {
            Ok(match command {
                Command::Request { payload, .. } => {
                    assert_eq!(payload["owner"], "transport");
                    assert_eq!(payload["operation_id"], "ale1:epoch:launch");
                    assert!(payload["launch_id"].is_null());
                    Response::Accepted { id: 1 }
                }
                Command::TakeResult { .. } => Response::Result {
                    payload: json!({"ok":true,"retired":true,"owner":"transport","operation_id":"ale1:epoch:launch","launch_id":null,"never_admitted":confirmed}),
                },
                Command::ReleaseResult { .. } => Response::Ok,
                _ => panic!("unexpected broker operation"),
            })
        };
        for _ in 0..3 {
            coordinator.advance_applications(&mut send).unwrap();
        }
        let status = coordinator.handle(
            "transport",
            "client",
            json!({"operation":"lookup","operation_id":attempt}),
        );
        assert_eq!(
            status["status"],
            if confirmed {
                "succeeded"
            } else {
                "recovery_required"
            }
        );
    }
    coordinator.advance_work(&mut |_| unreachable!()).unwrap();
    assert_eq!(
        coordinator.handle(
            "transport",
            "client",
            json!({"operation":"lookup","operation_id":"end"})
        )["status"],
        "succeeded"
    );
}

#[test]
fn transferred_action_fences_new_preparation_across_sessions_until_terminal() {
    let dir = TestDirectory::new();
    let mut coordinator = Coordinator::open(&dir.journal()).unwrap();
    coordinator.configured = true;
    let first = coordinator.handle(
        "transport",
        "client",
        json!({"operation":"prepare","operation_id":"first"}),
    );
    assert_eq!(first["status"], "pending");
    let key = receipt_key("transport", "client", "first");
    let mut next = coordinator.shared.borrow().value.clone();
    let work = next.receipts.get_mut(&key).unwrap().work.as_mut().unwrap();
    work.action_transfer = Some(ActionTransfer {
        service: "action-service".into(),
        permit: "a".repeat(32),
        binding: crate::action_authority::ActionBinding {
            operation_id: "action".into(),
            target: crate::action_authority::ActionTarget::Session {
                host_epoch: first["host_epoch"].as_str().unwrap().into(),
                session_id: first["session_id"].as_str().unwrap().into(),
                expected_generation: 1,
            },
            action_id: "display/remove".into(),
            plan_digest: "b".repeat(64),
            source: crate::action_authority::Principal {
                caller: "transport".into(),
                initiator: "client".into(),
            },
        },
        input_reservation_id: None,
        admission_id: None,
    });
    save(&coordinator.shared, next).unwrap();
    let rejected = coordinator.handle(
        "other",
        "other-client",
        json!({"operation":"prepare","operation_id":"second"}),
    );
    assert_eq!(rejected["reason"], "action_transition_in_progress");
    let mut next = coordinator.shared.borrow().value.clone();
    next.receipts
        .get_mut(&key)
        .unwrap()
        .work
        .as_mut()
        .unwrap()
        .action_transfer = None;
    save(&coordinator.shared, next).unwrap();
    assert_eq!(
        coordinator.handle(
            "other",
            "other-client",
            json!({"operation":"prepare","operation_id":"second"})
        )["status"],
        "pending"
    );
}
