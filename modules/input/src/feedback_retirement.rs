//! Input-side retirement handshake. A successful response is accepted only from
//! the trusted selected stream service and only for the exact retired binding.
use amaterasu_extension_api::LifecycleTarget;
use amaterasu_module_sdk::{Command, ModuleResult, Response, effect_authority::StreamRetirement};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

pub(crate) struct Job {
    service: String,
    token: String,
    target: LifecycleTarget,
    input_boot_epoch: String,
    operation_id: String,
    pending: Option<u64>,
    next_attempt: Instant,
}
impl Job {
    pub fn new(
        service: String,
        token: String,
        target: LifecycleTarget,
        input_boot_epoch: String,
        operation_id: String,
    ) -> Result<Self, &'static str> {
        if operation_id.is_empty()
            || operation_id.len() > 128
            || operation_id.chars().any(char::is_control)
        {
            return Err("invalid_feedback_retirement_operation");
        }
        Ok(Self {
            service,
            token,
            target,
            input_boot_epoch,
            operation_id,
            pending: None,
            next_attempt: Instant::now(),
        })
    }
    pub fn advance(&mut self, send: &mut impl FnMut(Command) -> ModuleResult<Response>) -> bool {
        if let Some(id) = self.pending {
            match send(Command::TakeResult { request: id }) {
                Ok(Response::Pending) => return false,
                Ok(Response::Result { payload }) => {
                    // If releasing the result is uncertain keep the handle and
                    // retry; a lost release must not turn into early success.
                    if !matches!(
                        send(Command::ReleaseResult { request: id }),
                        Ok(Response::Ok)
                    ) {
                        return false;
                    }
                    self.pending = None;
                    if self.matches(&payload) {
                        return true;
                    }
                }
                _ => {
                    let _ = send(Command::ReleaseResult { request: id });
                    self.pending = None;
                }
            }
            self.next_attempt = Instant::now() + Duration::from_secs(1);
            return false;
        }
        if Instant::now() < self.next_attempt {
            return false;
        }
        let response = send(Command::Request {
            service: self.service.clone(),
            payload: json!({"action":"retire_input_feedback","version":1,"operation_id":self.operation_id,"token":self.token,"target":self.target,"input_boot_epoch":self.input_boot_epoch}),
            timeout_ms: 5000,
            operation_id: self.operation_id.clone(),
            parent: None,
        });
        match response {
            Ok(Response::Accepted { id }) => self.pending = Some(id),
            _ => self.next_attempt = Instant::now() + Duration::from_secs(1),
        }
        false
    }
    fn matches(&self, value: &Value) -> bool {
        let Some(object) = value.as_object() else {
            return false;
        };
        if object.len() != 9
            || value["ok"] != true
            || value["status"] != "retired"
            || value["pairing_retired"] != true
            || value["source_count"]
                .as_u64()
                .is_none_or(|count| count > 64)
            || value["token"] != self.token
            || value["target"] != json!(self.target)
            || value["input_boot_epoch"] != self.input_boot_epoch
            || value["operation_id"] != self.operation_id
        {
            return false;
        }
        let Ok(receipt) = serde_json::from_value::<StreamRetirement>(value["retirement"].clone())
        else {
            return false;
        };
        receipt.ok
            && receipt.state == "retired"
            && receipt.scope == "whole_control_stream"
            && receipt.stream != 0
            && receipt.provider_epoch.len() == 32
            && receipt
                .provider_epoch
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            && !receipt.service.is_empty()
            && receipt.service.ends_with("/1")
            && !receipt.operation_id.is_empty()
            && receipt.operation_id.len() <= 128
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use amaterasu_contracts::{Grants, RuntimeConfig};
    use amaterasu_runtime::Broker;

    fn target() -> LifecycleTarget {
        LifecycleTarget::Stream {
            host_epoch: "host".into(),
            session_id: "session".into(),
            stream_id: "stream".into(),
            generation: 1,
        }
    }
    fn setup() -> (Broker, Job) {
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
        let job = Job::new(
            "owner/1".into(),
            "binding".into(),
            target(),
            "a".repeat(32),
            "revoke".into(),
        )
        .unwrap();
        (broker, job)
    }
    fn reply() -> Value {
        json!({"ok":true,"status":"retired","token":"binding","target":target(),"input_boot_epoch":"a".repeat(32),"operation_id":"revoke","pairing_retired":true,"source_count":1,"retirement":{"ok":true,"service":"control/1","stream":3,"provider_epoch":"b".repeat(32),"operation_id":"retire","scope":"whole_control_stream","state":"retired"}})
    }
    #[test]
    fn selected_broker_owner_must_complete_exact_retirement_before_ack() {
        let (mut broker, mut job) = setup();
        assert!(!job.advance(&mut |cmd| Ok(broker.dispatch("input", cmd))));
        let Response::Request {
            id,
            caller,
            initiator,
            payload,
            ..
        } = broker.dispatch("owner", Command::Poll {})
        else {
            panic!("missing request")
        };
        assert_eq!(caller, "input");
        assert_eq!(initiator, "input");
        assert_eq!(payload["token"], "binding");
        assert!(!job.advance(&mut |cmd| Ok(broker.dispatch("input", cmd))));
        assert!(matches!(
            broker.dispatch(
                "owner",
                Command::Complete {
                    request: id,
                    payload: reply()
                }
            ),
            Response::Ok
        ));
        assert!(job.advance(&mut |cmd| Ok(broker.dispatch("input", cmd))));
    }
    #[test]
    fn stale_or_incomplete_receipt_never_acknowledges() {
        let (_, job) = setup();
        for (field, wrong) in [
            ("token", json!("other")),
            ("input_boot_epoch", json!("c".repeat(32))),
            ("operation_id", json!("other")),
            ("retirement", Value::Null),
        ] {
            let mut value = reply();
            value[field] = wrong;
            assert!(!job.matches(&value));
        }
        let mut value = reply();
        value["retirement"]["scope"] = "source".into();
        assert!(!job.matches(&value));
    }
    #[test]
    fn lost_release_reply_does_not_fabricate_acknowledgment() {
        let (mut broker, mut job) = setup();
        assert!(!job.advance(&mut |cmd| Ok(broker.dispatch("input", cmd))));
        let Response::Request { id, .. } = broker.dispatch("owner", Command::Poll {}) else {
            panic!("missing request")
        };
        assert!(matches!(
            broker.dispatch(
                "owner",
                Command::Complete {
                    request: id,
                    payload: reply()
                }
            ),
            Response::Ok
        ));
        assert!(!job.advance(&mut |cmd| {
            let is_release = matches!(cmd, Command::ReleaseResult { .. });
            let result = broker.dispatch("input", cmd);
            if is_release {
                Err("reply lost".into())
            } else {
                Ok(result)
            }
        }));
        assert!(!job.advance(&mut |cmd| Ok(broker.dispatch("input", cmd))));
    }

    #[test]
    fn unavailable_or_lost_admission_never_acknowledges() {
        let (_, mut job) = setup();
        assert!(!job.advance(&mut |_| Err("lost".into())));
        assert!(!job.advance(&mut |_| panic!("retry must back off")));
    }
}
