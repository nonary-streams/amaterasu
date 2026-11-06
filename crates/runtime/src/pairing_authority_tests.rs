use crate::Broker;
use amaterasu_contracts::{Command, Grants, Response, RuntimeConfig};
use amaterasu_module_sdk::{
    pairing_authority::{self, Authorization, Request, SERVICE},
    requests::RequestOptions,
};
use serde_json::{Value, json};

fn broker(selected: &str) -> Broker {
    let mut config = RuntimeConfig::default();
    config.principals.insert(
        "session-consumer".into(),
        Grants {
            call: [SERVICE.into()].into(),
            ..Default::default()
        },
    );
    for principal in ["builtin-authority", "replacement-authority", "outsider"] {
        config.principals.insert(
            principal.into(),
            Grants {
                provide: if principal == selected {
                    [SERVICE.into()].into()
                } else {
                    Default::default()
                },
                ..Default::default()
            },
        );
    }
    Broker::new(config).unwrap()
}

fn request(fingerprint: &str, operation_id: &str) -> Command {
    pairing_authority::command(
        RequestOptions {
            service: SERVICE,
            timeout_ms: 1_000,
            operation_id,
            parent: None,
        },
        &Request::ClientAuthorization {
            certificate_id: fingerprint.into(),
        },
    )
    .unwrap()
}

fn accepted(response: Response) -> u64 {
    let Response::Accepted { id } = response else {
        panic!("expected accepted request: {response:?}")
    };
    id
}

fn read_authority(
    broker: &mut Broker,
    provider: &str,
    fingerprint: &str,
    respond: impl FnOnce(Request) -> Value,
) -> Authorization {
    let id = accepted(broker.dispatch("session-consumer", request(fingerprint, "membership.read")));
    assert!(matches!(
        broker.dispatch("outsider", Command::TakeResult { request: id }),
        Response::Error { code, .. } if code == "permission_denied"
    ));
    let Response::Request {
        id: delivered,
        caller,
        initiator,
        payload,
        ..
    } = broker.dispatch(provider, Command::Poll {})
    else {
        panic!("selected provider did not receive request")
    };
    assert_eq!(delivered, id);
    assert_eq!(caller, "session-consumer");
    assert_eq!(initiator, "session-consumer");
    assert!(matches!(
        broker.dispatch(provider, Command::CheckRequest { request: id }),
        Response::Ok
    ));
    let operation: Request = serde_json::from_value(payload).unwrap();
    operation.validate().unwrap();
    assert!(matches!(
        broker.dispatch(
            provider,
            Command::Complete {
                request: id,
                payload: respond(operation)
            }
        ),
        Response::Ok
    ));
    let Response::Result { payload } =
        broker.dispatch("session-consumer", Command::TakeResult { request: id })
    else {
        panic!("consumer did not receive result")
    };
    let observation = Authorization::parse(fingerprint, &payload).unwrap();
    assert!(matches!(
        broker.dispatch("session-consumer", Command::ReleaseResult { request: id }),
        Response::Ok
    ));
    observation
}

#[test]
fn pairing_authority_sdk_consumer_works_unchanged_with_selected_replacement_provider() {
    let fingerprint = "ab".repeat(32);
    for (provider, paired) in [
        ("builtin-authority", true),
        ("replacement-authority", false),
    ] {
        let mut broker = broker(provider);
        let unselected = if provider == "builtin-authority" {
            "replacement-authority"
        } else {
            "builtin-authority"
        };
        assert!(matches!(
            broker.dispatch(unselected, Command::Register { service: SERVICE.into() }),
            Response::Error { code, .. } if code == "permission_denied"
        ));
        assert!(matches!(
            broker.dispatch(
                provider,
                Command::Register {
                    service: SERVICE.into()
                }
            ),
            Response::Ok
        ));
        let actual = read_authority(&mut broker, provider, &fingerprint, |operation| {
            let Request::ClientAuthorization { certificate_id } = operation;
            json!({"ok":true,"certificate_id":certificate_id,"paired":paired,"may_view":paired,"may_launch":false,"may_terminate":false,"revision":9,"name":paired.then_some("Player")})
        });
        assert_eq!(actual.paired, paired);
        assert_eq!(actual.may_view, paired);
        assert!(matches!(
            broker.dispatch(unselected, Command::Poll {}),
            Response::Pending
        ));
    }
}

#[test]
fn pairing_authority_has_no_missing_provider_fallback_and_preserves_cancel_ownership() {
    let fingerprint = "ab".repeat(32);
    let mut broker = broker("builtin-authority");
    assert!(matches!(
        broker.dispatch("session-consumer", request(&fingerprint, "missing.provider")),
        Response::Error { code, .. } if code == "unavailable"
    ));
    assert!(matches!(
        broker.dispatch(
            "builtin-authority",
            Command::Register {
                service: SERVICE.into()
            }
        ),
        Response::Ok
    ));
    let id = accepted(broker.dispatch("session-consumer", request(&fingerprint, "cancelled.read")));
    assert!(
        matches!(broker.dispatch("builtin-authority", Command::Poll {}), Response::Request { id: delivered, .. } if delivered == id)
    );
    assert!(matches!(
        broker.dispatch("outsider", Command::Cancel { request: id }),
        Response::Error { code, .. } if code == "permission_denied"
    ));
    assert!(matches!(
        broker.dispatch("session-consumer", Command::Cancel { request: id }),
        Response::Ok
    ));
    assert!(matches!(
        broker.dispatch("builtin-authority", Command::CheckRequest { request: id }),
        Response::Error { code, .. } if code == "request_ended"
    ));
    assert!(matches!(
        broker.dispatch("session-consumer", Command::ReleaseResult { request: id }),
        Response::Ok
    ));
}
