//! Neutral replacement provider for the optional outbound feedback contract.
use amaterasu_module_sdk::input::outbound::{
    OutboundContribution, OutboundFeedbackEvent, bounded_feedback_response,
};
use amaterasu_module_sdk::{Command, ManagedModule, ModuleResult, Response, RuntimeClient};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

const INPUT: &str = "amaterasu.input/1";
struct Neutral {
    runtime: RuntimeClient,
    session: Option<u64>,
    operation: Option<String>,
    lease: Option<String>,
    generation: u64,
    ttl_ms: u64,
    expires: Instant,
    last_peeked: u64,
    events: VecDeque<Value>,
}
impl ManagedModule for Neutral {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        runtime.require_success(Command::Register {
            service: INPUT.into(),
        })?;
        Ok(Self {
            runtime,
            session: None,
            operation: None,
            lease: None,
            generation: 0,
            ttl_ms: 0,
            expires: Instant::now(),
            last_peeked: 0,
            events: VecDeque::new(),
        })
    }
    fn tick(&mut self) -> ModuleResult<()> {
        for _ in 0..8 {
            let Response::Request {
                id,
                caller,
                payload,
                ..
            } = self.runtime.send(Command::Poll {})?
            else {
                break;
            };
            let result = self.handle(&caller, payload);
            self.runtime.require_success(Command::Complete {
                request: id,
                payload: result,
            })?;
        }
        Ok(())
    }
}
impl Neutral {
    fn active(&self) -> Value {
        json!({"ok":true,"state":"active","operation_id":self.operation,"lease":self.lease,
            "ttl_ms":self.ttl_ms,"expires_in_ms":self.ttl_ms,"backend":"neutral-fixture","cleanup_required":false})
    }
    fn handle(&mut self, caller: &str, payload: Value) -> Value {
        if caller != "amaterasu.stream-session" {
            return json!({"ok":false,"error":{"code":"permission_denied"}});
        }
        let action = payload["action"].as_str().unwrap_or("");
        if self.lease.is_some() && Instant::now() >= self.expires {
            self.lease = None;
            self.session = None;
            self.events.clear();
        }
        if action == "acquire" {
            let Some(operation) = payload["operation_id"].as_str() else {
                return json!({"ok":false,"error":{"code":"invalid_request"}});
            };
            let Some(ttl_ms) = payload["ttl_ms"]
                .as_u64()
                .filter(|ttl| (1000..=30000).contains(ttl))
            else {
                return json!({"ok":false,"error":{"code":"invalid_request"}});
            };
            if self.lease.is_some() && self.operation.as_deref() != Some(operation) {
                return json!({"ok":false,"error":{"code":"input_busy"}});
            }
            if self.lease.is_none() {
                self.generation += 1;
                self.lease = Some(format!("{:032x}", self.generation));
                self.operation = Some(operation.into());
                self.ttl_ms = ttl_ms;
                self.expires = Instant::now() + Duration::from_millis(ttl_ms);
                self.session = None;
                self.last_peeked = 0;
                self.events.clear();
            }
            return self.active();
        }
        if action == "lookup" {
            return if self.operation.as_deref() == payload["operation_id"].as_str()
                && self.lease.is_some()
            {
                self.active()
            } else {
                json!({"ok":false,"error":{"code":"not_found"}})
            };
        }
        if self.lease.is_none() || payload["lease"].as_str() != self.lease.as_deref() {
            return json!({"ok":false,"error":{"code":"permission_denied"}});
        }
        match payload["action"].as_str() {
            Some("feedback_peek") if payload["contribution_version"] == 1 => {
                let Some(session) = payload["session"].as_u64().filter(|id| *id != 0) else {
                    return json!({"ok":false,"error":{"code":"invalid_request"}});
                };
                if self.session.is_none() {
                    self.session = Some(session);
                    for (sequence, schema, byte) in [
                        (1, "org.example.alpha", 1),
                        (2, "org.example.denied", 9),
                        (3, "org.example.beta", 2),
                    ] {
                        let contribution = OutboundContribution {
                            version: 1,
                            schema: schema.into(),
                            schema_version: 1,
                            session,
                            resource: "neutral-resource".into(),
                            incarnation: self.generation,
                            payload: vec![byte],
                        };
                        self.events.push_back(
                            serde_json::to_value(OutboundFeedbackEvent {
                                sequence,
                                contribution,
                            })
                            .unwrap(),
                        );
                    }
                }
                if self.session != Some(session) {
                    return json!({"ok":false,"error":{"code":"permission_denied"}});
                }
                let limit = payload["limit"].as_u64().unwrap_or(0).min(16) as usize;
                let events: Vec<_> = self.events.iter().take(limit).cloned().collect();
                if let Some(sequence) = events.last().and_then(|event| event["sequence"].as_u64()) {
                    self.last_peeked = self.last_peeked.max(sequence);
                }
                bounded_feedback_response(&events)
                    .unwrap_or_else(|_| json!({"ok":false,"error":{"code":"response_too_large"}}))
            }
            Some("feedback_ack") => {
                let Some(through) = payload["through_sequence"].as_u64() else {
                    return json!({"ok":false,"error":{"code":"invalid_request"}});
                };
                if through > self.last_peeked {
                    return json!({"ok":false,"error":{"code":"invalid_request"}});
                }
                while self.events.front().is_some_and(|item| {
                    item["sequence"]
                        .as_u64()
                        .is_some_and(|sequence| sequence <= through)
                }) {
                    self.events.pop_front();
                }
                json!({"ok":true,"through_sequence":through})
            }
            Some("renew") => {
                let Some(ttl) = payload["ttl_ms"]
                    .as_u64()
                    .filter(|ttl| (1000..=30000).contains(ttl))
                else {
                    return json!({"ok":false,"error":{"code":"invalid_request"}});
                };
                self.ttl_ms = ttl;
                self.expires = Instant::now() + Duration::from_millis(ttl);
                json!({"ok":true,"lease":self.lease,"ttl_ms":ttl})
            }
            Some("batch") => {
                json!({"ok":true,"lease":self.lease,"accepted":payload["events"].as_array().map_or(0, Vec::len)})
            }
            Some("release") => {
                self.lease = None;
                self.session = None;
                self.events.clear();
                json!({"ok":true,"released":true})
            }
            _ => json!({"ok":false,"error":{"code":"unsupported"}}),
        }
    }
}
amaterasu_module_sdk::export_module!(Neutral);
