use super::Broker;
use amaterasu_contracts::*;
use serde_json::json;
use std::collections::BTreeSet;
use std::time::{Duration, Instant};

fn names(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|s| (*s).into()).collect()
}
fn test_broker() -> Broker {
    let mut config = RuntimeConfig::default();
    config.principals.insert(
        "client".into(),
        Grants {
            call: names(&["echo/1"]),
            subscribe: names(&["changed/1"]),
            ..Default::default()
        },
    );
    config.principals.insert(
        "provider".into(),
        Grants {
            provide: names(&["echo/1"]),
            call: names(&["secret/1"]),
            publish: names(&["changed/1"]),
            hook: names(&["prepare/1"]),
            ..Default::default()
        },
    );
    config.principals.insert(
        "secret".into(),
        Grants {
            provide: names(&["secret/1"]),
            ..Default::default()
        },
    );
    config
        .principals
        .insert("outsider".into(), Grants::default());
    config.hooks.insert(
        "prepare/1".into(),
        HookSpec {
            owner: "client".into(),
            participants: vec!["provider".into()],
            timeout_ms: 100,
        },
    );
    let mut broker = Broker::new(config).unwrap();
    broker.dispatch(
        "provider",
        Command::Register {
            service: "echo/1".into(),
        },
    );
    broker.dispatch(
        "secret",
        Command::Register {
            service: "secret/1".into(),
        },
    );
    broker
}

#[test]
fn host_state_context_is_available_only_to_trusted_state_consumers() {
    let mut configuration = RuntimeConfig::default();
    configuration
        .principals
        .insert("amaterasu.state".into(), Grants::default());
    configuration
        .principals
        .insert("amaterasu.audio".into(), Grants::default());
    configuration
        .principals
        .insert("other".into(), Grants::default());
    configuration.host_context = RuntimeHostContext {
        schema: 1,
        state_root: Some(r"C:\\ProgramData\\Amaterasu\\runtime\\user-state\\S-1-5-21-1".into()),
    };
    let state_root = configuration.host_context.state_root.clone();
    let mut broker = Broker::new(configuration).unwrap();
    let Response::Result { payload } =
        broker.dispatch("amaterasu.state", Command::GetHostContext {})
    else {
        panic!("state service did not receive host context");
    };
    assert_eq!(payload["schema"], 1);
    let audio_context = broker.dispatch("amaterasu.audio", Command::GetHostContext {});
    if cfg!(target_os = "linux") {
        let Response::Result { payload } = audio_context else {
            panic!("Linux audio did not receive its host-owned state root");
        };
        assert_eq!(payload["state_root"].as_str(), state_root.as_deref());
    } else {
        assert!(matches!(
            audio_context,
            Response::Error { code, .. } if code == "permission_denied"
        ));
    }
    assert!(matches!(
        broker.dispatch("other", Command::GetHostContext {}),
        Response::Error { code, .. } if code == "permission_denied"
    ));
}
fn request(operation: &str) -> Command {
    Command::Request {
        service: "echo/1".into(),
        payload: json!({"x":1}),
        timeout_ms: 100,
        operation_id: operation.into(),
        parent: None,
    }
}
fn accepted_id(request_id: Response) -> u64 {
    if let Response::Accepted { id } = request_id {
        id
    } else {
        panic!("{request_id:?}")
    }
}

fn synthetic_api(privilege: ApiPrivilege) -> (RuntimeConfig, ApiMethod) {
    let service = "org.example.paint/1";
    let method = ApiMethod {
        service: service.into(),
        method: "render".into(),
        interface_major: 1,
        interface_minor: 2,
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
        privilege,
    };
    let mut config = RuntimeConfig::default();
    config.principals.insert(
        "org.example.provider".into(),
        Grants {
            provide: names(&[service]),
            ..Default::default()
        },
    );
    for caller in ["org.example.client", "org.example.outsider"] {
        config.principals.insert(
            caller.into(),
            Grants {
                call: names(&[service]),
                ..Default::default()
            },
        );
    }
    config.api_methods.push(ApiMethodApproval {
        provider: "org.example.provider".into(),
        callers: names(&["org.example.client"]),
        method: method.clone(),
    });
    (config, method)
}

#[test]
fn installed_user_api_methods_enforce_identity_schema_version_and_raw_route_block() {
    let (config, method) = synthetic_api(ApiPrivilege::User);
    let mut broker = Broker::new(config).unwrap();
    let provider = "org.example.provider";
    let client = "org.example.client";
    let outsider = "org.example.outsider";
    assert_error_code(
        broker.dispatch(
            outsider,
            Command::RegisterApi {
                method: method.clone(),
            },
        ),
        "permission_denied",
    );
    let mut altered = method.clone();
    altered.request.allow_extra = true;
    assert_error_code(
        broker.dispatch(provider, Command::RegisterApi { method: altered }),
        "permission_denied",
    );
    assert_error_code(
        broker.dispatch(
            provider,
            Command::Register {
                service: method.service.clone(),
            },
        ),
        "permission_denied",
    );
    assert!(matches!(
        broker.dispatch(
            provider,
            Command::RegisterApi {
                method: method.clone()
            }
        ),
        Response::Ok
    ));
    assert_error_code(
        broker.dispatch(
            client,
            Command::Request {
                service: method.service.clone(),
                payload: json!({"method":"render","arguments":{"size":1}}),
                timeout_ms: 100,
                operation_id: "raw.route".into(),
                parent: None,
            },
        ),
        "permission_denied",
    );
    assert_error_code(
        broker.dispatch(
            outsider,
            Command::QueryApi {
                service: method.service.clone(),
                method: "render".into(),
                interface_major: 1,
                min_minor: 2,
            },
        ),
        "permission_denied",
    );
    assert!(matches!(broker.dispatch(client, Command::QueryApi {
        service: method.service.clone(), method:"render".into(), interface_major:1, min_minor:2,
    }), Response::Result { payload } if payload == serde_json::to_value(&method).unwrap()));
    assert_error_code(
        broker.dispatch(
            client,
            Command::QueryApi {
                service: method.service.clone(),
                method: "render".into(),
                interface_major: 1,
                min_minor: 3,
            },
        ),
        "incompatible_api",
    );
    let call = |payload, minor, operation: &str| Command::CallApi {
        service: method.service.clone(),
        method: "render".into(),
        interface_major: 1,
        interface_minor: minor,
        payload,
        timeout_ms: 100,
        operation_id: operation.into(),
        parent: None,
    };
    assert_error_code(
        broker.dispatch(client, call(json!({"size":"1"}), 2, "bad.shape")),
        "invalid_request",
    );
    assert_error_code(
        broker.dispatch(client, call(json!({"size":1}), 1, "bad.version")),
        "incompatible_api",
    );
    assert_error_code(
        broker.dispatch(outsider, call(json!({"size":1}), 2, "bad.caller")),
        "permission_denied",
    );
    let id = accepted_id(broker.dispatch(client, call(json!({"size":4}), 2, "valid.call")));
    assert!(
        matches!(broker.dispatch(provider, Command::Poll {}), Response::Request { payload, .. }
        if payload == json!({"method":"render","interface_major":1,"interface_minor":2,"arguments":{"size":4}}))
    );
    assert_error_code(
        broker.dispatch(
            provider,
            Command::Complete {
                request: id,
                payload: json!({"done":"yes"}),
            },
        ),
        "invalid_request",
    );
    assert!(matches!(
        broker.dispatch(
            provider,
            Command::Complete {
                request: id,
                payload: json!({"done":true})
            }
        ),
        Response::Ok
    ));
    assert!(
        matches!(broker.dispatch(client, Command::TakeResult { request:id }), Response::Result { payload } if payload == json!({"done":true}))
    );
}

#[test]
fn publisher_approval_never_registers_an_elevated_handler_in_user_broker() {
    let (config, method) = synthetic_api(ApiPrivilege::Elevated);
    let mut broker = Broker::new(config).unwrap();
    assert_error_code(
        broker.dispatch(
            "org.example.provider",
            Command::RegisterApi {
                method: method.clone(),
            },
        ),
        "permission_denied",
    );
    assert_error_code(
        broker.dispatch(
            "org.example.client",
            Command::CallApi {
                service: method.service,
                method: method.method,
                interface_major: 1,
                interface_minor: 2,
                payload: json!({"size":1}),
                timeout_ms: 100,
                operation_id: "elevated.denied".into(),
                parent: None,
            },
        ),
        "permission_denied",
    );
}

#[test]
fn dynamic_api_policy_cannot_shadow_a_core_service() {
    for reserved in ["amaterasu.authentication/1", "hooks/org.example.paint/1"] {
        let (mut config, mut method) = synthetic_api(ApiPrivilege::User);
        let original = method.service.clone();
        method.service = reserved.into();
        for grants in config.principals.values_mut() {
            if grants.provide.remove(&original) {
                grants.provide.insert(method.service.clone());
            }
            if grants.call.remove(&original) {
                grants.call.insert(method.service.clone());
            }
        }
        config.api_methods[0].method = method;
        assert!(Broker::new(config).is_err());
    }
}

#[test]
fn delegated_api_call_requires_approval_for_the_original_initiator() {
    let (mut config, method) = synthetic_api(ApiPrivilege::User);
    let intermediary = "org.example.intermediary";
    config.principals.insert(
        intermediary.into(),
        Grants {
            provide: names(&["org.example.forward/1"]),
            call: names(&[&method.service]),
            ..Default::default()
        },
    );
    for principal in ["org.example.client", "org.example.outsider"] {
        config
            .principals
            .get_mut(principal)
            .unwrap()
            .call
            .insert("org.example.forward/1".into());
    }
    config.api_methods[0].callers.insert(intermediary.into());
    let mut broker = Broker::new(config).unwrap();
    assert!(matches!(
        broker.dispatch(
            "org.example.provider",
            Command::RegisterApi {
                method: method.clone()
            }
        ),
        Response::Ok
    ));
    assert!(matches!(
        broker.dispatch(
            intermediary,
            Command::Register {
                service: "org.example.forward/1".into()
            }
        ),
        Response::Ok
    ));
    let forward = |operation: &str| Command::Request {
        service: "org.example.forward/1".into(),
        payload: json!({}),
        timeout_ms: 100,
        operation_id: operation.into(),
        parent: None,
    };
    let bad_parent = accepted_id(broker.dispatch("org.example.outsider", forward("outsider")));
    assert!(matches!(
        broker.dispatch(intermediary, Command::Poll {}),
        Response::Request { id, .. } if id == bad_parent
    ));
    let api_call = |parent| Command::CallApi {
        service: method.service.clone(),
        method: method.method.clone(),
        interface_major: 1,
        interface_minor: 2,
        payload: json!({"size": 1}),
        timeout_ms: 100,
        operation_id: format!("api.{parent}"),
        parent: Some(parent),
    };
    assert_error_code(
        broker.dispatch(intermediary, api_call(bad_parent)),
        "permission_denied",
    );
    let good_parent = accepted_id(broker.dispatch("org.example.client", forward("client")));
    assert!(matches!(
        broker.dispatch(intermediary, Command::Poll {}),
        Response::Request { id, .. } if id == good_parent
    ));
    assert!(matches!(
        broker.dispatch(intermediary, api_call(good_parent)),
        Response::Accepted { .. }
    ));
}
fn assert_error_code(request_id: Response, expected: &str) {
    assert!(
        matches!(request_id, Response::Error { ref code, .. } if code == expected),
        "{request_id:?}"
    );
}

#[test]
fn permissions_are_separate_and_identity_is_bound() {
    let mut broker = test_broker();
    assert_error_code(
        broker.dispatch("outsider", request("op")),
        "permission_denied",
    );
    assert_error_code(
        broker.dispatch(
            "client",
            Command::Publish {
                topic: "changed/1".into(),
                payload: json!(1),
            },
        ),
        "permission_denied",
    );
    assert_error_code(
        broker.dispatch(
            "provider",
            Command::Subscribe {
                topic: "changed/1".into(),
                capacity: 1,
                coalesce: false,
            },
        ),
        "permission_denied",
    );
    assert_error_code(
        broker.dispatch(
            "client",
            Command::Register {
                service: "echo/1".into(),
            },
        ),
        "permission_denied",
    );
    assert!(serde_json::from_value::<Command>(json!({"op":"poll","caller":"provider"})).is_err());
}

#[test]
fn completed_results_are_released_only_by_the_request_owner() {
    let mut broker = test_broker();
    let id = accepted_id(broker.dispatch("client", request("release")));
    assert_error_code(
        broker.dispatch("provider", Command::ReleaseResult { request: id }),
        "permission_denied",
    );
    assert_error_code(
        broker.dispatch("client", Command::ReleaseResult { request: id }),
        "request_pending",
    );
    broker.dispatch("provider", Command::Poll {});
    broker.dispatch(
        "provider",
        Command::Complete {
            request: id,
            payload: json!("done"),
        },
    );
    assert_error_code(
        broker.dispatch("outsider", Command::ReleaseResult { request: id }),
        "permission_denied",
    );
    assert!(matches!(
        broker.dispatch("client", Command::ReleaseResult { request: id }),
        Response::Ok
    ));
    assert_error_code(
        broker.dispatch("client", Command::TakeResult { request: id }),
        "not_found",
    );
    // Explicit release relinquishes retry protection. Reusing an operation ID
    // intentionally starts a new request, with a new opaque request handle.
    let replacement = accepted_id(broker.dispatch("client", request("release")));
    assert!(replacement > id);
}

#[test]
fn consumed_or_cancelled_calls_do_not_exhaust_long_running_capacity() {
    let mut broker = test_broker();
    let now = Instant::now();
    for index in 0..2000 {
        let id = accepted_id(broker.dispatch_at("client", request(&format!("call.{index}")), now));
        if index % 2 == 0 {
            broker.dispatch_at("provider", Command::Poll {}, now);
            broker.dispatch_at(
                "provider",
                Command::Complete {
                    request: id,
                    payload: json!(index),
                },
                now,
            );
            assert!(matches!(
                broker.dispatch_at("client", Command::TakeResult { request: id }, now),
                Response::Result { .. }
            ));
        } else {
            broker.dispatch_at("client", Command::Cancel { request: id }, now);
        }
        assert!(matches!(
            broker.dispatch_at("client", Command::ReleaseResult { request: id }, now),
            Response::Ok
        ));
    }
    assert!(matches!(
        broker.dispatch_at("provider", Command::Poll {}, now),
        Response::Pending
    ));
}
#[test]
fn completion_and_results_have_distinct_owners() {
    let mut broker = test_broker();
    let request_id = accepted_id(broker.dispatch("client", request("op")));
    assert_error_code(
        broker.dispatch(
            "outsider",
            Command::Complete {
                request: request_id,
                payload: json!(2),
            },
        ),
        "permission_denied",
    );
    assert_error_code(
        broker.dispatch(
            "provider",
            Command::Complete {
                request: request_id,
                payload: json!(2),
            },
        ),
        "permission_denied",
    );
    assert!(
        matches!(broker.dispatch("provider", Command::Poll {}), Response::Request { initiator, .. } if initiator == "client")
    );
    assert!(matches!(
        broker.dispatch(
            "provider",
            Command::Complete {
                request: request_id,
                payload: json!(2)
            }
        ),
        Response::Ok
    ));
    assert_error_code(
        broker.dispatch(
            "outsider",
            Command::TakeResult {
                request: request_id,
            },
        ),
        "permission_denied",
    );
    assert!(
        matches!(broker.dispatch("client", Command::TakeResult { request: request_id }), Response::Result { payload } if payload == 2)
    );
}
#[test]
fn duplicate_operations_do_not_repeat_mutations() {
    let mut broker = test_broker();
    let first = accepted_id(broker.dispatch("client", request("same")));
    assert_eq!(
        accepted_id(broker.dispatch("client", request("same"))),
        first
    );
    let mut conflicting = request("same");
    if let Command::Request {
        ref mut payload, ..
    } = conflicting
    {
        *payload = json!(2);
    }
    assert_error_code(broker.dispatch("client", conflicting), "conflict");
    broker.dispatch("provider", Command::Poll {});
    assert!(matches!(
        broker.dispatch("provider", Command::Poll {}),
        Response::Pending
    ));
}
#[test]
fn cancellation_and_deadlines_reject_late_completion() {
    let mut broker = test_broker();
    let start = Instant::now();
    let request_id = accepted_id(broker.dispatch_at("client", request("op"), start));
    broker.dispatch_at("provider", Command::Poll {}, start);
    assert_error_code(
        broker.dispatch_at(
            "provider",
            Command::Complete {
                request: request_id,
                payload: json!(1),
            },
            start + Duration::from_millis(101),
        ),
        "request_ended",
    );
    assert_error_code(
        broker.dispatch(
            "client",
            Command::TakeResult {
                request: request_id,
            },
        ),
        "deadline_exceeded",
    );
    let request_id = accepted_id(broker.dispatch("client", request("cancel")));
    broker.dispatch(
        "client",
        Command::Cancel {
            request: request_id,
        },
    );
    assert!(matches!(
        broker.dispatch("provider", Command::Poll {}),
        Response::Pending
    ));
}
#[test]
fn delegated_calls_cannot_widen_initiator_authority() {
    let mut broker = test_broker();
    let parent = accepted_id(broker.dispatch("client", request("op")));
    broker.dispatch("provider", Command::Poll {});
    assert_error_code(
        broker.dispatch(
            "provider",
            Command::Request {
                service: "secret/1".into(),
                payload: json!(null),
                timeout_ms: 100,
                operation_id: "delegated".into(),
                parent: Some(parent),
            },
        ),
        "permission_denied",
    );
}
#[test]
fn overflow_requires_explicit_resynchronization() {
    let mut broker = test_broker();
    let subscription_id = accepted_id(broker.dispatch(
        "client",
        Command::Subscribe {
            topic: "changed/1".into(),
            capacity: 1,
            coalesce: false,
        },
    ));
    for n in 0..3 {
        broker.dispatch(
            "provider",
            Command::Publish {
                topic: "changed/1".into(),
                payload: json!(n),
            },
        );
    }
    for _ in 0..2 {
        assert!(matches!(
            broker.dispatch(
                "client",
                Command::NextEvent {
                    subscription: subscription_id
                }
            ),
            Response::ResyncRequired { .. }
        ));
    }
    assert_error_code(
        broker.dispatch(
            "outsider",
            Command::Unsubscribe {
                subscription: subscription_id,
            },
        ),
        "permission_denied",
    );
}
#[test]
fn telemetry_coalesces_and_retains_publisher_sequence() {
    let mut broker = test_broker();
    let subscription_id = accepted_id(broker.dispatch(
        "client",
        Command::Subscribe {
            topic: "changed/1".into(),
            capacity: 1,
            coalesce: true,
        },
    ));
    for n in 1..4 {
        broker.dispatch(
            "provider",
            Command::Publish {
                topic: "changed/1".into(),
                payload: json!(n),
            },
        );
    }
    assert!(
        matches!(broker.dispatch("client", Command::NextEvent { subscription: subscription_id }), Response::Event { sequence: 3, payload, .. } if payload == 3)
    );
}
#[test]
fn hooks_are_owner_scoped_and_fail_closed() {
    let mut broker = test_broker();
    assert_error_code(
        broker.dispatch(
            "client",
            Command::RunHook {
                hook: "prepare/1".into(),
                payload: json!({}),
            },
        ),
        "unavailable",
    );
    broker.dispatch(
        "provider",
        Command::RegisterHook {
            hook: "prepare/1".into(),
        },
    );
    assert_error_code(
        broker.dispatch(
            "outsider",
            Command::RunHook {
                hook: "prepare/1".into(),
                payload: json!({}),
            },
        ),
        "permission_denied",
    );
    let start = Instant::now();
    let invocation_id = accepted_id(broker.dispatch_at(
        "client",
        Command::RunHook {
            hook: "prepare/1".into(),
            payload: json!({}),
        },
        start,
    ));
    assert!(matches!(
        broker.dispatch_at(
            "client",
            Command::HookResult {
                invocation: invocation_id
            },
            start
        ),
        Response::Pending
    ));
    assert_error_code(
        broker.dispatch_at(
            "client",
            Command::HookResult {
                invocation: invocation_id,
            },
            start + Duration::from_millis(101),
        ),
        "deadline_exceeded",
    );
}
#[test]
fn hook_proposals_return_in_configured_order() {
    let mut broker = test_broker();
    broker.dispatch(
        "provider",
        Command::RegisterHook {
            hook: "prepare/1".into(),
        },
    );
    let invocation_id = accepted_id(broker.dispatch(
        "client",
        Command::RunHook {
            hook: "prepare/1".into(),
            payload: json!({}),
        },
    ));
    broker.dispatch(
        "client",
        Command::HookResult {
            invocation: invocation_id,
        },
    );
    let Response::Request { id: request_id, .. } = broker.dispatch("provider", Command::Poll {})
    else {
        panic!()
    };
    broker.dispatch(
        "provider",
        Command::Complete {
            request: request_id,
            payload: json!({"fps":60}),
        },
    );
    assert!(
        matches!(broker.dispatch("client", Command::HookResult { invocation: invocation_id }), Response::HookResult { proposals } if proposals.len() == 1 && proposals[0].participant == "provider")
    );
}
#[test]
fn manifest_requests_are_not_grants() {
    let requested = Grants {
        call: names(&["a/1", "broker/1"]),
        ..Default::default()
    };
    let policy = Grants {
        call: names(&["broker/1", "c/1"]),
        ..Default::default()
    };
    assert_eq!(requested.intersect(&policy).call, names(&["broker/1"]));
}

#[test]
fn settings_are_isolated_to_the_bound_endpoint() {
    let mut configuration = RuntimeConfig::default();
    for principal in ["first", "second", "default"] {
        configuration
            .principals
            .insert(principal.into(), Grants::default());
    }
    configuration
        .settings
        .insert("first".into(), json!({"path": "first.json"}));
    configuration
        .settings
        .insert("second".into(), json!({"path": "second.json"}));
    let mut broker = Broker::new(configuration).unwrap();
    for principal in ["first", "second"] {
        assert!(
            matches!(broker.dispatch(principal, Command::GetSettings {}),
            Response::Result { payload } if payload == json!({"path": format!("{principal}.json")}))
        );
    }
    assert!(
        matches!(broker.dispatch("default", Command::GetSettings {}),
        Response::Result { payload } if payload == json!({}))
    );
    assert_error_code(
        broker.dispatch("unknown", Command::GetSettings {}),
        "permission_denied",
    );
    assert!(
        serde_json::from_value::<Command>(json!({"op": "get_settings", "principal": "second"}))
            .is_err()
    );
}

#[test]
fn settings_configuration_rejects_unknown_principals_and_large_payloads() {
    let mut configuration = RuntimeConfig::default();
    configuration.settings.insert("unknown".into(), json!({}));
    assert!(Broker::new(configuration.clone()).is_err());
    configuration
        .principals
        .insert("unknown".into(), Grants::default());
    configuration
        .settings
        .insert("unknown".into(), json!("x".repeat(16_384)));
    assert!(Broker::new(configuration).is_err());
}

#[test]
fn retained_results_expire_and_release_capacity() {
    let mut broker = test_broker();
    let start = Instant::now();
    let first = accepted_id(broker.dispatch_at("client", request("retained.0"), start));
    for index in 1..512 {
        accepted_id(broker.dispatch_at("client", request(&format!("retained.{index}")), start));
    }
    assert_error_code(
        broker.dispatch_at("client", request("full"), start),
        "resource_exhausted",
    );
    let expired = start + Duration::from_secs(1);
    assert_error_code(
        broker.dispatch_at("client", Command::TakeResult { request: first }, expired),
        "deadline_exceeded",
    );
    let cleaned = expired + Duration::from_secs(300);
    assert_error_code(
        broker.dispatch_at("client", Command::TakeResult { request: first }, cleaned),
        "not_found",
    );
    accepted_id(broker.dispatch_at("client", request("replacement"), cleaned));
}

#[test]
fn delegated_retries_survive_parent_completion_and_children_cancel() {
    let mut configuration = RuntimeConfig::default();
    for principal in ["owner", "middle", "leaf"] {
        configuration.principals.insert(
            principal.into(),
            Grants {
                call: names(&["work/1"]),
                provide: names(&["work/1"]),
                ..Default::default()
            },
        );
    }
    let mut broker = Broker::new(configuration).unwrap();
    broker.dispatch(
        "middle",
        Command::Register {
            service: "work/1".into(),
        },
    );
    let make_request = |operation: &str, parent| Command::Request {
        service: "work/1".into(),
        payload: json!(null),
        timeout_ms: 30_000,
        operation_id: operation.into(),
        parent,
    };
    let parent = accepted_id(broker.dispatch("owner", make_request("parent", None)));
    broker.dispatch("middle", Command::Poll {});
    let child = accepted_id(broker.dispatch("middle", make_request("child", Some(parent))));
    broker.dispatch("middle", Command::Poll {});
    let grandchild =
        accepted_id(broker.dispatch("middle", make_request("grandchild", Some(child))));
    broker.dispatch(
        "middle",
        Command::Complete {
            request: parent,
            payload: json!(true),
        },
    );
    assert_eq!(
        accepted_id(broker.dispatch("middle", make_request("child", Some(parent)))),
        child
    );
    for request in [child, grandchild] {
        assert_error_code(
            broker.dispatch("middle", Command::TakeResult { request }),
            "cancelled",
        );
    }
    assert_error_code(
        broker.dispatch("middle", make_request("new", Some(parent))),
        "permission_denied",
    );
}

#[test]
fn hook_operations_do_not_conflict_with_caller_operation_identifiers() {
    let mut broker = test_broker();
    broker.dispatch(
        "provider",
        Command::RegisterHook {
            hook: "prepare/1".into(),
        },
    );
    let invocation = accepted_id(broker.dispatch(
        "client",
        Command::RunHook {
            hook: "prepare/1".into(),
            payload: json!({}),
        },
    ));
    broker.dispatch("client", Command::HookResult { invocation });
    accepted_id(broker.dispatch("client", request(&format!("hook.{invocation}.provider"))));
}

#[test]
fn provider_can_check_only_delivered_active_requests() {
    let mut broker = test_broker();
    let now = Instant::now();
    let id = accepted_id(broker.dispatch_at("client", request("check"), now));
    for caller in ["client", "outsider", "provider"] {
        assert_error_code(
            broker.dispatch_at(caller, Command::CheckRequest { request: id }, now),
            "permission_denied",
        );
    }
    broker.dispatch_at("provider", Command::Poll {}, now);
    assert!(matches!(
        broker.dispatch_at("provider", Command::CheckRequest { request: id }, now),
        Response::Ok
    ));
    broker.dispatch_at("client", Command::Cancel { request: id }, now);
    assert_error_code(
        broker.dispatch_at("provider", Command::CheckRequest { request: id }, now),
        "request_ended",
    );
    let deadline_id = accepted_id(broker.dispatch_at("client", request("check-deadline"), now));
    broker.dispatch_at("provider", Command::Poll {}, now);
    assert_error_code(
        broker.dispatch_at(
            "provider",
            Command::CheckRequest {
                request: deadline_id,
            },
            now + Duration::from_millis(101),
        ),
        "request_ended",
    );
}
