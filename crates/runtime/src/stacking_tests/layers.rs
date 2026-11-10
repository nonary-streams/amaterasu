use super::*;
use amaterasu_contracts::{ApiFieldKind, ApiMethod, ApiMethodApproval, ApiPrivilege, ApiSchema};

#[test]
fn a_layer_serves_the_consumers_call_and_forwards_it_as_the_consumer() {
    let mut broker = layered_broker(&["audit"]);
    let now = Instant::now();

    let outer =
        accepted(broker.dispatch_at("consumer", request("call.1", echoed("hi"), None), now));
    assert!(poll(&mut broker, "provider", now).is_none());
    let received = poll(&mut broker, "audit", now).expect("the layer receives the call");
    assert_eq!(received.id, outer);
    assert_eq!(received.caller, "consumer");
    assert_eq!(received.payload, echoed("hi"));

    let forwarded = accepted(broker.dispatch_at(
        "audit",
        request("forward.1", echoed("hi, audited"), Some(received.id)),
        now,
    ));
    let served = poll(&mut broker, "provider", now).expect("the provider receives the forward");
    assert_eq!(served.id, forwarded);
    // The provider authorizes the consumer, exactly as it would unwrapped.
    assert_eq!(served.caller, "consumer");
    assert_eq!(served.initiator, "consumer");
    assert_eq!(served.payload, echoed("hi, audited"));

    assert!(matches!(
        complete(&mut broker, "provider", served.id, echoed("reply")),
        Response::Ok
    ));
    // The layer made the forward, so only the layer reads its result.
    assert_eq!(
        error_code(result(&mut broker, "consumer", forwarded)),
        "permission_denied"
    );
    assert_eq!(
        result_payload(&mut broker, "audit", forwarded),
        echoed("reply")
    );
    assert!(matches!(
        complete(&mut broker, "audit", outer, echoed("reply, audited")),
        Response::Ok
    ));
    assert_eq!(
        result_payload(&mut broker, "consumer", outer),
        echoed("reply, audited")
    );
}

#[test]
fn layers_run_outermost_first() {
    let mut broker = layered_broker(&["outer", "inner"]);
    let now = Instant::now();

    accepted(broker.dispatch_at("consumer", request("call.1", echoed("0"), None), now));
    assert!(poll(&mut broker, "inner", now).is_none());
    let at_outer = poll(&mut broker, "outer", now).unwrap();

    accepted(broker.dispatch_at(
        "outer",
        request("forward.outer", echoed("1"), Some(at_outer.id)),
        now,
    ));
    assert!(poll(&mut broker, "provider", now).is_none());
    let at_inner = poll(&mut broker, "inner", now).unwrap();
    assert_eq!(at_inner.caller, "consumer");
    assert_eq!(at_inner.payload, echoed("1"));

    accepted(broker.dispatch_at(
        "inner",
        request("forward.inner", echoed("2"), Some(at_inner.id)),
        now,
    ));
    let at_provider = poll(&mut broker, "provider", now).unwrap();
    assert_eq!(at_provider.caller, "consumer");
    assert_eq!(at_provider.initiator, "consumer");
    assert_eq!(at_provider.payload, echoed("2"));
}

#[test]
fn a_call_the_layer_makes_for_itself_shows_the_layer() {
    let mut broker = layered_broker(&["audit"]);
    let now = Instant::now();

    accepted(broker.dispatch_at("audit", request("warm.cache", echoed("x"), None), now));
    let served = poll(&mut broker, "provider", now).unwrap();
    assert_eq!(served.caller, "audit");
    assert_eq!(served.initiator, "audit");
}

#[test]
fn a_request_for_another_service_does_not_lend_its_caller() {
    const OTHER: &str = "example.other/1";
    let mut config = layered_config(&["audit"]);
    config
        .principals
        .insert("consumer".into(), caller_of(&[ECHO, OTHER]));
    config.principals.insert(
        "audit".into(),
        Grants {
            provide: names(&[OTHER]),
            call: names(&[ECHO]),
            ..Default::default()
        },
    );
    let mut broker = Broker::new(config).unwrap();
    for (principal, service) in [("provider", ECHO), ("audit", ECHO), ("audit", OTHER)] {
        assert!(matches!(
            register(&mut broker, principal, service),
            Response::Ok
        ));
    }
    let now = Instant::now();

    accepted(broker.dispatch_at(
        "consumer",
        Command::Request {
            service: OTHER.into(),
            payload: json!({}),
            timeout_ms: 1_000,
            operation_id: "other.1".into(),
            parent: None,
        },
        now,
    ));
    let received = poll(&mut broker, "audit", now).unwrap();
    accepted(broker.dispatch_at(
        "audit",
        request("delegated.1", echoed("x"), Some(received.id)),
        now,
    ));
    let served = poll(&mut broker, "provider", now).unwrap();
    // Ordinary delegation: the layer is the caller, the consumer the initiator.
    assert_eq!(served.caller, "audit");
    assert_eq!(served.initiator, "consumer");
}

#[test]
fn a_call_is_refused_until_the_whole_stack_has_registered() {
    let mut broker = Broker::new(layered_config(&["outer", "inner"])).unwrap();
    let now = Instant::now();
    let mut attempt = 0;
    let mut call = |broker: &mut Broker, principal: &str| {
        attempt += 1;
        broker.dispatch_at(
            principal,
            request(&format!("call.{attempt}"), echoed("x"), None),
            now,
        )
    };

    assert_eq!(error_code(call(&mut broker, "consumer")), "unavailable");
    register(&mut broker, "provider", ECHO);
    // The provider alone must not be reachable around its layers.
    assert_eq!(error_code(call(&mut broker, "consumer")), "unavailable");
    register(&mut broker, "outer", ECHO);
    assert_eq!(error_code(call(&mut broker, "consumer")), "unavailable");
    assert_eq!(error_code(call(&mut broker, "outer")), "unavailable");
    register(&mut broker, "inner", ECHO);

    accepted(call(&mut broker, "consumer"));
    assert!(poll(&mut broker, "outer", now).is_some());
    assert!(poll(&mut broker, "provider", now).is_none());
}

#[test]
fn a_layer_without_its_provider_is_unavailable() {
    let mut broker = Broker::new(layered_config(&["audit"])).unwrap();
    register(&mut broker, "audit", ECHO);
    assert_eq!(
        error_code(broker.dispatch("consumer", request("call.1", echoed("x"), None))),
        "unavailable"
    );
    assert_eq!(
        error_code(broker.dispatch("audit", request("call.2", echoed("x"), None))),
        "unavailable"
    );
}

#[test]
fn only_a_listed_layer_registers_without_a_provide_grant() {
    let mut broker = Broker::new(layered_config(&["audit"])).unwrap();
    assert!(matches!(register(&mut broker, "audit", ECHO), Response::Ok));
    assert_eq!(
        error_code(register(&mut broker, "outsider", ECHO)),
        "permission_denied"
    );
    assert_eq!(
        error_code(register(&mut broker, "audit", "example.other/1")),
        "permission_denied"
    );
    // Registering again is harmless, as it is for a provider.
    assert!(matches!(register(&mut broker, "audit", ECHO), Response::Ok));
}

#[test]
fn ending_the_consumers_request_ends_what_the_layer_forwarded() {
    let mut broker = layered_broker(&["audit"]);
    let now = Instant::now();

    let outer = accepted(broker.dispatch_at("consumer", request("call.1", echoed("x"), None), now));
    let received = poll(&mut broker, "audit", now).unwrap();
    let forwarded = accepted(broker.dispatch_at(
        "audit",
        request("forward.1", echoed("x"), Some(received.id)),
        now,
    ));
    let served = poll(&mut broker, "provider", now).unwrap();

    assert!(matches!(
        broker.dispatch_at("consumer", Command::Cancel { request: outer }, now),
        Response::Ok
    ));
    assert_eq!(
        error_code(broker.dispatch_at(
            "provider",
            Command::CheckRequest { request: served.id },
            now
        )),
        "request_ended"
    );
    assert_eq!(
        error_code(result(&mut broker, "audit", forwarded)),
        "cancelled"
    );
}

#[test]
fn a_direct_call_follows_the_same_stack() {
    let mut broker = Broker::new(layered_config(&["audit"])).unwrap();
    assert_eq!(
        error_code(broker.direct_call_route("consumer", ECHO, 2).err().unwrap()),
        "unavailable"
    );
    register(&mut broker, "provider", ECHO);
    register(&mut broker, "audit", ECHO);

    let from_consumer = broker.direct_call_route("consumer", ECHO, 2).unwrap();
    assert_eq!(from_consumer.provider, "audit");
    assert!(!from_consumer.caller_is_layer);
    let from_layer = broker.direct_call_route("audit", ECHO, 2).unwrap();
    assert_eq!(from_layer.provider, "provider");
    assert!(from_layer.caller_is_layer);
    assert_eq!(
        error_code(broker.direct_call_route("outsider", ECHO, 2).err().unwrap()),
        "permission_denied"
    );
}

fn paint_method() -> ApiMethod {
    ApiMethod {
        service: "org.example.paint/1".into(),
        method: "render".into(),
        interface_major: 1,
        interface_minor: 0,
        request: ApiSchema {
            required: [("size".into(), ApiFieldKind::Integer)].into(),
            optional: Default::default(),
            allow_extra: false,
        },
        response: ApiSchema {
            required: [("done".into(), ApiFieldKind::Boolean)].into(),
            optional: Default::default(),
            allow_extra: false,
        },
        privilege: ApiPrivilege::User,
    }
}

fn call_paint(operation: &str, size: u64, parent: Option<u64>) -> Command {
    Command::CallApi {
        service: "org.example.paint/1".into(),
        method: "render".into(),
        interface_major: 1,
        interface_minor: 0,
        payload: json!({ "size": size }),
        timeout_ms: 1_000,
        operation_id: operation.into(),
        parent,
    }
}

#[test]
fn a_layer_wraps_an_installed_api_method_as_an_approved_caller() {
    const PAINT: &str = "org.example.paint/1";
    let mut config = RuntimeConfig::default();
    config
        .principals
        .insert("org.example.painter".into(), provider_of(&[PAINT]));
    for caller in ["org.example.client", "org.example.limiter"] {
        config.principals.insert(caller.into(), caller_of(&[PAINT]));
    }
    config.api_methods.push(ApiMethodApproval {
        provider: "org.example.painter".into(),
        callers: names(&["org.example.client", "org.example.limiter"]),
        method: paint_method(),
    });
    config
        .service_layers
        .insert(PAINT.into(), vec!["org.example.limiter".into()]);
    let mut broker = Broker::new(config).unwrap();
    assert!(matches!(
        broker.dispatch(
            "org.example.painter",
            Command::RegisterApi {
                method: paint_method()
            }
        ),
        Response::Ok
    ));
    assert!(matches!(
        register(&mut broker, "org.example.limiter", PAINT),
        Response::Ok
    ));
    let now = Instant::now();

    let outer =
        accepted(broker.dispatch_at("org.example.client", call_paint("paint.1", 900, None), now));
    let received = poll(&mut broker, "org.example.limiter", now).unwrap();
    assert_eq!(received.caller, "org.example.client");
    assert_eq!(received.payload["arguments"], json!({ "size": 900 }));

    // The forward is checked against the installed schema like any other call.
    let forwarded = accepted(broker.dispatch_at(
        "org.example.limiter",
        call_paint("forward.1", 64, Some(received.id)),
        now,
    ));
    let served = poll(&mut broker, "org.example.painter", now).unwrap();
    assert_eq!(served.caller, "org.example.client");
    assert_eq!(served.payload["arguments"], json!({ "size": 64 }));

    assert!(matches!(
        complete(
            &mut broker,
            "org.example.painter",
            served.id,
            json!({ "done": true })
        ),
        Response::Ok
    ));
    assert_eq!(
        result_payload(&mut broker, "org.example.limiter", forwarded),
        json!({ "done": true })
    );
    // The layer's own reply must match the method's response schema too.
    assert_eq!(
        error_code(complete(
            &mut broker,
            "org.example.limiter",
            outer,
            json!({ "finished": true })
        )),
        "invalid_request"
    );
    assert!(matches!(
        complete(
            &mut broker,
            "org.example.limiter",
            outer,
            json!({ "done": true })
        ),
        Response::Ok
    ));
}

#[test]
fn layer_policy_must_name_distinct_known_principals_that_do_not_provide_the_service() {
    let rejected = |edit: fn(&mut RuntimeConfig)| {
        let mut config = layered_config(&["audit"]);
        edit(&mut config);
        Broker::new(config).is_err()
    };
    assert!(Broker::new(layered_config(&["audit"])).is_ok());
    assert!(rejected(|config| {
        config
            .service_layers
            .insert(ECHO.into(), vec!["missing".into()]);
    }));
    assert!(rejected(|config| {
        config
            .service_layers
            .insert(ECHO.into(), vec!["audit".into(), "audit".into()]);
    }));
    assert!(rejected(|config| {
        config
            .service_layers
            .insert(ECHO.into(), vec!["provider".into()]);
    }));
    assert!(rejected(|config| {
        config
            .service_layers
            .insert("hooks/prepare".into(), vec!["audit".into()]);
    }));
    assert!(rejected(|config| {
        config.service_layers.insert(
            amaterasu_contracts::HOST_LOG_SERVICE.into(),
            vec!["audit".into()],
        );
    }));
    assert!(rejected(|config| {
        let layers = (0..9)
            .map(|index| format!("layer.{index}"))
            .collect::<Vec<_>>();
        for layer in &layers {
            config.principals.insert(layer.clone(), caller_of(&[ECHO]));
        }
        config.service_layers.insert(ECHO.into(), layers);
    }));
}

#[test]
fn layer_policy_survives_serialization_and_is_omitted_when_empty() {
    let plain = serde_json::to_value(RuntimeConfig::default()).unwrap();
    assert!(plain.get("service_layers").is_none());

    let layered = serde_json::to_value(layered_config(&["audit"])).unwrap();
    assert_eq!(layered["service_layers"], json!({ ECHO: ["audit"] }));
    let restored: RuntimeConfig = serde_json::from_value(layered).unwrap();
    assert_eq!(restored.service_layers[ECHO], vec!["audit".to_owned()]);
}
