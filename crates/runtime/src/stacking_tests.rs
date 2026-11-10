//! Service layers, provider refusals and the hook flows built for stacking.
mod hooks;
mod layers;
mod refusal;

use crate::Broker;
use amaterasu_contracts::{Command, Grants, Response, RuntimeConfig};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::time::Instant;

const ECHO: &str = "example.echo/1";

fn names(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|name| (*name).to_owned()).collect()
}

fn caller_of(services: &[&str]) -> Grants {
    Grants {
        call: names(services),
        ..Default::default()
    }
}

fn provider_of(services: &[&str]) -> Grants {
    Grants {
        provide: names(services),
        ..Default::default()
    }
}

/// A consumer, the echo provider, and `layers` stacked on it outermost first.
fn layered_config(layers: &[&str]) -> RuntimeConfig {
    let mut config = RuntimeConfig::default();
    config
        .principals
        .insert("consumer".into(), caller_of(&[ECHO]));
    config
        .principals
        .insert("provider".into(), provider_of(&[ECHO]));
    config
        .principals
        .insert("outsider".into(), Grants::default());
    for layer in layers {
        config
            .principals
            .insert((*layer).into(), caller_of(&[ECHO]));
    }
    if !layers.is_empty() {
        config.service_layers.insert(
            ECHO.into(),
            layers.iter().map(|layer| (*layer).to_owned()).collect(),
        );
    }
    config
}

fn register(broker: &mut Broker, principal: &str, service: &str) -> Response {
    broker.dispatch(
        principal,
        Command::Register {
            service: service.into(),
        },
    )
}

/// The echo provider and every listed layer, all registered.
fn layered_broker(layers: &[&str]) -> Broker {
    let mut broker = Broker::new(layered_config(layers)).unwrap();
    assert!(matches!(
        register(&mut broker, "provider", ECHO),
        Response::Ok
    ));
    for layer in layers {
        assert!(matches!(register(&mut broker, layer, ECHO), Response::Ok));
    }
    broker
}

fn request(operation: &str, payload: Value, parent: Option<u64>) -> Command {
    Command::Request {
        service: ECHO.into(),
        payload,
        timeout_ms: 1_000,
        operation_id: operation.into(),
        parent,
    }
}

fn accepted(response: Response) -> u64 {
    match response {
        Response::Accepted { id } => id,
        other => panic!("expected an accepted request, got {other:?}"),
    }
}

fn error_code(response: Response) -> String {
    match response {
        Response::Error { code, .. } => code,
        other => panic!("expected an error, got {other:?}"),
    }
}

/// What a serving principal is shown for its next queued request.
struct Offered {
    id: u64,
    caller: String,
    initiator: String,
    payload: Value,
}

fn poll(broker: &mut Broker, principal: &str, now: Instant) -> Option<Offered> {
    match broker.dispatch_at(principal, Command::Poll {}, now) {
        Response::Request {
            id,
            caller,
            initiator,
            payload,
            ..
        } => Some(Offered {
            id,
            caller,
            initiator,
            payload,
        }),
        Response::Pending => None,
        other => panic!("unexpected poll reply: {other:?}"),
    }
}

fn complete(broker: &mut Broker, principal: &str, request: u64, payload: Value) -> Response {
    broker.dispatch(principal, Command::Complete { request, payload })
}

fn result(broker: &mut Broker, principal: &str, request: u64) -> Response {
    broker.dispatch(principal, Command::TakeResult { request })
}

fn result_payload(broker: &mut Broker, principal: &str, request: u64) -> Value {
    match result(broker, principal, request) {
        Response::Result { payload } => payload,
        other => panic!("expected a result, got {other:?}"),
    }
}

fn echoed(text: &str) -> Value {
    json!({ "text": text })
}
