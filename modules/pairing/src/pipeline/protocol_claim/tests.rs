use super::*;
use amaterasu_contracts::{
    ApiMethod, ApiMethodApproval, ApiPrivilege, ApiSchema, Grants,
    HOST_LIFECYCLE_CONTROL_PRINCIPAL, RuntimeConfig, SelectedService,
    effect_fence::{EffectFencePolicy, EffectSource, ProtectedMethod},
};
use amaterasu_runtime::Broker;
use amaterasu_state::test_support::TestStateStore;
use serde_json::{Value, json};

fn scope() -> EffectScope {
    EffectScope {
        authority_epoch: "authority".into(),
        issuer_epoch: "issuer-epoch".into(),
        source: EffectSource {
            version: 1,
            id: "a".repeat(32),
            generation: 1,
            host_epoch: "host".into(),
            session_id: "session".into(),
            stream_id: "stream".into(),
            stream_generation: 1,
            device: "device".into(),
            device_generation: 1,
            effect_owner_epoch: "owner".into(),
        },
    }
}
fn broker() -> Broker {
    broker_with_state_instance('b')
}
fn broker_with_state_instance(state_instance: char) -> Broker {
    broker_with_options(state_instance, true, false)
}
fn broker_with_recovery(state_instance: char, recover: bool) -> Broker {
    broker_with_options(state_instance, recover, false)
}
fn broker_with_handoff(state_instance: char) -> Broker {
    broker_with_options(state_instance, true, true)
}
fn broker_with_options(state_instance: char, recover: bool, handoff: bool) -> Broker {
    let mut config = RuntimeConfig {
        host_lifecycle_control: true,
        ..Default::default()
    };
    config
        .principals
        .insert(HOST_LIFECYCLE_CONTROL_PRINCIPAL.into(), Grants::default());
    config.principals.insert(
        "pairing".into(),
        Grants {
            provide: [
                "amaterasu.pairing-effects/1".into(),
                "amaterasu.pairing/1".into(),
            ]
            .into(),
            call: [STATE_SERVICE.into()].into(),
            ..Default::default()
        },
    );
    config.principals.insert(
        "console".into(),
        Grants {
            call: ["amaterasu.pairing/1".into(), STATE_SERVICE.into()].into(),
            ..Default::default()
        },
    );
    config.principals.insert(
        "untrusted".into(),
        Grants {
            call: [
                "amaterasu.pairing/1".into(),
                "amaterasu.pairing-effects/1".into(),
            ]
            .into(),
            ..Default::default()
        },
    );
    config.principals.insert(
        "issuer".into(),
        Grants {
            call: [
                "amaterasu.pairing-effects/1".into(),
                "example.effects/1".into(),
            ]
            .into(),
            ..Default::default()
        },
    );
    config.principals.insert(
        "provider".into(),
        Grants {
            provide: ["example.effects/1".into()].into(),
            ..Default::default()
        },
    );
    config.principals.insert(
        "state".into(),
        Grants {
            provide: [STATE_SERVICE.into()].into(),
            ..Default::default()
        },
    );
    config.service_selections.broker.insert(
        STATE_SERVICE.into(),
        Some(SelectedService {
            provider: "state".into(),
            contract: STATE_SERVICE.into(),
        }),
    );
    if handoff {
        config.service_selections.broker.insert(
            "amaterasu.pairing-effects/1".into(),
            Some(SelectedService {
                provider: "pairing".into(),
                contract: "amaterasu.pairing-effects/1".into(),
            }),
        );
    }
    config.host_context.durable_cas_provider = Some("state".into());
    config.host_context.provider_incarnations.insert(
        "state".into(),
        amaterasu_contracts::effect_fence::ProviderIncarnation {
            version: 1,
            id: state_instance.to_string().repeat(32),
        },
    );
    let method = ApiMethod {
        service: "example.effects/1".into(),
        method: "apply".into(),
        interface_major: 1,
        interface_minor: 0,
        request: ApiSchema {
            required: Default::default(),
            optional: Default::default(),
            allow_extra: handoff,
        },
        response: ApiSchema {
            required: Default::default(),
            optional: Default::default(),
            allow_extra: false,
        },
        privilege: ApiPrivilege::User,
    };
    let mut cleanup_method = method.clone();
    cleanup_method.method = "cleanup".into();
    config.host_context.provider_incarnations.insert(
        "provider".into(),
        amaterasu_contracts::effect_fence::ProviderIncarnation {
            version: 1,
            id: "a".repeat(32),
        },
    );
    config.api_methods.push(ApiMethodApproval {
        provider: "provider".into(),
        callers: ["issuer".into()].into(),
        method: method.clone(),
    });
    config.api_methods.push(ApiMethodApproval {
        provider: "provider".into(),
        callers: ["issuer".into()].into(),
        method: cleanup_method.clone(),
    });
    config.effect_fence = Some(EffectFencePolicy {
        authority: "pairing".into(),
        issuer: "issuer".into(),
        source_slots: true,
        require_handoff: handoff,
        methods: vec![ProtectedMethod {
            service: "example.effects/1".into(),
            method: "apply".into(),
            interface_major: 1,
        }],
        cleanup_methods: vec![ProtectedMethod {
            service: "example.effects/1".into(),
            method: "cleanup".into(),
            interface_major: 1,
        }],
    });
    let mut broker = Broker::new(config).unwrap();
    for (caller, service) in [
        ("pairing", "amaterasu.pairing-effects/1"),
        ("pairing", "amaterasu.pairing/1"),
        ("state", STATE_SERVICE),
    ] {
        assert!(matches!(
            broker.dispatch(
                caller,
                Command::Register {
                    service: service.into()
                }
            ),
            Response::Ok
        ));
    }
    assert!(matches!(
        broker.dispatch("provider", Command::RegisterApi { method }),
        Response::Ok
    ));
    assert!(matches!(
        broker.dispatch(
            "provider",
            Command::RegisterApi {
                method: cleanup_method
            }
        ),
        Response::Ok
    ));
    if !recover {
        return broker;
    }
    // Test fixture only: production participants must independently retire real workers.
    let Response::Result { payload } = broker.dispatch(
        "pairing",
        Command::EffectFence {
            command: Box::new(EffectFenceCommand::BrokerEpoch {}),
        },
    ) else {
        panic!("epoch")
    };
    let epoch = payload["broker_epoch"].as_str().unwrap();
    for caller in [
        "issuer",
        "provider",
        "state",
        HOST_LIFECYCLE_CONTROL_PRINCIPAL,
    ] {
        assert!(matches!(
            broker.dispatch(
                caller,
                Command::EffectFence {
                    command: Box::new(EffectFenceCommand::RecoveryRetired {
                        broker_epoch: epoch.into()
                    })
                }
            ),
            Response::Ok
        ));
    }
    assert!(matches!(
        broker.dispatch(
            "pairing",
            Command::EffectFence {
                command: Box::new(EffectFenceCommand::RecoverSlots {
                    broker_epoch: epoch.into(),
                    retired_incarnations: vec![0; 64]
                })
            }
        ),
        Response::Ok
    ));
    broker
}
fn open_parent(broker: &mut Broker) -> u64 {
    let Response::Accepted { id } = broker.dispatch(
        "issuer",
        Command::Request {
            service: "amaterasu.pairing-effects/1".into(),
            payload: json!({"test":"claim"}),
            timeout_ms: 30_000,
            operation_id: "claim.parent".into(),
            parent: None,
        },
    ) else {
        panic!("parent")
    };
    assert!(matches!(
        broker.dispatch("pairing", Command::Poll {}),
        Response::Request { .. }
    ));
    id
}
fn binding() -> DurableStateBinding {
    DurableStateBinding::new(
        "pairing",
        "state",
        amaterasu_module_sdk::protocol_state::DURABLE_CAS_CAPABILITY,
    )
    .unwrap()
}
fn directory() -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "claim-actor-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}
fn service_one(broker: &mut Broker, store: &TestStateStore) -> Option<(u64, Value)> {
    match broker.dispatch("state", Command::Poll {}) {
        Response::Request {
            id,
            caller,
            payload,
            ..
        } => Some((id, store.handle(&caller, payload))),
        Response::Pending => None,
        other => panic!("unexpected State request: {other:?}"),
    }
}
fn finish_state(broker: &mut Broker, result: (u64, Value)) {
    assert!(matches!(
        broker.dispatch(
            "state",
            Command::Complete {
                request: result.0,
                payload: result.1
            }
        ),
        Response::Ok
    ));
}

#[test]
fn claim_actor_persists_before_any_broker_slot_is_opened() {
    let path = directory();
    let store = TestStateStore::open(path.clone()).unwrap();
    let mut broker = broker();
    let parent = open_parent(&mut broker);
    let mut job = ClaimJob::new(parent, scope(), "issuer".into(), binding()).unwrap();
    let mut claimed = None;
    for _ in 0..20 {
        match job
            .advance(true, |command| Ok(broker.dispatch("pairing", command)))
            .unwrap()
        {
            Outcome::Pending => {}
            Outcome::Claimed { lease, scope } => {
                claimed = Some((lease, scope));
                break;
            }
            Outcome::Failed(code) => panic!("{code}"),
        }
        if let Some(result) = service_one(&mut broker, &store) {
            finish_state(&mut broker, result);
        }
    }
    let (lease, scope) = claimed.expect("claim not durable");
    let value = store.handle(
        "pairing",
        protocol_slot_state::read_request(lease.slot)
            .unwrap()
            .payload(),
    );
    let (_, slot) = protocol_slot_state::read(lease.slot, &value).unwrap();
    assert_eq!(slot.active.unwrap().scope, *scope);
    let Response::Result { payload } = broker.dispatch(
        "pairing",
        Command::EffectFence {
            command: Box::new(EffectFenceCommand::SlotStatus { slot: lease.slot }),
        },
    ) else {
        panic!("status")
    };
    assert!(payload["active"].is_null());
    drop(store);
    std::fs::remove_dir_all(path).unwrap();
}

#[test]
fn delivered_write_after_parent_cancellation_never_opens_the_slot() {
    let path = directory();
    let store = TestStateStore::open(path.clone()).unwrap();
    let mut broker = broker();
    let parent = open_parent(&mut broker);
    let mut job = ClaimJob::new(parent, scope(), "issuer".into(), binding()).unwrap();
    assert!(matches!(
        job.advance(true, |command| Ok(broker.dispatch("pairing", command)))
            .unwrap(),
        Outcome::Pending
    ));
    let read = service_one(&mut broker, &store).unwrap();
    finish_state(&mut broker, read);
    assert!(matches!(
        job.advance(true, |command| Ok(broker.dispatch("pairing", command)))
            .unwrap(),
        Outcome::Pending
    ));
    // The State provider has durably committed the CAS, but its reply has not
    // reached the authority when the original request is cancelled.
    let (write_id, write_result) = service_one(&mut broker, &store).unwrap();
    assert_eq!(write_result["ok"], true);
    assert!(matches!(
        broker.dispatch("issuer", Command::Cancel { request: parent }),
        Response::Ok
    ));
    assert!(matches!(
        broker.dispatch(
            "state",
            Command::Complete {
                request: write_id,
                payload: write_result
            }
        ),
        Response::Ok
    ));
    assert!(matches!(
        job.advance(true, |command| Ok(broker.dispatch("pairing", command)))
            .unwrap(),
        Outcome::Failed("protocol_claim_uncertain")
    ));
    let slot = scope().source.slot().unwrap();
    let value = store.handle(
        "pairing",
        protocol_slot_state::read_request(slot).unwrap().payload(),
    );
    assert!(
        protocol_slot_state::read(slot, &value)
            .unwrap()
            .1
            .active
            .is_some()
    );
    let Response::Result { payload } = broker.dispatch(
        "pairing",
        Command::EffectFence {
            command: Box::new(EffectFenceCommand::SlotStatus { slot }),
        },
    ) else {
        panic!("status")
    };
    assert!(payload["active"].is_null());
    drop(store);
    std::fs::remove_dir_all(path).unwrap();
}

#[test]
fn pairing_actor_drives_real_state_claim_and_fences_future_trust_changes() {
    let path = directory();
    let store = TestStateStore::open(path.clone()).unwrap();
    let mut broker = broker();
    let (mut pairing, fixture, certificate, epoch, source, _scope) =
        super::super::tests::authorized_feedback_for_test();
    assert_eq!(store.handle("pairing",json!({"action":"set","key":"pairing/v1","value":fixture.document.unwrap(),"expected_revision":0}))["revision"],1);
    pairing.effect_authority.as_mut().unwrap().effect_owner = "issuer".into();
    let method = json!({"service":"example.effects/1","method":"apply","interface_major":1,"interface_minor":0,"schema_revision":1});
    let cleanup = json!({"service":"example.effects/1","method":"cleanup","interface_major":1,"interface_minor":0,"schema_revision":1});
    let schema = |method: &Value| {
        let mut value = method.clone();
        value.as_object_mut().unwrap().remove("schema_revision");
        value["request"] = json!({"required":{},"optional":{},"allow_extra":false});
        value["response"] = json!({"required":{},"optional":{},"allow_extra":false});
        value["privilege"] = json!("user");
        value
    };
    pairing.effect_authority.as_mut().unwrap().protocol_policies=serde_json::from_value(json!([{
        "capability":"example.control/1","provider":"provider","message_type":21818,"method":method,"cleanup":cleanup,
        "method_schema":schema(&method),"cleanup_schema":schema(&cleanup),"max_payload_bytes":64,"required_permission":"view"}])).unwrap();
    let attested = pairing
        .apply_effect_request(
            "nvhttp",
            "nvhttp",
            json!({"action":"attest_protocol_capabilities","version":1,
        "epoch":epoch,"source":source,"capabilities":["example.control/1"]}),
            super::super::tests::now(),
        )
        .unwrap();
    let source: amaterasu_module_sdk::effect_authority::Source =
        serde_json::from_value(source).unwrap();
    let scope = EffectScope {
        authority_epoch: epoch.clone(),
        issuer_epoch: attested["issuer_epoch"].as_str().unwrap().into(),
        source: amaterasu_module_sdk::broker_effect_fence::source(&source).unwrap(),
    };
    let parent = open_parent(&mut broker);
    pairing.pending_protocol_claim =
        Some(ClaimJob::new(parent, scope.clone(), "issuer".into(), binding()).unwrap());
    let mut result = None;
    for _ in 0..30 {
        pairing
            .tick_at(super::super::tests::now(), |command| {
                Ok(broker.dispatch("pairing", command))
            })
            .unwrap();
        if let Some(state) = service_one(&mut broker, &store) {
            finish_state(&mut broker, state);
        }
        if let Response::Result { payload } =
            broker.dispatch("issuer", Command::TakeResult { request: parent })
        {
            result = Some(payload);
            break;
        }
    }
    assert_eq!(result.unwrap()["code"], "protocol_reconciliation_required");
    assert!(pairing.protocol_claim_recovery_required);
    assert!(pairing.retained_protocol_claim.is_some());
    let stored = store.handle(
        "pairing",
        protocol_slot_state::read_request(scope.source.slot().unwrap())
            .unwrap()
            .payload(),
    );
    assert_eq!(
        protocol_slot_state::read(scope.source.slot().unwrap(), &stored)
            .unwrap()
            .1
            .active
            .unwrap()
            .scope,
        scope
    );
    let recover = pairing.apply_effect_request(
        "issuer",
        "issuer",
        json!({"action":"recover","epoch":epoch,"owner_epoch":"owner"}),
        super::super::tests::now(),
    );
    assert_eq!(recover, Err("protocol_reconciliation_required"));
    let Response::Accepted { id } = broker.dispatch(
        "console",
        Command::Request {
            service: "amaterasu.pairing/1".into(),
            payload: json!({"action":"set_permission_mode","enabled":true,"expected_revision":1}),
            timeout_ms: 30_000,
            operation_id: "trust-change".into(),
            parent: None,
        },
    ) else {
        panic!("trust request")
    };
    pairing
        .tick_at(super::super::tests::now(), |command| {
            Ok(broker.dispatch("pairing", command))
        })
        .unwrap();
    let Response::Result { payload } =
        broker.dispatch("console", Command::TakeResult { request: id })
    else {
        panic!("trust response")
    };
    assert_eq!(payload["code"], "protocol_reconciliation_required");
    assert!(matches!(
        broker.dispatch("state", Command::Poll {}),
        Response::Pending
    ));
    let config_update = |overrides: Value,
                         revision: u64,
                         operation: &str,
                         pairing: &mut super::super::Pipeline,
                         broker: &mut Broker|
     -> Value {
        let Response::Accepted{id}=broker.dispatch("console",Command::Request {service:"amaterasu.pairing/1".into(),
            payload:json!({"action":"update_client_config_overrides","certificate_id":certificate,"config_overrides":overrides,"expected_revision":revision}),
            timeout_ms:30_000,operation_id:operation.into(),parent:None}) else {panic!("config request")};
        for _ in 0..30 {
            pairing
                .tick_at(super::super::tests::now(), |command| {
                    Ok(broker.dispatch("pairing", command))
                })
                .unwrap();
            if let Some(state) = service_one(broker, &store) {
                finish_state(broker, state);
            }
            if let Response::Result { payload } =
                broker.dispatch("console", Command::TakeResult { request: id })
            {
                return payload;
            }
        }
        panic!("config request pending")
    };
    let tuning = config_update(
        json!({"fec_percentage":10}),
        2,
        "tuning",
        &mut pairing,
        &mut broker,
    );
    assert_eq!(tuning["ok"], true);
    let security = config_update(
        json!({"fec_percentage":10,"controller":false}),
        3,
        "controller-policy",
        &mut pairing,
        &mut broker,
    );
    assert_eq!(security["code"], "protocol_reconciliation_required");
    drop(store);
    std::fs::remove_dir_all(path).unwrap();
}

#[test]
fn lost_state_write_reply_reconciles_only_the_exact_durable_claim() {
    let path = directory();
    let store = TestStateStore::open(path.clone()).unwrap();
    let mut broker = broker();
    let parent = open_parent(&mut broker);
    let mut job = ClaimJob::new(parent, scope(), "issuer".into(), binding()).unwrap();
    assert!(matches!(
        job.advance(true, |command| Ok(broker.dispatch("pairing", command)))
            .unwrap(),
        Outcome::Pending
    ));
    let read = service_one(&mut broker, &store).unwrap();
    finish_state(&mut broker, read);
    assert!(matches!(
        job.advance(true, |command| Ok(broker.dispatch("pairing", command)))
            .unwrap(),
        Outcome::Pending
    ));
    let write = service_one(&mut broker, &store).unwrap();
    finish_state(&mut broker, write);
    let mut lost = false;
    let result = job
        .advance(true, |command| {
            if matches!(command, Command::TakeResult { .. }) && !lost {
                lost = true;
                Err("lost response".into())
            } else {
                Ok(broker.dispatch("pairing", command))
            }
        })
        .unwrap();
    assert!(matches!(result, Outcome::Pending));
    let read = service_one(&mut broker, &store).unwrap();
    finish_state(&mut broker, read);
    let Outcome::Claimed {
        lease,
        scope: claimed,
    } = job
        .advance(true, |command| Ok(broker.dispatch("pairing", command)))
        .unwrap()
    else {
        panic!("claim was not reconciled")
    };
    assert_eq!(*claimed, scope());
    let value = store.handle(
        "pairing",
        protocol_slot_state::read_request(lease.slot)
            .unwrap()
            .payload(),
    );
    assert_eq!(
        protocol_slot_state::read(lease.slot, &value)
            .unwrap()
            .1
            .active
            .unwrap()
            .lease,
        lease
    );
    let Response::Result { payload } = broker.dispatch(
        "pairing",
        Command::EffectFence {
            command: Box::new(EffectFenceCommand::SlotStatus { slot: lease.slot }),
        },
    ) else {
        panic!("status")
    };
    assert!(payload["active"].is_null());
    drop(store);
    std::fs::remove_dir_all(path).unwrap();
}

#[test]
fn default_feedback_recovers_without_selecting_historical_profile() {
    let settings: crate::protocol::Settings = serde_json::from_value(json!({
        "administrator_principals": ["console"], "transport_principals": ["nvhttp"],
        "effect_authority": { "protocol_owner": "nvhttp", "effect_owner": "issuer",
            "feedback_policies": [{"capability":{"schema":"org.amaterasu.feedback.rgb_led","version":1,"direction":"to_client"},"required_permission":"view"}] }
    }))
    .unwrap();
    settings.validate().unwrap();
    let mut pairing = super::super::Pipeline::new(settings);
    assert!(!pairing.protocol_claim_recovery_required);
    let epoch = pairing.effect_gate.as_ref().unwrap().epoch().to_owned();
    let recovered = pairing
        .apply_effect_request(
            "issuer",
            "issuer",
            json!({"action":"recover","epoch":epoch,"owner_epoch":"owner"}),
            super::super::tests::now(),
        )
        .unwrap();
    assert_eq!(recovered["ok"], true);
    assert!(!pairing.effect_gate.as_ref().unwrap().fenced());
}

#[test]
fn retained_historical_state_quarantines_removed_policies_and_trust_writes() {
    let settings: crate::protocol::Settings = serde_json::from_value(json!({
        "administrator_principals": ["console"], "transport_principals": ["nvhttp"],
        "effect_authority": { "protocol_owner": "nvhttp", "effect_owner": "issuer",
            "feedback_policies": [], "protocol_policies": [] },
        "protocol_state": {"owner":"pairing", "provider":"state",
            "capability":amaterasu_module_sdk::protocol_state::DURABLE_CAS_CAPABILITY}
    }))
    .unwrap();
    settings.validate().unwrap();
    let mut pairing = super::super::Pipeline::new(settings);
    assert!(pairing.protocol_claim_recovery_required);
    assert!(pairing.protocol_recovery_auto);
    let mut broker = broker();
    let mut queried = false;
    pairing.progress_protocol_recovery(&mut |command| {
        queried |= matches!(&command, Command::EffectFence { command }
            if matches!(**command, EffectFenceCommand::SlotStatus { .. }));
        Ok(broker.dispatch("pairing", command))
    });
    assert!(
        queried,
        "retained history must not be skipped when policies are removed"
    );
    let Response::Accepted { id } = broker.dispatch(
        "console",
        Command::Request {
            service: "amaterasu.pairing/1".into(),
            payload: json!({"action":"set_permission_mode","enabled":true,"expected_revision":0}),
            timeout_ms: 30_000,
            operation_id: "retained-history.trust-change".into(),
            parent: None,
        },
    ) else {
        panic!("trust request")
    };
    pairing
        .tick_at(super::super::tests::now(), |command| {
            Ok(broker.dispatch("pairing", command))
        })
        .unwrap();
    assert!(
        matches!(broker.dispatch("console", Command::TakeResult { request:id }),
        Response::Result { payload } if payload["code"] == "protocol_reconciliation_required")
    );
}

#[test]
fn configured_protocol_authority_starts_closed_until_slot_recovery() {
    let method = json!({"service":"example.effects/1","method":"apply","interface_major":1,"interface_minor":0,"schema_revision":1});
    let cleanup = json!({"service":"example.effects/1","method":"cleanup","interface_major":1,"interface_minor":0,"schema_revision":1});
    let schema = |method: &Value| {
        let mut value = method.clone();
        value.as_object_mut().unwrap().remove("schema_revision");
        value["request"] = json!({"required":{},"optional":{},"allow_extra":false});
        value["response"] = json!({"required":{},"optional":{},"allow_extra":false});
        value["privilege"] = json!("user");
        value
    };
    let settings:crate::protocol::Settings=serde_json::from_value(json!({"administrator_principals":["console"],"transport_principals":["nvhttp"],
        "effect_authority":{"protocol_owner":"nvhttp","effect_owner":"issuer","protocol_policies":[{
            "capability":"example.control/1","provider":"provider","message_type":21818,"method":method,"cleanup":cleanup,
            "method_schema":schema(&method),"cleanup_schema":schema(&cleanup),"max_payload_bytes":64,"required_permission":"view"}]}})).unwrap();
    settings.validate().unwrap();
    let mut pairing = super::super::Pipeline::new(settings);
    assert!(pairing.protocol_claim_recovery_required);
    let initial = pairing.effect_gate.as_ref().unwrap().epoch().to_owned();
    assert_eq!(
        pairing.apply_effect_request(
            "issuer",
            "issuer",
            json!({"action":"recover","epoch":initial,"owner_epoch":"owner"}),
            super::super::tests::now()
        ),
        Err("protocol_reconciliation_required")
    );
    let mut broker = broker();
    let Response::Accepted { id } = broker.dispatch(
        "console",
        Command::Request {
            service: "amaterasu.pairing/1".into(),
            payload: json!({"action":"set_permission_mode","enabled":true,"expected_revision":0}),
            timeout_ms: 30_000,
            operation_id: "startup-trust-change".into(),
            parent: None,
        },
    ) else {
        panic!("trust request")
    };
    pairing
        .tick_at(super::super::tests::now(), |command| {
            Ok(broker.dispatch("pairing", command))
        })
        .unwrap();
    let Response::Result { payload } =
        broker.dispatch("console", Command::TakeResult { request: id })
    else {
        panic!("trust response")
    };
    assert_eq!(payload["code"], "protocol_reconciliation_required");
    assert!(matches!(
        broker.dispatch("state", Command::Poll {}),
        Response::Pending
    ));
}

#[test]
fn real_pairing_request_atomically_admits_selected_effect() {
    run_real_protocol_issue(false, false);
}

#[test]
fn canceled_request_after_issue_write_keeps_durable_obligation_fenced() {
    run_real_protocol_issue(true, false);
}

#[test]
fn state_provider_replacement_mid_claim_leaves_claim_fenced() {
    run_real_protocol_issue(false, true);
}

fn run_real_protocol_issue(cancel_after_write: bool, replace_after_claim: bool) {
    use amaterasu_module_sdk::{
        effect_authority::{Request as EffectRequest, Source},
        extension_api::{LifecycleTarget, MethodRef},
        protocol::{DispatchRequest, ProtocolRouter, Route, SessionContext},
        protocol_slot_adapter::{SelectedSlotAction, SlotActionRequest},
        protocol_state::CleanupReference,
    };
    let path = directory();
    let store = TestStateStore::open(path.clone()).unwrap();
    let mut broker = broker_with_handoff('b');
    let (mut pairing, fixture, _certificate, epoch, source_json) =
        super::super::tests::authorized_protocol_for_test();
    assert_eq!(store.handle("pairing", json!({"action":"set","key":"pairing/v1","value":fixture.document.unwrap(),"expected_revision":0}))["revision"], 1);
    let source: Source = serde_json::from_value(source_json).unwrap();
    let method = MethodRef {
        service: "example.effects/1".into(),
        method: "apply".into(),
        interface_major: 1,
        interface_minor: 0,
        schema_revision: 1,
    };
    let cleanup = MethodRef {
        method: "cleanup".into(),
        ..method.clone()
    };
    let schema = ApiMethod {
        service: method.service.clone(),
        method: method.method.clone(),
        interface_major: 1,
        interface_minor: 0,
        request: ApiSchema {
            required: Default::default(),
            optional: Default::default(),
            allow_extra: true,
        },
        response: ApiSchema {
            required: Default::default(),
            optional: Default::default(),
            allow_extra: false,
        },
        privilege: ApiPrivilege::User,
    };
    let mut cleanup_schema = schema.clone();
    cleanup_schema.method = "cleanup".into();
    let route = Route {
        message_type: 21818,
        capability: "example.control/1".into(),
        approval: ApiMethodApproval {
            provider: "provider".into(),
            callers: ["issuer".into()].into(),
            method: schema.clone(),
        },
        method: method.clone(),
        cleanup: cleanup.clone(),
        cleanup_approval: ApiMethodApproval {
            provider: "provider".into(),
            callers: ["issuer".into()].into(),
            method: cleanup_schema.clone(),
        },
        max_payload_bytes: 64,
    };
    pairing.effect_authority.as_mut().unwrap().protocol_policies = serde_json::from_value(json!([{"capability":"example.control/1","provider":"provider","message_type":21818,"method":method,"cleanup":cleanup,"method_schema":schema,"cleanup_schema":cleanup_schema,"max_payload_bytes":64,"required_permission":"view"}])).unwrap();
    pairing.protocol_state_binding = Some(binding());
    let attested = pairing.apply_effect_request("nvhttp", "nvhttp", json!({"action":"attest_protocol_capabilities","version":1,"epoch":epoch,"source":source,"capabilities":["example.control/1"]}), super::super::tests::now()).unwrap();
    let issuer_epoch = attested["issuer_epoch"].as_str().unwrap().to_owned();
    let scope = EffectScope {
        authority_epoch: epoch.clone(),
        issuer_epoch: issuer_epoch.clone(),
        source: amaterasu_module_sdk::broker_effect_fence::source(&source).unwrap(),
    };
    let context = SessionContext {
        authority_epoch: epoch.clone(),
        effect_owner_epoch: source.effect_owner_epoch.clone(),
        target: LifecycleTarget::Stream {
            host_epoch: source.host_epoch.clone(),
            session_id: source.session_id.clone(),
            stream_id: source.stream_id.clone(),
            generation: source.stream_generation,
        },
        peer_ref: source.id.clone(),
    };
    let operation_id = protocol_slot_state::operation_id("issuer", &scope, 1).unwrap();
    let bytes = vec![1, 2, 3];
    let effect = ProtocolRouter::new(vec![route.clone()])
        .unwrap()
        .prepare_effect(
            "issuer",
            &DispatchRequest {
                context: &context,
                message_type: 21818,
                payload: &bytes,
                operation_id: &operation_id,
                issuer_epoch: &issuer_epoch,
                timeout_ms: 5_000,
                parent: None,
            },
        )
        .unwrap();
    let request = EffectRequest::AdmitProtocolEffectV2 {
        version: 2,
        epoch,
        source: source.clone(),
        effect: Box::new(effect.clone()),
        sequence: 1,
        payload: bytes.clone(),
        cleanup: CleanupReference::new("b".repeat(64)).unwrap(),
    };
    let payload = serde_json::to_value(request).unwrap();
    let original_instance = pairing
        .selected_protocol_state(&mut |command| Ok(broker.dispatch("pairing", command)))
        .unwrap();
    pairing.protocol_state_instance = Some(original_instance);
    let mut replacement = broker_with_handoff('c');
    assert!(
        !pairing
            .protocol_state_current(&mut |command| Ok(replacement.dispatch("pairing", command)))
    );
    pairing.protocol_state_binding = Some(
        DurableStateBinding::new(
            "pairing",
            "wrong-state",
            amaterasu_module_sdk::protocol_state::DURABLE_CAS_CAPABILITY,
        )
        .unwrap(),
    );
    let Response::Accepted { id: mismatched_id } = broker.dispatch(
        "issuer",
        Command::Request {
            service: "amaterasu.pairing-effects/1".into(),
            payload: payload.clone(),
            timeout_ms: 30_000,
            operation_id: "admit.wrong-state".into(),
            parent: None,
        },
    ) else {
        panic!("mismatch request denied too early")
    };
    pairing
        .tick_at(super::super::tests::now(), |command| {
            Ok(broker.dispatch("pairing", command))
        })
        .unwrap();
    assert!(
        matches!(broker.dispatch("issuer", Command::TakeResult { request:mismatched_id }), Response::Result { payload } if payload["code"] == "protocol_authority_unavailable")
    );
    assert!(matches!(
        broker.dispatch("state", Command::Poll {}),
        Response::Pending
    ));
    pairing.protocol_state_binding = Some(binding());
    let mut forged = payload.clone();
    forged["effect"]["action_approval_sha256"] = json!("c".repeat(64));
    let Response::Accepted { id: forged_id } = broker.dispatch(
        "issuer",
        Command::Request {
            service: "amaterasu.pairing-effects/1".into(),
            payload: forged,
            timeout_ms: 30_000,
            operation_id: "admit.forged".into(),
            parent: None,
        },
    ) else {
        panic!("forged request denied too early")
    };
    pairing
        .tick_at(super::super::tests::now(), |command| {
            Ok(broker.dispatch("pairing", command))
        })
        .unwrap();
    assert!(
        matches!(broker.dispatch("issuer", Command::TakeResult { request:forged_id }), Response::Result { payload } if payload["code"] == "scope_denied")
    );
    assert!(matches!(
        broker.dispatch("state", Command::Poll {}),
        Response::Pending
    ));
    let Response::Accepted { id } = broker.dispatch(
        "issuer",
        Command::Request {
            service: "amaterasu.pairing-effects/1".into(),
            payload,
            timeout_ms: 30_000,
            operation_id: "admit.test".into(),
            parent: None,
        },
    ) else {
        panic!("admission request denied")
    };
    let mut result = None;
    let mut canceled = false;
    let mut replaced = false;
    for _ in 0..50 {
        pairing
            .tick_at(super::super::tests::now(), |command| {
                if replaced && matches!(&command, Command::EffectFence { command } if matches!(**command, EffectFenceCommand::SelectedStateProvider {})) {
                    Ok(replacement.dispatch("pairing", command))
                } else {
                    Ok(broker.dispatch("pairing", command))
                }
            })
            .unwrap();
        if let Some(state) = service_one(&mut broker, &store) {
            if cancel_after_write && state.1["revision"] == 3 && state.1["ok"] == true {
                assert!(matches!(
                    broker.dispatch("issuer", Command::Cancel { request: id }),
                    Response::Ok
                ));
                canceled = true;
            }
            if replace_after_claim && state.1["revision"] == 2 && state.1["ok"] == true {
                replaced = true;
            }
            finish_state(&mut broker, state);
        }
        if let Response::Result { payload } =
            broker.dispatch("issuer", Command::TakeResult { request: id })
        {
            result = Some(payload);
            break;
        }
        if (canceled || replaced) && pairing.protocol_claim_recovery_required {
            break;
        }
    }
    if cancel_after_write {
        assert!(canceled && pairing.protocol_claim_recovery_required);
        assert!(result.is_none());
    } else if replace_after_claim {
        assert!(replaced && pairing.protocol_claim_recovery_required);
    } else {
        let response = result.unwrap();
        assert_eq!(response["ok"], true);
        let lease: amaterasu_module_sdk::broker_effect_fence::SourceSlotLease =
            serde_json::from_value(response["lease"].clone()).unwrap();
        let action = SelectedSlotAction::query(
            SlotActionRequest {
                pending: effect,
                issuer: &source,
                scope: scope.clone(),
                lease: lease.clone(),
                route: &route,
                sequence: 1,
                payload: &bytes,
            },
            |command| Ok(broker.dispatch("pairing", command)),
        )
        .unwrap();
        let guarded = action.guarded_effect(&bytes).unwrap();
        assert!(matches!(
            broker.dispatch(
                "issuer",
                Command::EffectFence {
                    command: Box::new(EffectFenceCommand::SlotCommand {
                        lease,
                        command: Box::new(EffectFenceCommand::Dispatch { effect: guarded })
                    })
                }
            ),
            Response::Accepted { .. }
        ));
        assert!(matches!(
            broker.dispatch("provider", Command::Poll {}),
            Response::Request { .. }
        ));
    }
    let saved = store.handle(
        "pairing",
        protocol_slot_state::read_request(scope.source.slot().unwrap())
            .unwrap()
            .payload(),
    );
    let claim = protocol_slot_state::read(scope.source.slot().unwrap(), &saved)
        .unwrap()
        .1
        .active
        .unwrap();
    assert_eq!(claim.scope, scope);
    if replace_after_claim {
        assert!(claim.entries.is_empty());
    } else {
        assert_eq!(claim.entries.len(), 1);
        assert_eq!(claim.entries[0].identity.operation_id, operation_id);
        assert_eq!(claim.entries[0].provider_incarnation.id, "a".repeat(32));
    }
    let Response::Result { payload } = broker.dispatch(
        "pairing",
        Command::EffectFence {
            command: Box::new(EffectFenceCommand::SlotStatus {
                slot: scope.source.slot().unwrap(),
            }),
        },
    ) else {
        panic!("slot status")
    };
    assert_eq!(
        payload["active"].is_null(),
        cancel_after_write || replace_after_claim
    );
    let settings = crate::protocol::Settings {
        administrator_principals: ["console".into()].into(),
        transport_principals: ["nvhttp".into()].into(),
        effect_authority: pairing.effect_authority.clone(),
        protocol_state: Some(crate::protocol::ProtocolStateSelection {
            owner: "pairing".into(),
            provider: "state".into(),
            capability: amaterasu_module_sdk::protocol_state::DURABLE_CAS_CAPABILITY.into(),
        }),
    };
    settings.validate().unwrap();
    drop(pairing);
    drop(store);
    let reopened = TestStateStore::open(path.clone()).unwrap();
    let saved = reopened.handle(
        "pairing",
        protocol_slot_state::read_request(scope.source.slot().unwrap())
            .unwrap()
            .payload(),
    );
    let recovered = protocol_slot_state::read(scope.source.slot().unwrap(), &saved)
        .unwrap()
        .1
        .active
        .unwrap();
    assert_eq!(recovered.scope, scope);
    let mut restarted = super::super::Pipeline::new(settings);
    let epoch = restarted.effect_gate.as_ref().unwrap().epoch().to_owned();
    assert_eq!(
        restarted.apply_effect_request(
            "issuer",
            "issuer",
            json!({"action":"recover","epoch":epoch,"owner_epoch":"owner"}),
            super::super::tests::now()
        ),
        Err("protocol_reconciliation_required")
    );
    drop(reopened);
    std::fs::remove_dir_all(path).unwrap();
}

#[test]
fn broker_selection_discovery_is_authority_only_and_does_not_grant_calls() {
    let mut broker = broker();
    let query = || Command::EffectFence {
        command: Box::new(EffectFenceCommand::SelectedActionApproval {
            method: ProtectedMethod {
                service: "example.effects/1".into(),
                method: "apply".into(),
                interface_major: 1,
            },
        }),
    };
    assert!(
        matches!(broker.dispatch("issuer", query()), Response::Error { code, .. } if code == "permission_denied")
    );
    assert!(
        matches!(broker.dispatch("console", Command::EffectFence { command:Box::new(EffectFenceCommand::SelectedStateProvider {}) }), Response::Error { code, .. } if code == "permission_denied")
    );
    let Response::Result { payload } = broker.dispatch("pairing", query()) else {
        panic!("selected action")
    };
    assert_eq!(payload["approval"]["provider"], "provider");
    assert_eq!(payload["provider_incarnation"]["id"], "a".repeat(32));
    assert!(
        matches!(broker.dispatch("pairing", Command::QueryApiApproval { service:"example.effects/1".into(), method:"apply".into(), interface_major:1, min_minor:0 }), Response::Error { code, .. } if code == "permission_denied")
    );
}

#[test]
fn trust_mutation_fences_real_broker_effect_and_waits_for_retirement() {
    run_real_broker_fence(false);
}

#[test]
fn explicit_protocol_source_fence_checks_owner_and_waits_for_retirement() {
    run_real_broker_fence(true);
}

fn run_real_broker_fence(explicit_source: bool) {
    use amaterasu_module_sdk::broker_effect_fence::{GuardedEffect, SelectedEffectMethod};
    let path = directory();
    let store = TestStateStore::open(path.clone()).unwrap();
    let mut broker = broker();
    let (mut pairing, _, _, epoch, _) = super::super::tests::authorized_protocol_for_test();
    let mut retained_scope = scope();
    retained_scope.authority_epoch = epoch;
    let parent = open_parent(&mut broker);
    let mut claim =
        ClaimJob::new(parent, retained_scope.clone(), "issuer".into(), binding()).unwrap();
    let lease = loop {
        match claim
            .advance(true, |command| Ok(broker.dispatch("pairing", command)))
            .unwrap()
        {
            Outcome::Pending => {}
            Outcome::Claimed { lease, .. } => break lease,
            Outcome::Failed(code) => panic!("{code}"),
        }
        if let Some(state) = service_one(&mut broker, &store) {
            finish_state(&mut broker, state);
        }
    };
    assert!(matches!(
        broker.dispatch(
            "pairing",
            Command::EffectFence {
                command: Box::new(EffectFenceCommand::OpenSlot {
                    lease: lease.clone(),
                    scope: retained_scope.clone(),
                    retired_through: 0
                })
            }
        ),
        Response::Ok
    ));
    let Response::Result { payload } = broker.dispatch(
        "pairing",
        Command::EffectFence {
            command: Box::new(EffectFenceCommand::SelectedMethod {
                method: ProtectedMethod {
                    service: "example.effects/1".into(),
                    method: "apply".into(),
                    interface_major: 1,
                },
            }),
        },
    ) else {
        panic!("action selection")
    };
    let selected: SelectedEffectMethod = serde_json::from_value(payload).unwrap();
    let effect = GuardedEffect {
        scope: retained_scope.clone(),
        sequence: 1,
        capability: "example.control/1".into(),
        service: "example.effects/1".into(),
        method: "apply".into(),
        interface_major: 1,
        interface_minor: 0,
        schema_revision: 1,
        payload_sha256: "a".repeat(64),
        payload: json!({}),
        timeout_ms: 5_000,
        operation_id: protocol_slot_state::operation_id("issuer", &retained_scope, 1).unwrap(),
        parent: None,
    };
    let Response::Result { payload } = broker.dispatch(
        "pairing",
        Command::EffectFence {
            command: Box::new(EffectFenceCommand::SelectedCleanupMethod {
                method: ProtectedMethod {
                    service: "example.effects/1".into(),
                    method: "cleanup".into(),
                    interface_major: 1,
                },
            }),
        },
    ) else {
        panic!("cleanup selection")
    };
    let entry = protocol_slot_state::Entry {
        identity: effect.identity("provider"),
        issued_lease: lease.clone(),
        provider_incarnation: selected.provider_incarnation.clone().unwrap(),
        selected_method: selected.clone(),
        cleanup_method: amaterasu_module_sdk::extension_api::MethodRef {
            service: "example.effects/1".into(),
            method: "cleanup".into(),
            interface_major: 1,
            interface_minor: 0,
            schema_revision: 1,
        },
        cleanup_approval: serde_json::from_value(payload["approval"].clone()).unwrap(),
        cleanup: amaterasu_module_sdk::protocol_state::CleanupReference::new("b".repeat(64))
            .unwrap(),
        cleanup_confirmed: false,
    };
    let slot_read = store.handle(
        "pairing",
        protocol_slot_state::read_request(lease.slot)
            .unwrap()
            .payload(),
    );
    let issue = protocol_slot_state::prepare_issue(&slot_read, lease.slot, &lease, entry).unwrap();
    let Response::Accepted { id: write_id } = broker.dispatch(
        "pairing",
        Command::Request {
            service: STATE_SERVICE.into(),
            payload: issue.request().payload(),
            timeout_ms: 5_000,
            operation_id: issue.state_operation_id().into(),
            parent: None,
        },
    ) else {
        panic!("journal write denied")
    };
    let saved = service_one(&mut broker, &store).unwrap();
    assert_eq!(saved.0, write_id);
    finish_state(&mut broker, saved);
    let Response::Result { payload } =
        broker.dispatch("pairing", Command::TakeResult { request: write_id })
    else {
        panic!("journal write pending")
    };
    issue.confirmed(payload).unwrap();
    assert!(matches!(
        broker.dispatch("pairing", Command::ReleaseResult { request: write_id }),
        Response::Ok
    ));
    assert!(matches!(
        broker.dispatch(
            "pairing",
            Command::EffectFence {
                command: Box::new(EffectFenceCommand::SlotCommand {
                    lease: lease.clone(),
                    command: Box::new(EffectFenceCommand::ArmSelected {
                        effect: effect.clone(),
                        expected: Box::new(selected)
                    })
                })
            }
        ),
        Response::Ok
    ));
    let Response::Accepted { id: effect_id } = broker.dispatch(
        "issuer",
        Command::EffectFence {
            command: Box::new(EffectFenceCommand::SlotCommand {
                lease: lease.clone(),
                command: Box::new(EffectFenceCommand::Dispatch {
                    effect: effect.clone(),
                }),
            }),
        },
    ) else {
        panic!("effect admission")
    };
    assert!(
        matches!(broker.dispatch("provider", Command::Poll {}), Response::Request { id, .. } if id == effect_id)
    );
    pairing.retained_protocol_claim = Some((lease.clone(), Box::new(retained_scope.clone())));
    pairing.protocol_claim_recovery_required = true;
    let caller = if explicit_source { "issuer" } else { "console" };
    let request = |operation: &str| Command::Request {
        service: if explicit_source {
            "amaterasu.pairing-effects/1"
        } else {
            "amaterasu.pairing/1"
        }
        .into(),
        payload: if explicit_source {
            json!({"action":"fence_protocol_source","version":1,"epoch":retained_scope.authority_epoch,"issuer_epoch":retained_scope.issuer_epoch,"source":retained_scope.source})
        } else {
            json!({"action":"set_permission_mode","enabled":true,"expected_revision":0})
        },
        timeout_ms: 30_000,
        operation_id: operation.into(),
        parent: None,
    };
    let Response::Accepted { id: denied_id } =
        broker.dispatch("untrusted", request("trust.denied"))
    else {
        panic!("untrusted request not delivered")
    };
    pairing
        .tick_at(super::super::tests::now(), |command| {
            Ok(broker.dispatch("pairing", command))
        })
        .unwrap();
    assert!(
        matches!(broker.dispatch("untrusted", Command::TakeResult { request:denied_id }), Response::Result { payload } if payload["code"] == "forbidden")
    );
    if explicit_source {
        let Command::Request {
            service,
            mut payload,
            timeout_ms,
            parent,
            ..
        } = request("ignored")
        else {
            unreachable!()
        };
        payload["issuer_epoch"] = json!("forged");
        let Response::Accepted { id } = broker.dispatch(
            caller,
            Command::Request {
                service,
                payload,
                timeout_ms,
                operation_id: "fence.forged".into(),
                parent,
            },
        ) else {
            panic!("forged request denied too early")
        };
        pairing
            .tick_at(super::super::tests::now(), |command| {
                Ok(broker.dispatch("pairing", command))
            })
            .unwrap();
        assert!(
            matches!(broker.dispatch(caller, Command::TakeResult { request:id }), Response::Result { payload } if payload["code"] == "source_denied")
        );
    }
    let Response::Accepted { id: canceled_id } = broker.dispatch(caller, request("trust.canceled"))
    else {
        panic!("trust request")
    };
    pairing
        .tick_at(super::super::tests::now(), |command| {
            let result = broker.dispatch("pairing", command);
            if matches!(&result, Response::Request { id, .. } if *id == canceled_id) {
                assert!(matches!(
                    broker.dispatch(
                        caller,
                        Command::Cancel {
                            request: canceled_id
                        }
                    ),
                    Response::Ok
                ));
            }
            Ok(result)
        })
        .unwrap();
    let lookup = |broker: &mut Broker| {
        broker.dispatch(
            "pairing",
            Command::EffectFence {
                command: Box::new(EffectFenceCommand::SlotCommand {
                    lease: lease.clone(),
                    command: Box::new(EffectFenceCommand::Lookup {
                        effect: effect.clone(),
                    }),
                }),
            },
        )
    };
    assert!(
        matches!(lookup(&mut broker), Response::Result { payload } if payload["status"] == "admitted")
    );
    let Response::Accepted { id } = broker.dispatch(caller, request("trust.active")) else {
        panic!("trust request")
    };
    pairing
        .tick_at(super::super::tests::now(), |command| {
            Ok(broker.dispatch("pairing", command))
        })
        .unwrap();
    let Response::Result { payload } = broker.dispatch(caller, Command::TakeResult { request: id })
    else {
        panic!("fence response")
    };
    if explicit_source {
        assert_eq!(payload["state"], "fenced");
        let Response::Accepted { id: retry_id } = broker.dispatch(caller, request("fence.retry"))
        else {
            panic!("fence retry")
        };
        pairing
            .tick_at(super::super::tests::now(), |command| {
                Ok(broker.dispatch("pairing", command))
            })
            .unwrap();
        assert!(
            matches!(broker.dispatch(caller, Command::TakeResult { request:retry_id }), Response::Result { payload } if payload["state"] == "fenced")
        );
    } else {
        assert_eq!(payload["code"], "protocol_reconciliation_required");
    }
    assert!(
        matches!(lookup(&mut broker), Response::Result { payload } if payload["status"] == "fenced")
    );
    assert!(
        matches!(broker.dispatch("pairing", Command::EffectFence { command:Box::new(EffectFenceCommand::CloseReady { lease:lease.clone(), scope:retained_scope, retired_through:0 }) }), Response::Error { code, .. } if code == "effect_pending")
    );
    assert!(
        matches!(broker.dispatch("issuer", Command::EffectFence { command:Box::new(EffectFenceCommand::SlotCommand { lease:lease.clone(), command:Box::new(EffectFenceCommand::Dispatch { effect }) }) }), Response::Error { code, .. } if code == "effect_fenced")
    );
    assert!(matches!(
        broker.dispatch("state", Command::Poll {}),
        Response::Pending
    ));
    drop(store);
    std::fs::remove_dir_all(path).unwrap();
}

#[test]
fn startup_enumeration_fences_orphan_broker_source_without_inferring_empty_state() {
    run_orphan_recovery(false);
}

#[test]
fn selected_owner_can_fence_broker_source_before_startup_scan_finishes() {
    run_orphan_recovery(true);
}

fn run_orphan_recovery(explicit_fence: bool) {
    use amaterasu_module_sdk::broker_effect_fence::{GuardedEffect, SelectedEffectMethod};
    let path = directory();
    let store = TestStateStore::open(path.clone()).unwrap();
    let mut broker = broker();
    let active_scope = scope();
    let slot = active_scope.source.slot().unwrap();
    let Response::Result { payload } = broker.dispatch(
        "pairing",
        Command::EffectFence {
            command: Box::new(EffectFenceCommand::SlotStatus { slot }),
        },
    ) else {
        panic!("slot")
    };
    let lease: SourceSlotLease = serde_json::from_value(payload["lease"].clone()).unwrap();
    assert!(matches!(
        broker.dispatch(
            "pairing",
            Command::EffectFence {
                command: Box::new(EffectFenceCommand::OpenSlot {
                    lease: lease.clone(),
                    scope: active_scope.clone(),
                    retired_through: 0
                })
            }
        ),
        Response::Ok
    ));
    let Response::Result { payload } = broker.dispatch(
        "pairing",
        Command::EffectFence {
            command: Box::new(EffectFenceCommand::SelectedMethod {
                method: ProtectedMethod {
                    service: "example.effects/1".into(),
                    method: "apply".into(),
                    interface_major: 1,
                },
            }),
        },
    ) else {
        panic!("selected action")
    };
    let selected: SelectedEffectMethod = serde_json::from_value(payload).unwrap();
    let effect = GuardedEffect {
        scope: active_scope.clone(),
        sequence: 1,
        capability: "example.control/1".into(),
        service: "example.effects/1".into(),
        method: "apply".into(),
        interface_major: 1,
        interface_minor: 0,
        schema_revision: 1,
        payload_sha256: "a".repeat(64),
        payload: json!({}),
        timeout_ms: 5_000,
        operation_id: "orphan.effect".into(),
        parent: None,
    };
    assert!(matches!(
        broker.dispatch(
            "pairing",
            Command::EffectFence {
                command: Box::new(EffectFenceCommand::SlotCommand {
                    lease: lease.clone(),
                    command: Box::new(EffectFenceCommand::ArmSelected {
                        effect: effect.clone(),
                        expected: Box::new(selected)
                    })
                })
            }
        ),
        Response::Ok
    ));
    let Response::Accepted { id } = broker.dispatch(
        "issuer",
        Command::EffectFence {
            command: Box::new(EffectFenceCommand::SlotCommand {
                lease: lease.clone(),
                command: Box::new(EffectFenceCommand::Dispatch {
                    effect: effect.clone(),
                }),
            }),
        },
    ) else {
        panic!("effect")
    };
    assert!(
        matches!(broker.dispatch("provider", Command::Poll {}), Response::Request { id:delivered, .. } if delivered == id)
    );
    let (mut pairing, _, _, _, _) = super::super::tests::authorized_protocol_for_test();
    let method = json!({"service":"example.effects/1","method":"apply","interface_major":1,"interface_minor":0,"schema_revision":1});
    let cleanup = json!({"service":"example.effects/1","method":"cleanup","interface_major":1,"interface_minor":0,"schema_revision":1});
    let schema = |method: &Value| {
        let mut value = method.clone();
        value.as_object_mut().unwrap().remove("schema_revision");
        value["request"] = json!({"required":{},"optional":{},"allow_extra":false});
        value["response"] = json!({"required":{},"optional":{},"allow_extra":false});
        value["privilege"] = json!("user");
        value
    };
    pairing.effect_authority.as_mut().unwrap().protocol_policies=serde_json::from_value(json!([{"capability":"example.control/1","provider":"provider","message_type":21818,"method":method,"cleanup":cleanup,"method_schema":schema(&method),"cleanup_schema":schema(&cleanup),"max_payload_bytes":64,"required_permission":"view"}])).unwrap();
    pairing.protocol_state_binding = Some(binding());
    pairing.protocol_claim_recovery_required = true;
    pairing.protocol_recovery_auto = true;
    if explicit_fence {
        let mut request = json!({"action":"fence_protocol_source","version":1,"epoch":active_scope.authority_epoch,"issuer_epoch":active_scope.issuer_epoch,"source":active_scope.source});
        request["issuer_epoch"] = json!("wrong-issuer");
        let Response::Accepted { id: wrong } = broker.dispatch(
            "issuer",
            Command::Request {
                service: "amaterasu.pairing-effects/1".into(),
                payload: request.clone(),
                timeout_ms: 30_000,
                operation_id: "restart.fence.wrong".into(),
                parent: None,
            },
        ) else {
            panic!("request")
        };
        pairing
            .tick_at(super::super::tests::now(), |command| {
                Ok(broker.dispatch("pairing", command))
            })
            .unwrap();
        assert!(
            matches!(broker.dispatch("issuer", Command::TakeResult { request:wrong }), Response::Result { payload } if payload["code"] == "source_denied")
        );
        assert!(
            matches!(broker.dispatch("pairing", Command::EffectFence { command:Box::new(EffectFenceCommand::SlotCommand { lease:lease.clone(), command:Box::new(EffectFenceCommand::Lookup { effect:effect.clone() }) }) }), Response::Result { payload } if payload["status"] == "admitted")
        );
        request["issuer_epoch"] = json!(active_scope.issuer_epoch);
        let Response::Accepted { id: fence } = broker.dispatch(
            "issuer",
            Command::Request {
                service: "amaterasu.pairing-effects/1".into(),
                payload: request,
                timeout_ms: 30_000,
                operation_id: "restart.fence".into(),
                parent: None,
            },
        ) else {
            panic!("request")
        };
        pairing
            .tick_at(super::super::tests::now(), |command| {
                Ok(broker.dispatch("pairing", command))
            })
            .unwrap();
        assert!(
            matches!(broker.dispatch("issuer", Command::TakeResult { request:fence }), Response::Result { payload } if payload["state"] == "fenced")
        );
    }
    for _ in 0..300 {
        pairing
            .tick_at(super::super::tests::now(), |command| {
                Ok(broker.dispatch("pairing", command))
            })
            .unwrap();
        if let Some(state) = service_one(&mut broker, &store) {
            finish_state(&mut broker, state);
        }
        if pairing.protocol_recovery_inventory.is_some() {
            break;
        }
    }
    let inventory = pairing
        .protocol_recovery_inventory
        .as_ref()
        .expect("startup scan did not finish");
    assert!(inventory.broker_ready);
    assert!(!inventory.broker_frozen);
    assert_eq!(
        inventory.broker_sources,
        vec![(lease.clone(), active_scope)]
    );
    assert!(inventory.state.active.is_empty());
    assert!(inventory.state.pending_close.is_empty());
    assert!(pairing.protocol_claim_recovery_required);
    assert!(
        matches!(broker.dispatch("pairing", Command::EffectFence { command:Box::new(EffectFenceCommand::SlotCommand { lease, command:Box::new(EffectFenceCommand::Lookup { effect }) }) }), Response::Result { payload } if payload["status"] == "fenced")
    );
    drop(store);
    std::fs::remove_dir_all(path).unwrap();
}

#[test]
fn fresh_broker_inventory_retains_durable_claim_without_assuming_old_workers_retired() {
    let path = directory();
    let store = TestStateStore::open(path.clone()).unwrap();
    let mut old_broker = broker();
    let parent = open_parent(&mut old_broker);
    let mut claim = ClaimJob::new(parent, scope(), "issuer".into(), binding()).unwrap();
    loop {
        match claim
            .advance(true, |command| Ok(old_broker.dispatch("pairing", command)))
            .unwrap()
        {
            Outcome::Pending => {}
            Outcome::Claimed { .. } => break,
            Outcome::Failed(code) => panic!("{code}"),
        }
        if let Some(state) = service_one(&mut old_broker, &store) {
            finish_state(&mut old_broker, state);
        }
    }
    drop(old_broker);
    let mut broker = broker_with_recovery('c', false);
    // Preserve the historical State selection even after removing all generic
    // policies: its old obligations must still be discovered and quarantined.
    let settings: crate::protocol::Settings = serde_json::from_value(json!({
        "administrator_principals":["console"], "transport_principals":["nvhttp"],
        "effect_authority":{"protocol_owner":"nvhttp","effect_owner":"issuer", "protocol_policies":[]},
        "protocol_state":{"owner":"pairing","provider":"state",
            "capability":amaterasu_module_sdk::protocol_state::DURABLE_CAS_CAPABILITY}
    })).unwrap();
    settings.validate().unwrap();
    let mut pairing = super::super::Pipeline::new(settings);
    assert!(pairing.protocol_claim_recovery_required);
    let mut attempted_recovery = false;
    for _ in 0..200 {
        pairing.tick_at(super::super::tests::now(), |command| {
            if matches!(&command, Command::EffectFence { command } if matches!(**command, EffectFenceCommand::RecoverSlots { .. })) { attempted_recovery = true; }
            Ok(broker.dispatch("pairing", command))
        }).unwrap();
        if let Some(state) = service_one(&mut broker, &store) {
            finish_state(&mut broker, state);
        }
        if pairing.protocol_recovery_inventory.is_some() {
            break;
        }
    }
    let inventory = pairing
        .protocol_recovery_inventory
        .as_ref()
        .expect("startup scan did not finish");
    assert!(!inventory.broker_ready);
    assert!(!inventory.broker_frozen);
    assert!(inventory.broker_sources.is_empty());
    assert_eq!(inventory.state.active.len(), 1);
    assert_eq!(inventory.state.active[0].scope, scope());
    assert!(pairing.protocol_claim_recovery_required);
    assert!(!attempted_recovery);
    let Response::Result { payload } = broker.dispatch(
        "pairing",
        Command::EffectFence {
            command: Box::new(EffectFenceCommand::SlotStatus { slot: 0 }),
        },
    ) else {
        panic!("slot status")
    };
    assert_eq!(payload["ready"], false);
    drop(store);
    std::fs::remove_dir_all(path).unwrap();
}
