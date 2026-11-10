use super::*;

fn refuse(broker: &mut Broker, principal: &str, request: u64, code: &str) -> Response {
    broker.dispatch(
        principal,
        Command::Refuse {
            request,
            code: code.into(),
            message: "not during a stream".into(),
        },
    )
}

#[test]
fn a_refusal_reaches_the_requester_under_the_refused_prefix() {
    let mut broker = layered_broker(&[]);
    let now = Instant::now();
    let outer = accepted(broker.dispatch_at("consumer", request("call.1", echoed("x"), None), now));
    let served = poll(&mut broker, "provider", now).unwrap();

    assert!(matches!(
        refuse(&mut broker, "provider", served.id, "busy"),
        Response::Ok
    ));
    match result(&mut broker, "consumer", outer) {
        Response::Error { code, message } => {
            assert_eq!(code, "refused.busy");
            assert_eq!(message, "not during a stream");
        }
        other => panic!("{other:?}"),
    }
    // The request has ended: nothing can replace the refusal.
    assert_eq!(
        error_code(complete(&mut broker, "provider", served.id, echoed("late"))),
        "request_ended"
    );
    assert_eq!(
        error_code(refuse(&mut broker, "provider", served.id, "busy")),
        "request_ended"
    );
    assert!(matches!(
        broker.dispatch("consumer", Command::ReleaseResult { request: outer }),
        Response::Ok
    ));
}

#[test]
fn only_the_serving_principal_refuses_a_request_it_was_delivered() {
    let mut broker = layered_broker(&[]);
    let now = Instant::now();
    let outer = accepted(broker.dispatch_at("consumer", request("call.1", echoed("x"), None), now));

    // Not delivered yet.
    assert_eq!(
        error_code(refuse(&mut broker, "provider", outer, "busy")),
        "permission_denied"
    );
    let served = poll(&mut broker, "provider", now).unwrap();
    for stranger in ["consumer", "outsider"] {
        assert_eq!(
            error_code(refuse(&mut broker, stranger, served.id, "busy")),
            "permission_denied"
        );
    }
    assert_eq!(
        error_code(refuse(&mut broker, "provider", served.id + 100, "busy")),
        "not_found"
    );
    assert!(matches!(
        result(&mut broker, "consumer", outer),
        Response::Pending
    ));
}

#[test]
fn an_invalid_refusal_leaves_the_request_live() {
    let mut broker = layered_broker(&[]);
    let now = Instant::now();
    let outer = accepted(broker.dispatch_at("consumer", request("call.1", echoed("x"), None), now));
    let served = poll(&mut broker, "provider", now).unwrap();

    let long_code = "x".repeat(65);
    for code in ["", "Busy", "refused.busy", long_code.as_str()] {
        assert_eq!(
            error_code(refuse(&mut broker, "provider", served.id, code)),
            "invalid_request",
            "{code}"
        );
    }
    for message in ["line\nbreak".to_owned(), "m".repeat(513)] {
        assert_eq!(
            error_code(broker.dispatch(
                "provider",
                Command::Refuse {
                    request: served.id,
                    code: "busy".into(),
                    message,
                },
            )),
            "invalid_request"
        );
    }
    assert!(matches!(
        complete(&mut broker, "provider", served.id, echoed("done")),
        Response::Ok
    ));
    assert_eq!(
        result_payload(&mut broker, "consumer", outer),
        echoed("done")
    );
}

#[test]
fn a_layer_refuses_the_consumer_without_reaching_the_provider() {
    let mut broker = layered_broker(&["gate"]);
    let now = Instant::now();
    let outer = accepted(broker.dispatch_at("consumer", request("call.1", echoed("x"), None), now));
    let received = poll(&mut broker, "gate", now).unwrap();

    assert!(matches!(
        refuse(&mut broker, "gate", received.id, "permission_denied"),
        Response::Ok
    ));
    assert_eq!(
        error_code(result(&mut broker, "consumer", outer)),
        "refused.permission_denied"
    );
    assert!(poll(&mut broker, "provider", now).is_none());
}
