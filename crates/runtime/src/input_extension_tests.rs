use crate::Broker;
use amaterasu_contracts::{Command, Grants, Response, RuntimeConfig, SelectedService};
use amaterasu_module_sdk::{
    input::extensions::{
        Capability, Direction, Endpoint, ExtensionAdapter, MAX_PAYLOAD_BYTES, Message, Principal,
        Route, RoutePolicy, SERVICE, SubmitError,
    },
    requests::IncomingRequest,
};
use serde_json::{Value, json};
use std::collections::BTreeSet;

#[derive(Default)]
struct FakeEndpoint {
    messages: Vec<Message>,
    busy: bool,
}
impl Endpoint for FakeEndpoint {
    fn supports(&self, capability: &Capability) -> bool {
        *capability == attributes() || *capability == speaker()
    }
    fn submit(&mut self, message: &Message) -> Result<(), SubmitError> {
        if self.busy || self.messages.len() == 4 {
            return Err(SubmitError::Busy);
        }
        self.messages.push(message.clone());
        Ok(())
    }
}

#[derive(Default)]
struct OtherEndpoint(Vec<Message>);
impl Endpoint for OtherEndpoint {
    fn supports(&self, capability: &Capability) -> bool {
        *capability == attributes() || *capability == speaker()
    }
    fn submit(&mut self, message: &Message) -> Result<(), SubmitError> {
        if self.0.len() == 4 {
            return Err(SubmitError::Busy);
        }
        self.0.push(message.clone());
        Ok(())
    }
}

fn route() -> Route {
    Route {
        host_epoch: "host-epoch".into(),
        session: "session-a".into(),
        source: "client-a".into(),
        device: "pad-1".into(),
        generation: 7,
    }
}
fn capability(name: &str, direction: Direction) -> Capability {
    Capability {
        schema: name.into(),
        version: 1,
        direction,
    }
}
fn attributes() -> Capability {
    capability("example.steam.attributes", Direction::ToDevice)
}
fn speaker() -> Capability {
    capability("example.controller.speaker", Direction::ToClient)
}
fn principal(name: &str) -> Principal {
    Principal {
        caller: name.into(),
        initiator: name.into(),
    }
}
fn policy() -> RoutePolicy {
    let offer: BTreeSet<_> = [attributes(), speaker()].into();
    RoutePolicy {
        route: route(),
        provider_offer: offer.clone(),
        adapter_offer: offer.clone(),
        required: offer,
        to_device: vec![principal("protocol")],
        to_client: vec![principal("feedback")],
    }
}
fn broker(provider: &str) -> Broker {
    let mut config = RuntimeConfig::default();
    for name in ["protocol", "feedback", "intruder"] {
        config.principals.insert(
            name.into(),
            Grants {
                call: [SERVICE.into()].into(),
                ..Default::default()
            },
        );
    }
    config.principals.insert(
        provider.into(),
        Grants {
            provide: [SERVICE.into()].into(),
            ..Default::default()
        },
    );
    config.service_selections.broker.insert(
        SERVICE.into(),
        Some(SelectedService {
            provider: provider.into(),
            contract: SERVICE.into(),
        }),
    );
    let mut broker = Broker::new(config).unwrap();
    assert!(matches!(
        broker.dispatch(
            provider,
            Command::Register {
                service: SERVICE.into()
            }
        ),
        Response::Ok
    ));
    broker
}
fn call<E: Endpoint>(
    broker: &mut Broker,
    adapter: &mut ExtensionAdapter<E>,
    provider: &str,
    caller: &str,
    message: Message,
    operation: &str,
) -> Value {
    let id = match broker.dispatch(
        caller,
        Command::Request {
            service: SERVICE.into(),
            payload: serde_json::to_value(message).unwrap(),
            timeout_ms: 5_000,
            operation_id: operation.into(),
            parent: None,
        },
    ) {
        Response::Accepted { id } => id,
        other => panic!("request admission failed: {other:?}"),
    };
    let incoming = match broker.dispatch(provider, Command::Poll {}) {
        Response::Request {
            id: delivered,
            caller,
            initiator,
            service,
            payload,
            remaining_ms,
        } => {
            assert_eq!(id, delivered);
            IncomingRequest {
                id,
                caller,
                initiator,
                service,
                payload,
                remaining_ms,
            }
        }
        other => panic!("no provider request: {other:?}"),
    };
    let response = adapter.dispatch(&incoming);
    assert!(matches!(
        broker.dispatch(
            provider,
            Command::Complete {
                request: id,
                payload: response.clone()
            }
        ),
        Response::Ok
    ));
    assert!(
        matches!(broker.dispatch(caller, Command::TakeResult { request: id }), Response::Result { payload } if payload == response)
    );
    assert!(matches!(
        broker.dispatch(caller, Command::ReleaseResult { request: id }),
        Response::Ok
    ));
    response
}
fn message(capability: Capability, sequence: u64, payload: Vec<u8>) -> Message {
    Message {
        route: route(),
        capability,
        sequence,
        payload,
    }
}

fn exercise_provider<E: Endpoint>(
    mut endpoint: ExtensionAdapter<E>,
    provider: &str,
) -> ExtensionAdapter<E> {
    let mut broker = broker(provider);
    let attributes_data = vec![1, 2, 3, 4, 5];
    let speaker_data = vec![42; 960];
    assert_eq!(
        call(
            &mut broker,
            &mut endpoint,
            provider,
            "protocol",
            message(attributes(), 1, attributes_data),
            "attributes-1"
        ),
        json!({"ok":true,"accepted":true})
    );
    assert_eq!(
        call(
            &mut broker,
            &mut endpoint,
            provider,
            "feedback",
            message(speaker(), 1, speaker_data),
            "speaker-1"
        ),
        json!({"ok":true,"accepted":true})
    );
    assert_eq!(
        call(
            &mut broker,
            &mut endpoint,
            provider,
            "intruder",
            message(attributes(), 2, vec![]),
            "wrong-caller"
        )["error"]["code"],
        "unauthorized"
    );
    let mut wrong = message(attributes(), 2, vec![]);
    wrong.route.source = "client-b".into();
    assert_eq!(
        call(
            &mut broker,
            &mut endpoint,
            provider,
            "protocol",
            wrong,
            "wrong-source"
        )["error"]["code"],
        "unauthorized"
    );
    let mut stale = message(attributes(), 2, vec![]);
    stale.route.generation -= 1;
    assert_eq!(
        call(
            &mut broker,
            &mut endpoint,
            provider,
            "protocol",
            stale,
            "old-generation"
        )["error"]["code"],
        "unauthorized"
    );
    assert_eq!(
        call(
            &mut broker,
            &mut endpoint,
            provider,
            "protocol",
            message(
                capability("example.unknown", Direction::ToDevice),
                2,
                vec![]
            ),
            "unknown-capability"
        )["error"]["code"],
        "unsupported_capability"
    );
    assert_eq!(
        call(
            &mut broker,
            &mut endpoint,
            provider,
            "protocol",
            message(attributes(), 2, vec![0; MAX_PAYLOAD_BYTES + 1]),
            "oversize"
        )["error"]["code"],
        "invalid_request"
    );
    assert_eq!(
        call(
            &mut broker,
            &mut endpoint,
            provider,
            "protocol",
            message(attributes(), 1, vec![]),
            "duplicate-sequence"
        )["error"]["code"],
        "invalid_sequence"
    );
    assert!(endpoint.retire(&route()));
    assert_eq!(
        call(
            &mut broker,
            &mut endpoint,
            provider,
            "protocol",
            message(attributes(), 2, vec![]),
            "retired"
        )["error"]["code"],
        "unauthorized"
    );
    endpoint
}

#[test]
fn input_extension_round_trips_and_isolates_through_selected_broker_providers() {
    let first = exercise_provider(
        ExtensionAdapter::new(FakeEndpoint::default(), vec![policy()]).unwrap(),
        "first-provider",
    );
    let second = exercise_provider(
        ExtensionAdapter::new(OtherEndpoint::default(), vec![policy()]).unwrap(),
        "second-provider",
    );
    assert_eq!(first.endpoint().messages.len(), 2);
    assert_eq!(second.endpoint().0.len(), 2);
    assert_eq!(
        first.endpoint().messages[0].payload,
        second.endpoint().0[0].payload
    );
    assert_eq!(first.endpoint().messages[1].payload, vec![42; 960]);
}

#[test]
fn input_extension_negotiation_and_busy_do_not_admit_or_consume_sequence() {
    let mut invalid = policy();
    invalid.adapter_offer.remove(&speaker());
    assert!(matches!(
        ExtensionAdapter::new(FakeEndpoint::default(), vec![invalid]),
        Err("required_capability_unavailable")
    ));
    let mut adapter = ExtensionAdapter::new(
        FakeEndpoint {
            busy: true,
            ..Default::default()
        },
        vec![policy()],
    )
    .unwrap();
    let mut broker = broker("provider");
    assert_eq!(
        call(
            &mut broker,
            &mut adapter,
            "provider",
            "protocol",
            message(attributes(), 1, vec![1]),
            "busy"
        )["error"]["code"],
        "busy"
    );
    // A different endpoint instance is not used to hide consumption: inspect the same route
    // through a second BUSY submission at sequence 1.
    assert_eq!(
        call(
            &mut broker,
            &mut adapter,
            "provider",
            "protocol",
            message(attributes(), 1, vec![1]),
            "busy-retry"
        )["error"]["code"],
        "busy"
    );
    assert!(adapter.endpoint().messages.is_empty());
}

#[test]
fn input_extension_retirement_requires_fresh_generation_and_negotiation() {
    let mut adapter = ExtensionAdapter::new(FakeEndpoint::default(), vec![policy()]).unwrap();
    let mut replacement = policy();
    replacement.route.generation += 1;
    assert_eq!(adapter.install(replacement), Err("route_active"));
    assert!(adapter.retire(&route()));
    assert_eq!(adapter.install(policy()), Err("stale_generation"));
    let mut replacement = policy();
    replacement.route.generation += 1;
    assert_eq!(adapter.install(replacement), Ok(()));
    let mut unimplemented = policy();
    unimplemented.route.device = "other".into();
    let unknown = capability("example.unsupported", Direction::ToDevice);
    unimplemented.provider_offer.insert(unknown.clone());
    unimplemented.adapter_offer.insert(unknown.clone());
    unimplemented.required.insert(unknown);
    assert_eq!(
        adapter.install(unimplemented),
        Err("provider_capability_unavailable")
    );
}
