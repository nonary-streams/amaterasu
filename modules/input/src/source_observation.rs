//! Bounded, opt-in active lifecycle observation for the selected input owner.
//! The result is a point-in-time observation, never an effect grant.
use amaterasu_extension_api::LifecycleTarget;
use amaterasu_module_sdk::{Command, ModuleResult, Response, input::extensions::source_label};
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::{Duration, Instant};

const LIFECYCLE: &str = "amaterasu.session-lifecycle/1";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Request {
    pub action: String,
    pub version: u8,
    pub lease: String,
    pub input_binding: String,
    pub controller_slot: u16,
}
impl Request {
    pub fn authorized(&self, selected_owner: &str, caller: &str, initiator: &str) -> bool {
        caller == selected_owner && initiator == caller && self.valid()
    }
    pub fn valid(&self) -> bool {
        self.action == "observe_feedback_source"
            && self.version == 1
            && self.controller_slot < 16
            && [&self.lease, &self.input_binding]
                .iter()
                .all(|s| !s.is_empty() && s.len() <= 256 && !s.chars().any(char::is_control))
    }
}

pub(crate) struct DeviceObservation {
    pub device: (u64, u32),
    pub offer: Vec<String>,
    pub immutable_controller_policy: bool,
}

pub(crate) struct Job {
    pub request_id: u64,
    pub caller: String,
    pub request: Request,
    pub target: LifecycleTarget,
    pub observation: DeviceObservation,
    boot_epoch: String,
    deadline: Instant,
    operation_id: String,
    pending: Option<u64>,
}

impl Job {
    pub fn new(
        request_id: u64,
        caller: String,
        request: Request,
        target: LifecycleTarget,
        observation: DeviceObservation,
        boot_epoch: String,
        deadline: Instant,
    ) -> Result<Self, &'static str> {
        if !request.valid()
            || observation.device.0 == 0
            || observation.offer.len() > 32
            || observation
                .offer
                .iter()
                .any(|schema| schema.len() > 128 || schema.is_empty())
            || boot_epoch.len() != 32
            || !boot_epoch
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err("invalid_source_observation");
        }
        let mut random = [0_u8; 16];
        getrandom::fill(&mut random).map_err(|_| "randomness_unavailable")?;
        let operation_id = format!(
            "input-source-{}",
            random
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        );
        let deadline = deadline.min(Instant::now() + Duration::from_secs(5));
        Ok(Self {
            request_id,
            caller,
            request,
            target,
            observation,
            boot_epoch,
            deadline,
            operation_id,
            pending: None,
        })
    }
    pub fn cancel(&mut self, send: &mut impl FnMut(Command) -> ModuleResult<Response>) {
        if let Some(request) = self.pending.take() {
            let _ = send(Command::Cancel { request });
            let _ = send(Command::ReleaseResult { request });
        }
    }
    /// `current` is recomputed by the owner around each async operation and must
    /// include its lease, active binding, owned device and exact incarnation.
    pub fn advance(
        &mut self,
        current: bool,
        send: &mut impl FnMut(Command) -> ModuleResult<Response>,
    ) -> Result<Option<Value>, &'static str> {
        if !current
            || Instant::now() >= self.deadline
            || !matches!(
                send(Command::CheckRequest {
                    request: self.request_id
                }),
                Ok(Response::Ok)
            )
        {
            self.cancel(send);
            return Err("source_observation_retired");
        }
        if let Some(request) = self.pending {
            return match send(Command::TakeResult { request }) {
                Ok(Response::Pending) => Ok(None),
                Ok(Response::Result { payload }) => {
                    if !matches!(send(Command::ReleaseResult { request }), Ok(Response::Ok)) {
                        return Err("source_observation_unavailable");
                    }
                    self.pending = None;
                    let source = source_label::observe(&payload, &self.target)
                        .map_err(|_| "source_observation_denied")?
                        .ok_or("source_observation_denied")?;
                    Ok(Some(json!({"ok":true,"version":1,"target":self.target,
                        "source":{"version":source.version,"id":source.id,"generation":source.generation},
                        "input_boot_epoch":self.boot_epoch,
                        "device":{"slot":self.request.controller_slot,"generation":self.observation.device.0,"capabilities":self.observation.device.1},"provider_offer":self.observation.offer,"immutable_controller_policy":self.observation.immutable_controller_policy})))
                }
                _ => Err("source_observation_unavailable"),
            };
        }
        let remaining = self
            .deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as u64;
        match send(Command::Request {
            service: LIFECYCLE.into(),
            payload: json!({"operation":"validate_input_binding","operation_id":self.operation_id,"token":self.request.input_binding,"input_boot_epoch":self.boot_epoch,"source_version":1}),
            timeout_ms: remaining.max(1),
            operation_id: self.operation_id.clone(),
            parent: None,
        }) {
            Ok(Response::Accepted { id }) => {
                self.pending = Some(id);
                Ok(None)
            }
            _ => Err("source_observation_unavailable"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use amaterasu_contracts::{Grants, RuntimeConfig};
    use amaterasu_runtime::Broker;
    use std::time::Duration;

    fn target() -> LifecycleTarget {
        LifecycleTarget::Stream {
            host_epoch: "host".into(),
            session_id: "session".into(),
            stream_id: "stream".into(),
            generation: 2,
        }
    }
    fn request() -> Request {
        serde_json::from_value(json!({"action":"observe_feedback_source","version":1,"lease":"lease","input_binding":"binding","controller_slot":3})).unwrap()
    }
    fn broker() -> Broker {
        let mut config = RuntimeConfig::default();
        config.principals.insert(
            "amaterasu.stream-session".into(),
            Grants {
                call: ["amaterasu.input/1".into()].into(),
                ..Default::default()
            },
        );
        config.principals.insert(
            "input".into(),
            Grants {
                provide: ["amaterasu.input/1".into()].into(),
                call: [LIFECYCLE.into()].into(),
                ..Default::default()
            },
        );
        config.principals.insert(
            "lifecycle".into(),
            Grants {
                provide: [LIFECYCLE.into()].into(),
                ..Default::default()
            },
        );
        let mut broker = Broker::new(config).unwrap();
        for (principal, service) in [("input", "amaterasu.input/1"), ("lifecycle", LIFECYCLE)] {
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
        broker
    }
    fn start(broker: &mut Broker) -> Job {
        let Response::Accepted { id } = broker.dispatch(
            "amaterasu.stream-session",
            Command::Request {
                service: "amaterasu.input/1".into(),
                payload: json!({}),
                timeout_ms: 5000,
                operation_id: "parent".into(),
                parent: None,
            },
        ) else {
            panic!("parent admission")
        };
        assert!(
            matches!(broker.dispatch("input", Command::Poll {}), Response::Request { id: delivered, .. } if delivered == id)
        );
        Job::new(
            id,
            "amaterasu.stream-session".into(),
            request(),
            target(),
            DeviceObservation {
                device: (7, 0x8000),
                offer: vec!["org.amaterasu.feedback.rumble".into()],
                immutable_controller_policy: true,
            },
            "a".repeat(32),
            Instant::now() + Duration::from_secs(5),
        )
        .unwrap()
    }
    #[test]
    fn source_observation_requires_selected_direct_owner() {
        let request = request();
        assert!(request.authorized(
            "amaterasu.stream-session",
            "amaterasu.stream-session",
            "amaterasu.stream-session"
        ));
        assert!(!request.authorized(
            "amaterasu.stream-session",
            "amaterasu.stream-session",
            "attacker"
        ));
        assert!(!request.authorized("amaterasu.stream-session", "other", "other"));
        assert!(request.authorized("alternate.stream", "alternate.stream", "alternate.stream"));
        assert!(!request.authorized(
            "alternate.stream",
            "amaterasu.stream-session",
            "amaterasu.stream-session"
        ));
        assert!(!request.authorized(
            "alternate.stream",
            "alternate.stream",
            "amaterasu.stream-session"
        ));
    }

    #[test]
    fn actual_broker_preserves_direct_lifecycle_identity_and_scoped_observation() {
        let mut broker = self::broker();
        let mut job = start(&mut broker);
        assert!(
            job.advance(true, &mut |cmd| Ok(broker.dispatch("input", cmd)))
                .unwrap()
                .is_none()
        );
        let Response::Request {
            id,
            caller,
            initiator,
            payload,
            ..
        } = broker.dispatch("lifecycle", Command::Poll {})
        else {
            panic!("lifecycle request")
        };
        assert_eq!(caller, "input");
        assert_eq!(initiator, "input");
        assert_eq!(payload["source_version"], 1);
        assert_eq!(payload["input_boot_epoch"], "a".repeat(32));
        let result = json!({"status":"succeeded","active":true,"target":target(),"source":{"version":1,"id":"b".repeat(32),"generation":2}});
        assert!(matches!(
            broker.dispatch(
                "lifecycle",
                Command::Complete {
                    request: id,
                    payload: result
                }
            ),
            Response::Ok
        ));
        let observed = job
            .advance(true, &mut |cmd| Ok(broker.dispatch("input", cmd)))
            .unwrap()
            .unwrap();
        assert_eq!(observed["source"]["id"], "b".repeat(32));
        assert_eq!(
            observed["device"],
            json!({"slot":3,"generation":7,"capabilities":0x8000})
        );
        assert_eq!(observed["input_boot_epoch"], "a".repeat(32));
        assert!(observed.get("certificate").is_none());
    }
    #[test]
    fn revoked_owner_or_cancelled_parent_cannot_receive_a_source_observation() {
        let mut broker = self::broker();
        let mut job = start(&mut broker);
        assert!(
            job.advance(true, &mut |cmd| Ok(broker.dispatch("input", cmd)))
                .unwrap()
                .is_none()
        );
        assert_eq!(
            job.advance(false, &mut |cmd| Ok(broker.dispatch("input", cmd))),
            Err("source_observation_retired")
        );
        let mut broker = self::broker();
        let mut job = start(&mut broker);
        assert!(matches!(
            broker.dispatch(
                "amaterasu.stream-session",
                Command::Cancel {
                    request: job.request_id
                }
            ),
            Response::Ok
        ));
        assert_eq!(
            job.advance(true, &mut |cmd| Ok(broker.dispatch("input", cmd))),
            Err("source_observation_retired")
        );
    }
    #[test]
    fn delivered_lifecycle_reply_cannot_revive_a_changed_device() {
        let mut broker = self::broker();
        let mut job = start(&mut broker);
        job.advance(true, &mut |cmd| Ok(broker.dispatch("input", cmd)))
            .unwrap();
        let Response::Request { id, .. } = broker.dispatch("lifecycle", Command::Poll {}) else {
            panic!("lifecycle request")
        };
        let reply = json!({"status":"succeeded","active":true,"target":target(),"source":{"version":1,"id":"b".repeat(32),"generation":2}});
        assert!(matches!(
            broker.dispatch(
                "lifecycle",
                Command::Complete {
                    request: id,
                    payload: reply
                }
            ),
            Response::Ok
        ));
        assert_eq!(
            job.advance(false, &mut |cmd| Ok(broker.dispatch("input", cmd))),
            Err("source_observation_retired")
        );
    }

    #[test]
    fn lifecycle_source_from_another_stream_is_rejected() {
        let mut broker = self::broker();
        let mut job = start(&mut broker);
        job.advance(true, &mut |cmd| Ok(broker.dispatch("input", cmd)))
            .unwrap();
        let Response::Request { id, .. } = broker.dispatch("lifecycle", Command::Poll {}) else {
            panic!("lifecycle request")
        };
        let mut wrong = serde_json::to_value(target()).unwrap();
        wrong["stream_id"] = json!("another-stream");
        let reply = json!({"status":"succeeded","active":true,"target":wrong,"source":{"version":1,"id":"b".repeat(32),"generation":2}});
        assert!(matches!(
            broker.dispatch(
                "lifecycle",
                Command::Complete {
                    request: id,
                    payload: reply
                }
            ),
            Response::Ok
        ));
        assert_eq!(
            job.advance(true, &mut |cmd| Ok(broker.dispatch("input", cmd))),
            Err("source_observation_denied")
        );
    }

    #[test]
    fn wrong_target_or_unattested_source_is_rejected() {
        for source in [
            Value::Null,
            json!({"version":1,"id":"b".repeat(32),"generation":3}),
        ] {
            let mut broker = self::broker();
            let mut job = start(&mut broker);
            job.advance(true, &mut |cmd| Ok(broker.dispatch("input", cmd)))
                .unwrap();
            let Response::Request { id, .. } = broker.dispatch("lifecycle", Command::Poll {})
            else {
                panic!("lifecycle request")
            };
            broker.dispatch("lifecycle", Command::Complete { request: id, payload: json!({"status":"succeeded","active":true,"target":target(),"source":source}) });
            assert_eq!(
                job.advance(true, &mut |cmd| Ok(broker.dispatch("input", cmd))),
                Err("source_observation_denied")
            );
        }
    }
}
