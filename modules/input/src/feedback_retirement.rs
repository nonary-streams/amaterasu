//! Built-in input uses the public SDK retirement handshake.
pub(crate) use amaterasu_module_sdk::input::feedback_retirement::Job;

#[cfg(test)]
mod tests {
    use super::*;
    use amaterasu_contracts::{Grants, RuntimeConfig};
    use amaterasu_extension_api::LifecycleTarget;
    use amaterasu_module_sdk::{Command, Response};
    use amaterasu_runtime::Broker;
    use serde_json::{Value, json};

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
