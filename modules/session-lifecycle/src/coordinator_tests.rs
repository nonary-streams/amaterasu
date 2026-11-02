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
        snapshot: None,
        stage: ActionTransferStage::CleanupPending,
        release_proof: None,
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

#[test]
fn frozen_action_lineage_survives_restart_and_rejects_tampering() {
    use crate::adapter::{MethodSettings, ParticipantSettings};
    use crate::prepare_authority::{Admission, MethodApproval, ProviderSelection};
    use sha2::{Digest, Sha256};

    let dir = TestDirectory::new();
    let path = dir.journal();
    let mut coordinator = Coordinator::open(&path).unwrap();
    let method = |name: &str| MethodSettings {
        service: "display/1".into(),
        method: name.into(),
        interface_major: 1,
        interface_minor: 0,
        schema_revision: 1,
    };
    let participant = ParticipantSettings {
        contract_version: 1,
        depends_on: vec![],
        provider: "display".into(),
        plan_method: method("plan"),
        callback_method: method("callback"),
        status_method: method("status"),
        binding_validate_method: None,
        capabilities: Default::default(),
        cleanup_methods: vec![],
        settings_revision: "rev".into(),
        settings: json!({}),
    };
    let approvals = [&participant.plan_method, &participant.callback_method, &participant.status_method]
        .iter().map(|method| MethodApproval { method: (*method).clone(), broker_approval: json!({"provider":"display","method":{"service":method.service,"method":method.method,"interface_major":method.interface_major,"interface_minor":method.interface_minor}}) }).collect();
    let selection = ProviderSelection {
        participant: participant.clone(),
        approvals,
        capabilities: Default::default(),
        cleanup_methods: vec![],
        authorized_borrowers: Default::default(),
        approved_control_bindings: Default::default(),
    };
    coordinator.settings.participants.push(participant);
    coordinator
        .selections
        .insert("display".into(), selection.clone());
    coordinator.approval_cursor = 3;
    let mut state = coordinator.shared.borrow().value.clone();
    let epoch = state.host_epoch.clone();
    let graph = ProviderGraph {
        order: vec!["display".into()],
        dependencies: [("display".into(), vec![])].into(),
    };
    let session_target = LifecycleTarget::Session {
        host_epoch: epoch.clone(),
        session_id: "session".into(),
        generation: 3,
    };
    let stream_target = LifecycleTarget::Stream {
        host_epoch: epoch.clone(),
        session_id: "session".into(),
        stream_id: "stream".into(),
        generation: 4,
    };
    let plan_value = json!({"plan_id":"plan","revision":1,"resources":[],"mutation_domains":[],"required_capabilities":[],"constraints":{"selected_settings_revision":"rev"},"obligations":[],"affected_consumers":[],"consumer_change":"reconnect_required","candidate_constraint":null,"allowed_degradation":[]});
    let plan: amaterasu_extension_api::ResourcePlan =
        serde_json::from_value(plan_value.clone()).unwrap();
    let stream = StreamRecord {
        provider_graph: Some(graph.clone()),
        generation: 4,
        phase: StreamPhase::Active,
        candidate: None,
        required: ["capture".into()].into(),
        acknowledgements: ["capture".into()].into(),
        retirements: Default::default(),
        binding_owner: "display".into(),
        preparation_transition: String::new(),
        provider_settings: [(
            "display".into(),
            settings_snapshot::FrozenSettings {
                settings: json!({}),
                revision: "rev".into(),
            },
        )]
        .into(),
    };
    let session = SessionRecord {
        resource_generations: BTreeMap::new(),
        provider_graph: Some(graph),
        host_epoch: epoch.clone(),
        id: "session".into(),
        generation: 3,
        phase: SessionPhase::Ready,
        owner: crate::action_authority::Principal {
            caller: "transport".into(),
            initiator: "user".into(),
        },
        delegates: Default::default(),
        application_ref: json!({}),
        settings_revision: "revision".into(),
        application_retired: false,
        application_retirement_proof: None,
        streams: [("stream".into(), stream)].into(),
        obligations: vec![],
        required_consumers: None,
        application: None,
        application_operation_id: None,
        admission_ids: ["session-admission".into(), "stream-admission".into()].into(),
        admission_targets: [
            ("session-admission".into(), session_target),
            ("stream-admission".into(), stream_target),
        ]
        .into(),
        capture_binding_mode: Default::default(),
        prepared_plan_ids: Default::default(),
        admission_phases: [
            ("session-admission".into(), LifecyclePhase::SessionPrepare),
            ("stream-admission".into(), LifecyclePhase::StreamPrepare),
        ]
        .into(),
        provider_settings: Default::default(),
    };
    state.sessions.insert("session".into(), session);
    for id in ["session-admission", "stream-admission"] {
        state.authority.settled.insert(
            id.into(),
            Admission {
                provider: "display".into(),
                selection: selection.clone(),
                plan: plan.clone(),
                rights: vec![],
            },
        );
        state
            .admission_transitions
            .insert(id.into(), format!("transition-{id}"));
    }
    save(&coordinator.shared, state).unwrap();
    let digest: String = Sha256::digest(serde_json::to_vec(&plan_value).unwrap())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let target =
        json!({"kind":"session","host_epoch":epoch,"session_id":"session","expected_generation":3});
    let service = coordinator.settings.action_service.clone();
    let auth = json!({"operation":"authorize_action","operation_id":"frozen-action","action_id":"display/remove","target":target,"plan_digest":digest,"plan":plan_value,"source":{"caller":"transport","initiator":"user"},"provider":"display","execute_method":{"service":"display/1","method":"execute","interface_major":1,"interface_minor":0,"schema_revision":1}});
    let permit = coordinator.action_operation(&service, "service", &auth);
    assert_eq!(permit["status"], "succeeded");
    let mut transition = json!({"operation":"consume_action_permit","operation_id":"frozen-action","action_id":"display/remove","target":target,"plan_digest":digest,"permit":permit["permit"]});
    assert_eq!(
        coordinator.action_operation(&service, "service", &transition)["status"],
        "succeeded"
    );
    transition["operation"] = json!("begin_action_transition");
    assert_eq!(
        coordinator.action_operation(&service, "service", &transition)["status"],
        "pending"
    );
    let frozen = coordinator.shared.borrow().value.clone();
    assert!(validate_journal(&frozen).is_ok());
    let key = receipt_key(&service, "action-transition", "frozen-action");
    let snapshot = frozen.receipts[&key]
        .work
        .as_ref()
        .unwrap()
        .action_transfer
        .as_ref()
        .unwrap()
        .snapshot
        .as_ref()
        .unwrap()
        .clone();
    assert!(
        snapshot
            .session_admissions
            .contains_key("session-admission")
    );
    assert!(snapshot.stream_admissions.contains_key("stream-admission"));
    for mutate in 0..4 {
        let mut tampered = frozen.clone();
        match mutate {
            0 => {
                tampered
                    .authority
                    .settled
                    .get_mut("session-admission")
                    .unwrap()
                    .plan
                    .revision += 1
            }
            1 => {
                *tampered
                    .admission_transitions
                    .get_mut("stream-admission")
                    .unwrap() = "other-transition".into()
            }
            2 => {
                tampered
                    .sessions
                    .get_mut("session")
                    .unwrap()
                    .owner
                    .initiator = "attacker".into()
            }
            _ => tampered.sessions.get_mut("session").unwrap().generation += 1,
        }
        assert!(
            validate_journal(&tampered).is_err(),
            "tampering case {mutate}"
        );
    }
    drop(coordinator);
    let recovered = Coordinator::open(&path).unwrap();
    let first_recovery_epoch = {
        let state = &recovered.shared.borrow().value;
        assert_ne!(state.host_epoch, epoch);
        let receipt = &state.receipts[&key];
        assert_eq!(receipt.response["status"], "recovery_required");
        assert!(
            receipt
                .work
                .as_ref()
                .unwrap()
                .action_transfer
                .as_ref()
                .unwrap()
                .snapshot
                .as_ref()
                == Some(&snapshot)
        );
        state.host_epoch.clone()
    };
    drop(recovered);
    let second_recovery = Coordinator::open(&path).unwrap();
    let state = &second_recovery.shared.borrow().value;
    assert_ne!(state.host_epoch, first_recovery_epoch);
    assert!(
        state.receipts[&key]
            .work
            .as_ref()
            .unwrap()
            .action_transfer
            .as_ref()
            .unwrap()
            .snapshot
            .as_ref()
            == Some(&snapshot)
    );
}
