//! Bounded input-retirement ingress. Accepted requests are never acknowledgments:
//! they remain pending until the paired source registry and transport are drained.
use super::{Send, rpc};
use amaterasu_extension_api::LifecycleTarget;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::VecDeque;

const MAX_RETIREMENTS: usize = 16;
const MAX_WAITERS: usize = 4;

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Request {
    action: String,
    version: u8,
    operation_id: String,
    token: String,
    target: LifecycleTarget,
    input_boot_epoch: String,
}
impl Request {
    fn valid(&self) -> bool {
        fn token(value: &str, limit: usize) -> bool {
            !value.is_empty() && value.len() <= limit && !value.chars().any(char::is_control)
        }
        let stream = match &self.target {
            LifecycleTarget::Stream {
                host_epoch,
                session_id,
                stream_id,
                generation,
            } => {
                token(host_epoch, 128)
                    && token(session_id, 128)
                    && token(stream_id, 128)
                    && *generation != 0
            }
            _ => false,
        };
        self.action == "retire_input_feedback"
            && self.version == 1
            && token(&self.operation_id, 128)
            && token(&self.token, 256)
            && stream
            && self.input_boot_epoch.len() == 32
            && self
                .input_boot_epoch
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    }
}
struct Entry {
    request: Request,
    waiters: Vec<u64>,
}
#[derive(Default)]
pub(super) struct Inbox {
    entries: VecDeque<Entry>,
}
impl Inbox {
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    /// This verifies broker identity and request shape only. No payload value is
    /// treated as evidence of a source, admitted lease or transport retirement.
    pub fn receive(
        &mut self,
        request_id: u64,
        caller: &str,
        initiator: &str,
        selected: Option<&str>,
        payload: Value,
        send: &mut Send<'_>,
    ) {
        if selected != Some(caller) || initiator != caller {
            rpc::complete(
                send,
                request_id,
                json!({"ok":false,"code":"permission_denied"}),
            );
            return;
        }
        let Ok(request) = serde_json::from_value::<Request>(payload) else {
            rpc::complete(
                send,
                request_id,
                json!({"ok":false,"code":"invalid_request"}),
            );
            return;
        };
        if !request.valid() {
            rpc::complete(
                send,
                request_id,
                json!({"ok":false,"code":"invalid_request"}),
            );
            return;
        }
        if let Some(entry) = self
            .entries
            .iter_mut()
            .find(|entry| entry.request.operation_id == request.operation_id)
        {
            if entry.request != request {
                rpc::complete(
                    send,
                    request_id,
                    json!({"ok":false,"code":"operation_conflict"}),
                );
            } else if entry.waiters.len() >= MAX_WAITERS {
                rpc::complete(
                    send,
                    request_id,
                    json!({"ok":false,"code":"resource_exhausted"}),
                );
            } else if !entry.waiters.contains(&request_id) {
                entry.waiters.push(request_id);
            }
            return;
        }
        if self.entries.len() >= MAX_RETIREMENTS {
            rpc::complete(
                send,
                request_id,
                json!({"ok":false,"code":"resource_exhausted"}),
            );
            return;
        }
        self.entries.push_back(Entry {
            request,
            waiters: vec![request_id],
        });
    }
    pub fn prune_unstarted(&mut self, send: &mut Send<'_>) {
        for entry in &mut self.entries {
            entry.waiters.retain(|id| rpc::active(send, *id));
        }
        // This inbox has not yet issued a source fence or touched a provider.
        // Once it owns work, its worker must retain the entry after cancellation.
        self.entries.retain(|entry| !entry.waiters.is_empty());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use amaterasu_module_sdk::{Command, Response};
    fn payload(operation: &str) -> Value {
        json!({"action":"retire_input_feedback","version":1,"operation_id":operation,"token":"binding","target":{"kind":"stream","host_epoch":"host","session_id":"session","stream_id":"stream","generation":1},"input_boot_epoch":"a".repeat(32)})
    }
    #[test]
    fn real_broker_keeps_ingress_pending_until_cancelled() {
        use amaterasu_contracts::{Grants, RuntimeConfig};
        use amaterasu_runtime::Broker;
        let mut config = RuntimeConfig::default();
        config.principals.insert(
            "input".into(),
            Grants {
                call: ["owner/1".into()].into(),
                ..Default::default()
            },
        );
        config.principals.insert(
            "owner".into(),
            Grants {
                provide: ["owner/1".into()].into(),
                ..Default::default()
            },
        );
        let mut broker = Broker::new(config).unwrap();
        assert!(matches!(
            broker.dispatch(
                "owner",
                Command::Register {
                    service: "owner/1".into()
                }
            ),
            Response::Ok
        ));
        let Response::Accepted { id } = broker.dispatch(
            "input",
            Command::Request {
                service: "owner/1".into(),
                payload: payload("op"),
                timeout_ms: 5000,
                operation_id: "op".into(),
                parent: None,
            },
        ) else {
            panic!("admission");
        };
        let Response::Request {
            caller,
            initiator,
            payload,
            ..
        } = broker.dispatch("owner", Command::Poll {})
        else {
            panic!("delivery");
        };
        let mut inbox = Inbox::default();
        inbox.receive(
            id,
            &caller,
            &initiator,
            Some("input"),
            payload,
            &mut |cmd| Ok(broker.dispatch("owner", cmd)),
        );
        assert_eq!(inbox.len(), 1);
        assert!(matches!(
            broker.dispatch("input", Command::TakeResult { request: id }),
            Response::Pending
        ));
        assert!(matches!(
            broker.dispatch("input", Command::Cancel { request: id }),
            Response::Ok
        ));
        inbox.prune_unstarted(&mut |cmd| Ok(broker.dispatch("owner", cmd)));
        assert_eq!(inbox.len(), 0);
    }

    #[test]
    fn only_direct_selected_input_can_queue_and_retry_is_exact() {
        let mut inbox = Inbox::default();
        let mut replies = Vec::new();
        let mut send = |command| {
            if let Command::Complete { request, payload } = command {
                replies.push((request, payload));
            }
            Ok(Response::Ok)
        };
        inbox.receive(
            1,
            "input",
            "attacker",
            Some("input"),
            payload("op"),
            &mut send,
        );
        inbox.receive(2, "other", "other", Some("input"), payload("op"), &mut send);
        inbox.receive(3, "input", "input", Some("input"), payload("op"), &mut send);
        inbox.receive(4, "input", "input", Some("input"), payload("op"), &mut send);
        let mut conflict = payload("op");
        conflict["token"] = "other".into();
        inbox.receive(5, "input", "input", Some("input"), conflict, &mut send);
        assert_eq!(inbox.len(), 1);
        assert_eq!(replies.len(), 3);
        assert_eq!(replies[0].1["code"], "permission_denied");
        assert_eq!(replies[2].1["code"], "operation_conflict");
    }
    #[test]
    fn bounded_queue_fails_closed_and_unstarted_cancellation_releases_capacity() {
        let mut inbox = Inbox::default();
        let mut replies = Vec::new();
        let mut send = |command| {
            match command {
                Command::Complete { request, payload } => replies.push((request, payload)),
                Command::CheckRequest { .. } => {
                    return Ok(Response::Error {
                        code: "request_ended".into(),
                        message: String::new(),
                    });
                }
                _ => {}
            }
            Ok(Response::Ok)
        };
        for n in 0..=MAX_RETIREMENTS {
            inbox.receive(
                n as u64,
                "input",
                "input",
                Some("input"),
                payload(&format!("op-{n}")),
                &mut send,
            );
        }
        assert_eq!(inbox.len(), MAX_RETIREMENTS);
        inbox.prune_unstarted(&mut send);
        assert_eq!(inbox.len(), 0);
        assert_eq!(replies[0].1["code"], "resource_exhausted");
    }
}
