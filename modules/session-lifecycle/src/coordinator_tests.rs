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
fn paired_source_attestation_requires_the_direct_protocol_owner_and_is_fenced_on_restart() {
    let dir = TestDirectory::new();
    let mut coordinator = Coordinator::open(&dir.journal()).unwrap();
    let request = json!({"operation":"prepare","operation_id":"source-session",
        "source_attestation":{"version":1,"kind":"paired_tls"}});
    for (caller, initiator) in [
        ("other", "other"),
        ("amaterasu.nvhttp", "other"),
        ("other", "amaterasu.nvhttp"),
    ] {
        assert_eq!(
            coordinator.handle(caller, initiator, request.clone())["reason"],
            "invalid_source_attestation"
        );
    }
    let result = coordinator.handle("amaterasu.nvhttp", "amaterasu.nvhttp", request.clone());
    assert_eq!(result["status"], "pending");
    let id = result["session_id"].as_str().unwrap();
    assert!(coordinator.shared.borrow().value.sessions[id].paired_source_attested);
    let mut changed = request;
    changed["source_attestation"] = json!({"version":1,"kind":"paired_tls","client_id":"forged"});
    assert_eq!(
        coordinator.handle("amaterasu.nvhttp", "amaterasu.nvhttp", changed)["reason"],
        "operation_id_conflict"
    );
    let session = id.to_owned();
    drop(coordinator);
    let coordinator = Coordinator::open(&dir.journal()).unwrap();
    assert!(!coordinator.shared.borrow().value.sessions[&session].paired_source_attested);
}

#[test]
fn real_broker_identity_cannot_be_replaced_by_payload_attestation_fields() {
    use amaterasu_contracts::{Grants, RuntimeConfig};
    use amaterasu_module_sdk::{Command, Response};
    use amaterasu_runtime::Broker;
    let dir = TestDirectory::new();
    let mut coordinator = Coordinator::open(&dir.journal()).unwrap();
    let service = "amaterasu.session-lifecycle/1";
    let mut configuration = RuntimeConfig::default();
    for caller in ["amaterasu.nvhttp", "untrusted"] {
        configuration.principals.insert(
            caller.into(),
            Grants {
                call: [service.into()].into(),
                ..Default::default()
            },
        );
    }
    configuration.principals.insert(
        "lifecycle".into(),
        Grants {
            provide: [service.into()].into(),
            ..Default::default()
        },
    );
    let mut broker = Broker::new(configuration).unwrap();
    assert!(matches!(
        broker.dispatch(
            "lifecycle",
            Command::Register {
                service: service.into()
            }
        ),
        Response::Ok
    ));
    for (caller, expected) in [("untrusted", "rejected"), ("amaterasu.nvhttp", "pending")] {
        let payload = json!({"operation":"prepare","operation_id":format!("source-{caller}"),
            "caller":"amaterasu.nvhttp","initiator":"amaterasu.nvhttp",
            "source_attestation":{"version":1,"kind":"paired_tls"}});
        let Response::Accepted { id } = broker.dispatch(
            caller,
            Command::Request {
                service: service.into(),
                payload,
                timeout_ms: 1000,
                operation_id: format!("broker-{caller}"),
                parent: None,
            },
        ) else {
            panic!("broker admission failed")
        };
        let Response::Request {
            id: incoming,
            caller: actual,
            initiator,
            payload,
            ..
        } = broker.dispatch("lifecycle", Command::Poll {})
        else {
            panic!("missing request")
        };
        assert_eq!(actual, caller);
        let response = coordinator.handle(&actual, &initiator, payload);
        assert_eq!(response["status"], expected);
        assert!(matches!(
            broker.dispatch(
                "lifecycle",
                Command::Complete {
                    request: incoming,
                    payload: response
                }
            ),
            Response::Ok
        ));
        assert!(matches!(
            broker.dispatch(caller, Command::TakeResult { request: id }),
            Response::Result { .. }
        ));
        assert!(matches!(
            broker.dispatch(caller, Command::ReleaseResult { request: id }),
            Response::Ok
        ));
    }
}

#[test]
fn opted_in_input_source_is_private_unique_and_fenced_by_binding_state() {
    let dir = TestDirectory::new();
    let mut coordinator = Coordinator::open(&dir.journal()).unwrap();
    coordinator.settings.consumers.input.insert("input".into());
    let session = coordinator.handle(
        "amaterasu.nvhttp",
        "amaterasu.nvhttp",
        json!({
        "operation":"prepare","operation_id":"source-session",
        "source_attestation":{"version":1,"kind":"paired_tls"}}),
    );
    let epoch = session["host_epoch"].as_str().unwrap().to_owned();
    let session_id = session["session_id"].as_str().unwrap().to_owned();
    let target = json!({"kind":"stream","host_epoch":epoch,"session_id":session_id,"stream_id":"stream","generation":4});
    let candidate: CaptureBinding = serde_json::from_value(json!({"capture_provider":"capture", "selector_schema":"selector/1", "selector":{}, "resource":{"provider":"display","resource_id":"output","incarnation":"native","revision":1,"execution_domain":"seat","mutation_domain":null},"mode":{"width":1920,"height":1080,"refresh_millihz":60000,"hdr":false},"plan_revision":1,"geometry_revision":7,"generation":4})).unwrap();
    {
        let mut shared = coordinator.shared.borrow_mut();
        let session = shared.value.sessions.get_mut(&session_id).unwrap();
        session.phase = SessionPhase::Ready;
        session.streams.insert(
            "stream".into(),
            StreamRecord {
                provider_graph: None,
                generation: 4,
                phase: StreamPhase::Ready,
                candidate: Some(candidate),
                required: BTreeSet::new(),
                acknowledgements: BTreeSet::from(["capture".into()]),
                retirements: BTreeSet::new(),
                binding_owner: String::new(),
                preparation_transition: String::new(),
                provider_settings: Default::default(),
            },
        );
    }
    let issue = |operation: &str| json!({"operation":"issue_input_binding","operation_id":operation,"target":target});
    let first = coordinator.handle("amaterasu.nvhttp", "amaterasu.nvhttp", issue("first"));
    let second = coordinator.handle("amaterasu.nvhttp", "amaterasu.nvhttp", issue("second"));
    assert_eq!(first["status"], "succeeded", "{first}");
    assert_eq!(second["status"], "succeeded", "{second}");
    let token = first["token"].as_str().unwrap();
    let mut validate =
        json!({"operation":"validate_input_binding","token":token,"source_version":1});
    let ready = coordinator.handle("input", "amaterasu.nvhttp", validate.clone());
    assert!(ready["source"].is_null());
    coordinator
        .shared
        .borrow_mut()
        .value
        .sessions
        .get_mut(&session_id)
        .unwrap()
        .streams
        .get_mut("stream")
        .unwrap()
        .phase = StreamPhase::Active;
    let active = coordinator.handle("input", "input", validate.clone());
    assert_eq!(active["source"]["version"], 1);
    assert_eq!(active["source"]["generation"], 4);
    assert_ne!(active["source"]["id"], first["token"]);
    assert!(active["paired_client"].is_null());
    let resolve = json!({"operation":"resolve_effect_sources","target":session["target"]});
    for (caller, initiator) in [
        ("input", "input"),
        ("amaterasu.nvhttp", "input"),
        ("foreign", "amaterasu.nvhttp"),
    ] {
        assert_eq!(
            coordinator.handle(caller, initiator, resolve.clone())["reason"],
            "permission_denied"
        );
    }
    let resolved = coordinator.handle("amaterasu.nvhttp", "amaterasu.nvhttp", resolve.clone());
    assert_eq!(resolved["status"], "succeeded");
    assert_eq!(resolved["sources"].as_array().unwrap().len(), 1);
    assert_eq!(resolved["sources"][0]["source"], active["source"]);
    assert_eq!(resolved["sources"][0]["target"], target);
    assert_eq!(resolved["sources"][0]["input_consumer"], "input");
    assert!(resolved.to_string().find(token).is_none());
    let mut forged = resolve.clone();
    forged["client_id"] = json!("forged");
    assert_eq!(
        coordinator.handle("amaterasu.nvhttp", "amaterasu.nvhttp", forged)["reason"],
        "invalid_request"
    );
    let legacy = coordinator.handle(
        "input",
        "input",
        json!({"operation":"validate_input_binding","token":token}),
    );
    assert!(legacy.get("source").is_none());
    let other = coordinator.handle(
        "input",
        "amaterasu.nvhttp",
        json!({"operation":"validate_input_binding","token":second["token"],"source_version":1}),
    );
    assert_ne!(active["source"]["id"], other["source"]["id"]);
    let mut restarted = validate.clone();
    restarted["input_boot_epoch"] = json!("different-provider-boot");
    assert_eq!(
        coordinator.handle("input", "input", restarted)["reason"],
        "input_provider_restarted"
    );
    coordinator
        .shared
        .borrow_mut()
        .value
        .sessions
        .get_mut(&session_id)
        .unwrap()
        .streams
        .get_mut("stream")
        .unwrap()
        .generation = 5;
    assert_eq!(
        coordinator.handle("input", "input", validate.clone())["reason"],
        "stale_input_binding"
    );
    let stale = coordinator.handle("amaterasu.nvhttp", "amaterasu.nvhttp", resolve.clone());
    assert!(stale["sources"].as_array().unwrap().is_empty());
    coordinator
        .shared
        .borrow_mut()
        .value
        .sessions
        .get_mut(&session_id)
        .unwrap()
        .streams
        .get_mut("stream")
        .unwrap()
        .generation = 4;
    coordinator
        .shared
        .borrow_mut()
        .value
        .sessions
        .get_mut(&session_id)
        .unwrap()
        .paired_source_attested = false;
    assert!(coordinator.handle("input", "input", validate.clone())["source"].is_null());
    coordinator
        .shared
        .borrow_mut()
        .value
        .sessions
        .get_mut(&session_id)
        .unwrap()
        .paired_source_attested = true;
    assert_eq!(
        coordinator.handle("foreign", "stream-session", validate.clone())["status"],
        "rejected"
    );
    assert_eq!(
        coordinator.handle("input", "foreign", validate.clone())["reason"],
        "source_initiator_denied"
    );
    coordinator
        .shared
        .borrow_mut()
        .value
        .sessions
        .get_mut(&session_id)
        .unwrap()
        .phase = SessionPhase::Ending;
    assert!(coordinator.handle("input", "input", validate.clone())["source"].is_null());
    coordinator
        .shared
        .borrow_mut()
        .value
        .sessions
        .get_mut(&session_id)
        .unwrap()
        .phase = SessionPhase::Ready;
    validate["source_version"] = json!(2);
    assert_eq!(
        coordinator.handle("input", "input", validate.clone())["reason"],
        "unsupported_source_version"
    );
    validate["source_version"] = json!(1);
    coordinator
        .shared
        .borrow_mut()
        .value
        .revoked_inputs
        .insert(token.into());
    assert_eq!(
        coordinator.handle("input", "input", validate)["reason"],
        "binding_revoked"
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
        paired_source_attested: false,
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

#[test]
fn selected_application_scope_requires_the_direct_paired_tls_coordinator() {
    let dir = TestDirectory::new();
    let mut coordinator = Coordinator::open(&dir.journal()).unwrap();
    coordinator.configured = true;
    let application = amaterasu_module_sdk::application_library::v2::ApplicationReference {
        library: "games".into(),
        id: "opaque/game".into(),
    };
    let scope =
        amaterasu_module_sdk::application_library::preflight::application_scope_id(&application)
            .unwrap();
    let reference = json!({"kind":"application","app_id":scope,"policy_identity":{
        "source":"library","version":1,"application":application,"entry_revision":"entry",
        "policy_revision":"policy","policy_source":"primary","snapshot_id":"snapshot"}});
    let make = |operation: &str| json!({"operation":"prepare","operation_id":operation,"application_ref":reference});
    let denied = coordinator.handle("foreign", "foreign", make("foreign"));
    assert_eq!(denied["status"], "rejected");
    let denied = coordinator.handle("amaterasu.nvhttp", "amaterasu.nvhttp", make("unpaired"));
    assert_eq!(denied["status"], "rejected");
    let mut trusted = make("trusted");
    trusted["source_attestation"] = json!({"version":1,"kind":"paired_tls"});
    assert_eq!(
        coordinator.handle("amaterasu.nvhttp", "amaterasu.nvhttp", trusted)["status"],
        "pending"
    );
}

#[test]
fn clipboard_stream_reservation_fences_shutdown_until_effect_settles() {
    let dir = TestDirectory::new();
    let mut coordinator = Coordinator::open(&dir.journal()).unwrap();
    let session = coordinator.handle("amaterasu.nvhttp", "amaterasu.nvhttp", json!({
        "operation":"prepare","operation_id":"clipboard-session", "source_attestation":{"version":1,"kind":"paired_tls"}}));
    let epoch = session["host_epoch"].as_str().unwrap().to_owned();
    let session_id = session["session_id"].as_str().unwrap().to_owned();
    let source = json!({"version":1,"host_epoch":epoch,"session_id":session_id,"stream_id":"stream","generation":4});
    let target = json!({"kind":"stream","host_epoch":epoch,"session_id":session_id,"stream_id":"stream","generation":4});
    let typed_source: amaterasu_module_sdk::effect_authority::ClipboardStream =
        serde_json::from_value(source.clone()).unwrap();
    let first_operation = typed_source.operation_id(1).unwrap();
    let next_operation = typed_source.operation_id(2).unwrap();
    let candidate: CaptureBinding = serde_json::from_value(json!({"capture_provider":"capture", "selector_schema":"selector/1", "selector":{}, "resource":{"provider":"display","resource_id":"output","incarnation":"native","revision":1,"execution_domain":"seat","mutation_domain":null},"mode":{"width":1920,"height":1080,"refresh_millihz":60000,"hdr":false},"plan_revision":1,"geometry_revision":7,"generation":4})).unwrap();
    {
        let mut shared = coordinator.shared.borrow_mut();
        let session = shared.value.sessions.get_mut(&session_id).unwrap();
        session.phase = SessionPhase::Ready;
        session.streams.insert(
            "stream".into(),
            StreamRecord {
                provider_graph: None,
                generation: 4,
                phase: StreamPhase::Active,
                candidate: Some(candidate),
                required: BTreeSet::new(),
                acknowledgements: BTreeSet::new(),
                retirements: BTreeSet::new(),
                binding_owner: String::new(),
                preparation_transition: String::new(),
                provider_settings: Default::default(),
            },
        );
    }
    let status = coordinator.handle(
        "amaterasu.nvhttp",
        "amaterasu.nvhttp",
        json!({"operation":"clipboard_stream_status"}),
    );
    assert_eq!(status["highwater"], 0);
    let reserve = json!({"operation":"reserve_clipboard_stream","operation_id":first_operation,"source":source,"sequence":1});
    assert_eq!(
        coordinator.handle("foreign", "foreign", reserve.clone())["reason"],
        "source_denied"
    );
    let receipt = coordinator.handle("amaterasu.nvhttp", "amaterasu.nvhttp", reserve.clone());
    assert_eq!(receipt["status"], "reserved", "{receipt}");
    assert_eq!(
        coordinator.handle("amaterasu.nvhttp", "amaterasu.nvhttp", reserve.clone()),
        receipt
    );
    let ticket = receipt["ticket"].as_str().unwrap().to_owned();
    let restart_dir = TestDirectory::new();
    std::fs::copy(dir.journal(), restart_dir.journal()).unwrap();
    let mut restarted = Coordinator::open(&restart_dir.journal()).unwrap();
    assert_eq!(
        restarted.handle(
            "amaterasu.nvhttp",
            "amaterasu.nvhttp",
            json!({"operation":"lookup_clipboard_stream","operation_id":first_operation})
        )["reason"],
        "clipboard_recovery_required"
    );
    let status = restarted.handle(
        "amaterasu.nvhttp",
        "amaterasu.nvhttp",
        json!({"operation":"clipboard_stream_status"}),
    );
    assert_eq!(status["highwater"], 1);
    assert_eq!(status["recovery_required"], true);
    drop(restarted);
    let paused = coordinator.handle(
        "amaterasu.nvhttp",
        "amaterasu.nvhttp",
        json!({"operation":"pause_stream","operation_id":"pause-clipboard","target":target}),
    );
    assert_eq!(paused["status"], "pending", "{paused}");
    assert_eq!(coordinator.handle("amaterasu.nvhttp","amaterasu.nvhttp",json!({"operation":"reserve_clipboard_stream","operation_id":next_operation,"source":source,"sequence":2}))["reason"],"source_denied");
    let work = coordinator
        .shared
        .borrow()
        .value
        .receipts
        .values()
        .find_map(|r| {
            r.work
                .clone()
                .filter(|w| w.phase == LifecyclePhase::StreamRelease)
        })
        .unwrap();
    assert!(!coordinator.retirement_ready(&work).unwrap());
    assert_eq!(coordinator.handle("foreign","foreign",json!({"operation":"complete_clipboard_stream","operation_id":first_operation,"ticket":ticket}))["reason"],"source_denied");
    assert_eq!(coordinator.handle("amaterasu.nvhttp","amaterasu.nvhttp",json!({"operation":"complete_clipboard_stream","operation_id":first_operation,"ticket":ticket}))["status"],"completed");
    assert!(coordinator.retirement_ready(&work).unwrap());
}
