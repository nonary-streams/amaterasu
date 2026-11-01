//! Built-in input uses the public SDK source-observation handshake.
pub(crate) use amaterasu_module_sdk::input::source_observation::{DeviceObservation, Job, Request};

#[cfg(test)]
mod tests {
    use super::*;
    use amaterasu_contracts::{Grants, RuntimeConfig};
    use amaterasu_extension_api::LifecycleTarget;
    use amaterasu_module_sdk::{Command, Response};
    use amaterasu_runtime::Broker;
    use serde_json::{Value, json};
    use std::time::{Duration, Instant};

    const LIFECYCLE: &str = "amaterasu.session-lifecycle/1";

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
