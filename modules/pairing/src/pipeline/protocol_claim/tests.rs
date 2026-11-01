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
    let method = ApiMethod {
        service: "example.effects/1".into(),
        method: "apply".into(),
        interface_major: 1,
        interface_minor: 0,
        request: ApiSchema {
            required: Default::default(),
            optional: Default::default(),
            allow_extra: false,
        },
        response: ApiSchema {
            required: Default::default(),
            optional: Default::default(),
            allow_extra: false,
        },
        privilege: ApiPrivilege::User,
    };
    config.api_methods.push(ApiMethodApproval {
        provider: "provider".into(),
        callers: ["issuer".into()].into(),
        method: method.clone(),
    });
    config.effect_fence = Some(EffectFencePolicy {
        authority: "pairing".into(),
        issuer: "issuer".into(),
        source_slots: true,
        methods: vec![ProtectedMethod {
            service: "example.effects/1".into(),
            method: "apply".into(),
            interface_major: 1,
        }],
        cleanup_methods: vec![],
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
    assert_eq!(payload["code"], "authority_draining");
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
