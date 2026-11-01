use super::super::worker::actual_pairing_ledger::EffectGate;
use super::*;
use amaterasu_contracts::{Grants, RuntimeConfig};
use amaterasu_module_sdk::{
    Command, Response,
    effect_authority::{FeedbackCapability, FeedbackDirection, Source, StreamScope},
};
use amaterasu_runtime::Broker;

fn apply(gate: &mut EffectGate, p: &Value) -> Result<Value, &'static str> {
    let epoch = p["epoch"].as_str().unwrap_or(gate.epoch()).to_owned();
    let op = || p["operation_id"].as_str().unwrap();
    match p["action"].as_str().unwrap() {
        "status" => Ok(json!({"ok":true,"epoch":gate.epoch(),"owner_epoch":gate.owner_epoch(),"startup_recovery_required":gate.startup_recovery_required(),"fence_generation":gate.fence_generation(),"fenced":gate.fenced(),"drain_required":gate.drain_required(),"in_flight":gate.in_flight(),"bound_sources":gate.bound_sources(),"pending_source_retirements":gate.pending_source_retirements()})),
        "fence_effect_owner_restart" => gate.fence_owner_restart(&epoch,p["owner_epoch"].as_str().unwrap(),op()).map(|(generation,count)|json!({"ok":true,"version":1,"epoch":epoch,"owner_epoch":p["owner_epoch"],"operation_id":op(),"fence_generation":generation,"count":count})),
        "read_effect_owner_restart" => gate.owner_restart_record(&epoch,p["owner_epoch"].as_str().unwrap(),op(),p["index"].as_u64().unwrap() as u32).map(|(record,count)|json!({"ok":true,"version":1,"epoch":epoch,"owner_epoch":p["owner_epoch"],"operation_id":op(),"index":p["index"],"count":count,"record":record})),
        "lookup_current_source_retirement" => gate.lookup_current_source_retirement(&epoch,&serde_json::from_value(p["source"].clone()).unwrap(),&serde_json::from_value(p["scope"].clone()).unwrap()).map(|(operation,complete)|json!({"ok":true,"epoch":epoch,"source":p["source"],"scope":p["scope"],"operation_id":operation,"state":if complete {"retired"} else {"fenced"}})),
        "fence_source" => gate.fence_source(&epoch,&serde_json::from_value(p["source"].clone()).unwrap(),&serde_json::from_value(p["scope"].clone()).unwrap(),op()).map(|done|json!({"ok":true,"epoch":epoch,"source":p["source"],"scope":p["scope"],"operation_id":op(),"state":if done {"retired"} else {"fenced"}})),
        "complete_stream_lease" => gate.complete_stream_lease(&epoch,op(),p["ticket"].as_str().unwrap(),&serde_json::from_value(p["retirement"].clone()).unwrap()).map(|()|json!({"ok":true,"epoch":epoch,"operation_id":op()})),
        "acknowledge" => gate.acknowledge(&epoch,op(),p["ticket"].as_str().unwrap()).map(|()|json!({"ok":true,"epoch":epoch,"operation_id":op()})),
        "complete_source_retirement" => gate.complete_source_retirement(&epoch,&serde_json::from_value(p["source"].clone()).unwrap(),&serde_json::from_value(p["scope"].clone()).unwrap(),op(),serde_json::from_value::<Option<StreamRetirement>>(p["retirement"].clone()).unwrap().as_ref()).map(|()|json!({"ok":true,"epoch":epoch,"source":p["source"],"scope":p["scope"],"operation_id":op(),"state":"retired"})),
        "confirm_effect_owner_restart" => gate.confirm_owner_restart_record(&epoch,p["owner_epoch"].as_str().unwrap(),op(),p["index"].as_u64().unwrap() as u32,&serde_json::from_value(p["retirement"].clone()).unwrap()).map(|()|json!({"ok":true,"version":1,"epoch":epoch,"owner_epoch":p["owner_epoch"],"operation_id":op(),"index":p["index"]})),
        "drain" => gate.acknowledge_drain(&epoch,p["fence_generation"].as_u64().unwrap()).map(|()|json!({"ok":true,"epoch":epoch,"fence_generation":p["fence_generation"]})),
        "recover" => { gate.require_epoch(&epoch)?; gate.recover(p["owner_epoch"].as_str().unwrap()).map(|()|json!({"ok":true,"epoch":gate.epoch()})) },
        other => panic!("unexpected pairing action {other}"),
    }
}

fn source(slot: u16) -> Source {
    Source {
        version: 1,
        id: "a".repeat(32),
        generation: 2,
        host_epoch: "host".into(),
        session_id: "session".into(),
        stream_id: "stream".into(),
        stream_generation: 2,
        device: format!("device-{slot}"),
        device_generation: 1,
        effect_owner_epoch: "owner".into(),
    }
}
fn scope(slot: u16, stream: u64, epoch: &str) -> StreamScope {
    StreamScope {
        capabilities: [FeedbackCapability {
            schema: "org.amaterasu.feedback.rumble".into(),
            version: 1,
            direction: FeedbackDirection::ToClient,
        }]
        .into(),
        controller_slot: slot,
        transport_service: "amaterasu.control-transport/1".into(),
        transport_stream: stream,
        transport_epoch: epoch.into(),
    }
}

fn run_restart(completed_lease: bool, corrupt_receipt: bool, retired_without_lease: Option<u16>) {
    let receiver = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    receiver.set_nonblocking(true).unwrap();
    let mut control = amaterasu_control_transport::test_support::ControlFixture::new("effect");
    let open = control.open("effect").unwrap();
    let stream = open["stream"].as_u64().unwrap();
    let provider_epoch = open["retirement_epoch"].as_str().unwrap().to_owned();
    control
        .stage_feedback("effect", stream, receiver.local_addr().unwrap())
        .unwrap();
    let mut gate = EffectGate::new().unwrap();
    gate.recover("owner").unwrap();
    let old_epoch = gate.epoch().to_owned();
    for slot in [1, 2] {
        let source = source(slot);
        let scope = scope(slot, stream, &provider_epoch);
        gate.bind_scoped(source.clone(), Some(scope.clone()))
            .unwrap();
        gate.admit_stream_lease(&source, &format!("lease-{slot}"), u64::from(slot), scope)
            .unwrap();
    }
    if let Some(slot) = retired_without_lease {
        let second = control.handle("effect", json!({"action":"open","peer_ip":"127.0.0.1",
            "connect_data":18,"key_hex":"1123456789abcdef0123456789abcdef","operation_id":"second-open"})).unwrap();
        let other_stream = second["stream"].as_u64().unwrap();
        let other_scope = scope(slot, other_stream, &provider_epoch);
        let other_source = source(slot);
        gate.bind_scoped(other_source.clone(), Some(other_scope.clone()))
            .unwrap();
        gate.fence_source(&old_epoch, &other_source, &other_scope, "already-retired")
            .unwrap();
        gate.complete_source_retirement(
            &old_epoch,
            &other_source,
            &other_scope,
            "already-retired",
            None,
        )
        .unwrap();
    }
    if completed_lease {
        let operation = format!("feedback-retire-{provider_epoch}-{stream}");
        let raw = control.handle("effect",json!({"action":"retire","stream":stream,"operation_id":operation,"provider_epoch":provider_epoch})).unwrap();
        let receipt = StreamRetirement {
            ok: true,
            service: "amaterasu.control-transport/1".into(),
            stream,
            provider_epoch: provider_epoch.clone(),
            operation_id: operation,
            scope: raw["scope"].as_str().unwrap().into(),
            state: raw["state"].as_str().unwrap().into(),
        };
        let (ticket, _) = gate
            .lookup_stream_lease(&source(1), "lease-1", &scope(1, stream, &provider_epoch))
            .unwrap();
        let ticket = ticket.to_owned();
        gate.complete_stream_lease(&old_epoch, "lease-1", &ticket, &receipt)
            .unwrap();
    }
    let mut config = RuntimeConfig::default();
    config.principals.insert(
        "effect".into(),
        Grants {
            call: [
                effect_authority::SERVICE.into(),
                "amaterasu.control-transport/1".into(),
            ]
            .into(),
            ..Default::default()
        },
    );
    config.principals.insert(
        "pairing".into(),
        Grants {
            provide: [effect_authority::SERVICE.into()].into(),
            ..Default::default()
        },
    );
    config.principals.insert(
        "control".into(),
        Grants {
            provide: ["amaterasu.control-transport/1".into()].into(),
            ..Default::default()
        },
    );
    let mut broker = Broker::new(config).unwrap();
    for (principal, service) in [
        ("pairing", effect_authority::SERVICE),
        ("control", "amaterasu.control-transport/1"),
    ] {
        assert!(matches!(
            broker.dispatch(
                principal,
                Command::Register {
                    service: service.into()
                }
            ),
            Response::Ok
        ));
    }
    let mut inbox = super::super::Inbox::new(true, "amaterasu.control-transport/1".into());
    let mut records = BTreeMap::new();
    let mut done = false;
    let mut lost = false;
    let mut retired = 0;
    let mut lose_request = None;
    for _ in 0..5000 {
        inbox.advance(&mut records, None, &mut |cmd| {
            let acknowledgment =
                matches!(&cmd, Command::Request {payload,..} if payload["action"] == "acknowledge");
            let result_request = match &cmd {
                Command::TakeResult { request } => Some(*request),
                _ => None,
            };
            let reply = broker.dispatch("effect", cmd);
            if acknowledgment
                && let Response::Accepted { id } = &reply
                && lose_request.is_none()
            {
                lose_request = Some(*id);
            }
            if result_request == lose_request && matches!(reply, Response::Result { .. }) && !lost {
                lost = true;
                Err("lost acknowledgment reply".into())
            } else {
                Ok(reply)
            }
        });
        done = !inbox.restart_required;
        for provider in ["pairing", "control"] {
            let Response::Request {
                id,
                caller,
                initiator,
                payload,
                ..
            } = broker.dispatch(provider, Command::Poll {})
            else {
                continue;
            };
            assert_eq!((caller.as_str(), initiator.as_str()), ("effect", "effect"));
            if let Some(slot) = retired_without_lease
                && payload["action"] == "complete_source_retirement"
            {
                assert_ne!(
                    payload["source"]["device"],
                    format!("device-{slot}"),
                    "a completed unleased source must not be completed again"
                );
            }
            let result = if provider == "control" {
                retired += 1;
                control.handle("effect", payload).map(|mut receipt| {
                    if corrupt_receipt && retired == 1 {
                        receipt["provider_epoch"] = json!("f".repeat(32));
                    }
                    receipt
                })
            } else {
                apply(&mut gate, &payload)
            };
            assert!(matches!(
                broker.dispatch(
                    provider,
                    Command::Complete {
                        request: id,
                        payload: result.unwrap_or_else(|code| json!({"ok":false,"code":code}))
                    }
                ),
                Response::Ok
            ));
        }
        if done {
            break;
        }
    }
    assert!(done, "restart remained fenced");
    assert!(lost);
    assert_eq!(
        retired,
        1 + usize::from(corrupt_receipt) + usize::from(retired_without_lease.is_some())
    );
    assert_ne!(gate.epoch(), old_epoch);
    assert_eq!(gate.in_flight(), 0);
    control.tick();
    let mut buf = [0; 2048];
    assert_eq!(
        receiver.recv_from(&mut buf).unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[test]
fn restart_drains_shared_transport_with_lost_reply() {
    run_restart(false, false, None);
}
#[test]
fn restart_acknowledges_a_previously_completed_lease() {
    run_restart(true, false, None);
}
#[test]
fn restart_rejects_mismatched_provider_receipt_and_retries() {
    run_restart(false, true, None);
}
#[test]
fn restart_confirms_previously_retired_unleased_source_with_real_provider_receipt() {
    run_restart(false, false, Some(0));
    run_restart(false, false, Some(3));
    run_restart(false, true, Some(0));
    run_restart(false, true, Some(3));
}
#[test]
fn pairing_restart_without_old_owner_remains_closed() {
    let mut gate = EffectGate::new().unwrap();
    assert!(gate.startup_recovery_required());
    assert!(gate.owner_epoch().is_none());
    let mut config = RuntimeConfig::default();
    config.principals.insert(
        "effect".into(),
        Grants {
            call: [effect_authority::SERVICE.into()].into(),
            ..Default::default()
        },
    );
    config.principals.insert(
        "pairing".into(),
        Grants {
            provide: [effect_authority::SERVICE.into()].into(),
            ..Default::default()
        },
    );
    let mut broker = Broker::new(config).unwrap();
    assert!(matches!(
        broker.dispatch(
            "pairing",
            Command::Register {
                service: effect_authority::SERVICE.into()
            }
        ),
        Response::Ok
    ));
    let mut restart = Restart::new("amaterasu.control-transport/1".into()).unwrap();
    for _ in 0..50 {
        assert!(!restart.advance(&mut |command| Ok(broker.dispatch("effect", command))));
        if let Response::Request { id, payload, .. } = broker.dispatch("pairing", Command::Poll {})
        {
            assert_eq!(payload["action"], "status");
            let response = apply(&mut gate, &payload).unwrap();
            assert!(matches!(
                broker.dispatch(
                    "pairing",
                    Command::Complete {
                        request: id,
                        payload: response
                    }
                ),
                Response::Ok
            ));
        }
    }
    assert!(gate.fenced());
    assert!(gate.startup_recovery_required());
    assert!(gate.owner_epoch().is_none());
}

#[test]
fn recheck_requires_the_same_source_and_retirement_operation() {
    let mut restart = Restart::new("amaterasu.control-transport/1".into()).unwrap();
    restart.epoch = "e".repeat(32);
    let source = source(1);
    let scope = scope(1, 7, &"c".repeat(32));
    restart.records.push(SourceRecord {
        source: source.clone(),
        scope: scope.clone(),
        lease: None,
        transport_retirement: None,
        source_retirement: None,
    });
    restart.source_operations.push(Some("expected".into()));
    restart.source_retired.push(false);
    restart.phase = Phase::RecheckSource(0);
    let reply = |operation: &str, state: &str| {
        json!({"ok":true,"epoch":"e".repeat(32),
        "source":source,"scope":scope,"operation_id":operation,"state":state})
    };
    assert!(!restart.handle_response(reply("foreign", "retired")));
    assert!(matches!(restart.phase, Phase::RecheckSource(0)));
    assert!(!restart.handle_response(reply("expected", "fenced")));
    assert!(matches!(restart.phase, Phase::RecheckSource(0)));
    assert!(!restart.handle_response(reply("expected", "retired")));
    assert!(restart.source_retired[0]);
    assert!(matches!(restart.phase, Phase::Confirm(0)));
}
