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
    fn validate(&self, _message: &Message) -> Result<(), &'static str> {
        Ok(())
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
    fn validate(&self, _message: &Message) -> Result<(), &'static str> {
        Ok(())
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
        sampled: Default::default(),
        terminal: Default::default(),
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

mod sampled_tests {
    use super::*;
    use amaterasu_module_sdk::input::extensions::{SampleDrain, speaker_pcm, steam_attributes};
    use std::{
        cell::{Cell, RefCell},
        rc::Rc,
        time::{Duration, Instant},
    };

    fn control_capability() -> Capability {
        capability("example.controller.control", Direction::ToClient)
    }
    fn terminal_capability() -> Capability {
        capability("example.controller.stop", Direction::ToClient)
    }
    fn validate_schema(message: &Message) -> Result<(), &'static str> {
        if message.capability == steam_attributes::capability() {
            steam_attributes::Attributes::decode(&message.payload).map(|_| ())
        } else if message.capability == speaker_pcm::capability() {
            speaker_pcm::Block::decode(&message.payload).map(|_| ())
        } else if (message.capability == control_capability()
            || message.capability == terminal_capability())
            && message.payload.is_empty()
        {
            Ok(())
        } else {
            Err("unsupported")
        }
    }
    #[derive(Default, Clone)]
    struct Trace {
        busy_audio: Rc<Cell<bool>>,
        busy_terminal: Rc<Cell<bool>>,
        messages: Rc<RefCell<Vec<Message>>>,
    }
    struct ProviderA(Trace);
    struct ProviderB(Trace);
    impl Endpoint for ProviderA {
        fn supports(&self, capability: &Capability) -> bool {
            *capability == steam_attributes::capability()
                || *capability == speaker_pcm::capability()
                || *capability == control_capability()
                || *capability == terminal_capability()
        }
        fn validate(&self, message: &Message) -> Result<(), &'static str> {
            validate_schema(message)
        }
        fn submit(&mut self, message: &Message) -> Result<(), SubmitError> {
            if message.capability == terminal_capability() && self.0.busy_terminal.get() {
                return Err(SubmitError::Busy);
            }
            if message.capability == speaker_pcm::capability() && self.0.busy_audio.get() {
                return Err(SubmitError::Busy);
            }
            let mut messages = self.0.messages.borrow_mut();
            if messages.len() >= 16 {
                return Err(SubmitError::Busy);
            }
            messages.push(message.clone());
            Ok(())
        }
    }
    impl Endpoint for ProviderB {
        fn supports(&self, capability: &Capability) -> bool {
            [
                steam_attributes::capability(),
                speaker_pcm::capability(),
                control_capability(),
                terminal_capability(),
            ]
            .contains(capability)
        }
        fn validate(&self, message: &Message) -> Result<(), &'static str> {
            if message.capability == steam_attributes::capability() {
                steam_attributes::Attributes::decode(&message.payload)?
                    .encode()
                    .map(|_| ())
            } else if message.capability == speaker_pcm::capability() {
                speaker_pcm::Block::decode(&message.payload)?
                    .encode()
                    .map(|_| ())
            } else if [control_capability(), terminal_capability()].contains(&message.capability)
                && message.payload.is_empty()
            {
                Ok(())
            } else {
                Err("unsupported")
            }
        }
        fn submit(&mut self, message: &Message) -> Result<(), SubmitError> {
            if message.capability == terminal_capability() && self.0.busy_terminal.get() {
                return Err(SubmitError::Busy);
            }
            let payload = if message.capability == speaker_pcm::capability() {
                if self.0.busy_audio.get() {
                    return Err(SubmitError::Busy);
                }
                speaker_pcm::Block::decode(&message.payload)
                    .map_err(|_| SubmitError::Rejected)?
                    .encode()
                    .map_err(|_| SubmitError::Rejected)?
            } else if message.capability == steam_attributes::capability() {
                steam_attributes::Attributes::decode(&message.payload)
                    .map_err(|_| SubmitError::Rejected)?
                    .encode()
                    .map_err(|_| SubmitError::Rejected)?
            } else {
                message.payload.clone()
            };
            if self.0.messages.borrow().len() >= 16 {
                return Err(SubmitError::Busy);
            }
            let mut copy = message.clone();
            copy.payload = payload;
            self.0.messages.borrow_mut().push(copy);
            Ok(())
        }
    }

    fn policy_for(route: Route, feedback_caller: &str) -> RoutePolicy {
        let capabilities = [
            steam_attributes::capability(),
            speaker_pcm::capability(),
            control_capability(),
            terminal_capability(),
        ]
        .into();
        RoutePolicy {
            route,
            provider_offer: capabilities,
            adapter_offer: [
                steam_attributes::capability(),
                speaker_pcm::capability(),
                control_capability(),
                terminal_capability(),
            ]
            .into(),
            required: [steam_attributes::capability(), speaker_pcm::capability()].into(),
            sampled: [(speaker_pcm::capability(), 50)].into(),
            terminal: [(terminal_capability(), [speaker_pcm::capability()].into())].into(),
            to_device: vec![principal("protocol")],
            to_client: vec![principal(feedback_caller)],
        }
    }
    fn attribute_payload() -> Vec<u8> {
        steam_attributes::Attributes {
            transport: 2,
            valid_mask: 0xff,
            firmware_build_time: 1234,
            bootloader_build_time: 5,
            board_revision: 6,
            capabilities: 7,
            radio_firmware_build_time: 8,
            dongle_firmware_build_time: 9,
            build_id: *b"build-id-1234567",
            unit_serial: *b"unit-serial-1234",
            board_serial: *b"board-serial-123",
        }
        .encode()
        .unwrap()
    }
    fn sample_payload(source_sequence: u32) -> Vec<u8> {
        speaker_pcm::Block {
            owner: 9,
            generation: 8,
            sequence: source_sequence,
            sample_position: u64::from(source_sequence) * u64::from(speaker_pcm::FRAMES),
            flags: if source_sequence == 0 {
                speaker_pcm::START
            } else {
                0
            },
            samples: [source_sequence as u8; speaker_pcm::SAMPLE_BYTES],
        }
        .encode()
        .unwrap()
    }
    fn exercise<E: Endpoint>(endpoint: E, trace: Trace, provider: &str) {
        let mut route_b = route();
        route_b.source = "client-b".into();
        let mut adapter = ExtensionAdapter::new(
            endpoint,
            vec![
                policy_for(route(), "feedback"),
                policy_for(route_b.clone(), "intruder"),
            ],
        )
        .unwrap();
        let mut broker = broker(provider);
        let attrs = Message {
            route: route(),
            capability: steam_attributes::capability(),
            sequence: 1,
            payload: attribute_payload(),
        };
        assert_eq!(
            call(
                &mut broker,
                &mut adapter,
                provider,
                "protocol",
                attrs.clone(),
                "attrs"
            )["ok"],
            true
        );
        let mut malformed = attrs.clone();
        malformed.sequence = 2;
        malformed.payload.pop();
        assert_eq!(
            call(
                &mut broker,
                &mut adapter,
                provider,
                "protocol",
                malformed,
                "bad-attrs"
            )["error"]["code"],
            "invalid_payload"
        );
        let mut oversized = attrs.clone();
        oversized.sequence = 2;
        oversized.payload = vec![0; MAX_PAYLOAD_BYTES + 1];
        assert_eq!(
            call(
                &mut broker,
                &mut adapter,
                provider,
                "protocol",
                oversized,
                "large"
            )["error"]["code"],
            "invalid_request"
        );
        let sample = |source: Route, seq: u64, value: u32| Message {
            route: source,
            capability: speaker_pcm::capability(),
            sequence: seq,
            payload: sample_payload(value),
        };
        assert_eq!(
            call(
                &mut broker,
                &mut adapter,
                provider,
                "intruder",
                sample(route(), 1, 0),
                "wrong-source"
            )["error"]["code"],
            "unauthorized"
        );
        let mut malformed = sample(route(), 1, 0);
        malformed.payload[0..8].fill(0);
        assert_eq!(
            call(
                &mut broker,
                &mut adapter,
                provider,
                "feedback",
                malformed,
                "bad-sample"
            )["error"]["code"],
            "invalid_payload"
        );
        let mut unsupported = sample(route(), 1, 0);
        unsupported.capability.version = 2;
        assert_eq!(
            call(
                &mut broker,
                &mut adapter,
                provider,
                "feedback",
                unsupported,
                "unsupported"
            )["error"]["code"],
            "unsupported_capability"
        );
        trace.busy_audio.set(true);
        assert_eq!(
            call(
                &mut broker,
                &mut adapter,
                provider,
                "feedback",
                sample(route(), 1, 0),
                "sample-a1"
            )["ok"],
            true
        );
        assert_eq!(
            adapter.drain_sample_at(&route(), &speaker_pcm::capability(), Instant::now()),
            SampleDrain::Busy
        );
        for seq in 2..=12 {
            assert_eq!(
                call(
                    &mut broker,
                    &mut adapter,
                    provider,
                    "feedback",
                    sample(route(), seq, (seq - 1) as u32),
                    &format!("sample-a{seq}")
                )["ok"],
                true
            );
        }
        assert_eq!(
            call(
                &mut broker,
                &mut adapter,
                provider,
                "intruder",
                sample(route_b.clone(), 1, 22),
                "sample-b"
            )["ok"],
            true
        );
        assert_eq!(adapter.pending_sample_count(), 2);
        // Strict control in the same direction remains independent of sample pressure.
        let control = |sequence| Message {
            route: route(),
            capability: control_capability(),
            sequence,
            payload: vec![],
        };
        assert_eq!(
            call(
                &mut broker,
                &mut adapter,
                provider,
                "feedback",
                control(1),
                "control-1"
            )["ok"],
            true
        );
        assert_eq!(
            call(
                &mut broker,
                &mut adapter,
                provider,
                "feedback",
                control(3),
                "control-gap"
            )["error"]["code"],
            "invalid_sequence"
        );
        assert_eq!(
            call(
                &mut broker,
                &mut adapter,
                provider,
                "feedback",
                control(2),
                "control-2"
            )["ok"],
            true
        );
        assert_eq!(
            call(
                &mut broker,
                &mut adapter,
                provider,
                "feedback",
                sample(route(), 12, 11),
                "sample-replay"
            )["error"]["code"],
            "invalid_sequence"
        );
        trace.busy_audio.set(false);
        assert_eq!(
            adapter.drain_sample_at(&route(), &speaker_pcm::capability(), Instant::now()),
            SampleDrain::Accepted
        );
        let recorded = trace.messages.borrow();
        let last = speaker_pcm::Block::decode(&recorded.last().unwrap().payload).unwrap();
        assert_eq!(last.sequence, 11);
        assert_eq!(last.sample_position, 11 * 240);
        drop(recorded);
        assert_eq!(
            call(
                &mut broker,
                &mut adapter,
                provider,
                "feedback",
                sample(route(), 13, 12),
                "sample-a13"
            )["ok"],
            true
        );
        let terminal = Message {
            route: route(),
            capability: terminal_capability(),
            sequence: 3,
            payload: vec![],
        };
        trace.busy_terminal.set(true);
        assert_eq!(
            call(
                &mut broker,
                &mut adapter,
                provider,
                "feedback",
                terminal.clone(),
                "terminal-busy"
            )["error"]["code"],
            "busy"
        );
        assert_eq!(adapter.pending_sample_count(), 2);
        trace.busy_terminal.set(false);
        assert_eq!(
            call(
                &mut broker,
                &mut adapter,
                provider,
                "feedback",
                terminal,
                "terminal"
            )["ok"],
            true
        );
        assert_eq!(
            adapter.drain_sample_at(&route(), &speaker_pcm::capability(), Instant::now()),
            SampleDrain::Empty
        );
        assert_eq!(
            call(
                &mut broker,
                &mut adapter,
                provider,
                "feedback",
                sample(route(), 14, 13),
                "after-terminal"
            )["error"]["code"],
            "stream_stopped"
        );
        assert_eq!(adapter.pending_sample_count(), 1);
        assert_eq!(
            adapter.drain_sample_at(
                &route_b,
                &speaker_pcm::capability(),
                Instant::now() + Duration::from_secs(1)
            ),
            SampleDrain::Expired
        );
        assert_eq!(
            call(
                &mut broker,
                &mut adapter,
                provider,
                "intruder",
                sample(route_b.clone(), 2, 23),
                "sample-b2"
            )["ok"],
            true
        );
        assert!(adapter.retire(&route_b));
        assert_eq!(
            adapter.drain_sample_at(&route_b, &speaker_pcm::capability(), Instant::now()),
            SampleDrain::Empty
        );
        let mut next = route_b.clone();
        next.generation += 1;
        adapter.install(policy_for(next, "intruder")).unwrap();
        assert_eq!(
            call(
                &mut broker,
                &mut adapter,
                provider,
                "intruder",
                sample(route_b, 3, 24),
                "stale-b"
            )["error"]["code"],
            "unauthorized"
        );
    }

    #[test]
    fn sampled_schemas_and_isolation_survive_broker_provider_replacement() {
        let trace = Trace::default();
        exercise(ProviderA(trace.clone()), trace, "sample-provider-a");
        let trace = Trace::default();
        exercise(ProviderB(trace.clone()), trace, "sample-provider-b");
    }
}
