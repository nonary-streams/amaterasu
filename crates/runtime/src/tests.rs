use super::Broker;
use amaterasu_abi::MAX_MESSAGE;
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
fn broker_accepts_large_results_and_rejects_results_over_the_limit() {
    let mut broker = test_broker();
    let id = accepted_id(broker.dispatch(
        "client",
        Command::Request {
            service: "echo/1".into(),
            payload: json!({}),
            timeout_ms: 1000,
            operation_id: "large-result".into(),
            parent: None,
        },
    ));
    assert!(matches!(
        broker.dispatch("provider", Command::Poll {}),
        Response::Request { .. }
    ));
    let oversized = json!({"data":"x".repeat(MAX_BROKER_RESULT_BYTES)});
    assert_error_code(
        broker.dispatch(
            "provider",
            Command::Complete {
                request: id,
                payload: oversized,
            },
        ),
        "invalid_request",
    );
    let large = json!({"data":"x".repeat(128 * 1024)});
    assert!(matches!(
        broker.dispatch(
            "provider",
            Command::Complete {
                request: id,
                payload: large.clone()
            }
        ),
        Response::Ok
    ));
    assert!(
        matches!(broker.dispatch("client", Command::TakeResult { request: id }), Response::Result { payload } if payload == large)
    );
}

fn logging_broker() -> Broker {
    let configuration = RuntimeConfig {
        principals: [
            ("producer".into(), Grants::default()),
            (DIAGNOSTICS_COMPONENT.into(), Grants::default()),
            (HOST_LIFECYCLE_CONTROL_PRINCIPAL.into(), Grants::default()),
        ]
        .into(),
        host_lifecycle_control: true,
        ..Default::default()
    };
    Broker::new(configuration).unwrap()
}

#[test]
fn product_log_service_is_reserved_to_host_and_management() {
    let configuration = RuntimeConfig {
        principals: [
            (
                "amaterasu.management".into(),
                Grants {
                    call: names(&[HOST_LOG_SERVICE]),
                    ..Default::default()
                },
            ),
            (
                "outsider".into(),
                Grants {
                    call: names(&[HOST_LOG_SERVICE]),
                    provide: names(&[HOST_LOG_SERVICE]),
                    ..Default::default()
                },
            ),
            (HOST_LIFECYCLE_CONTROL_PRINCIPAL.into(), Grants::default()),
        ]
        .into(),
        host_lifecycle_control: true,
        ..Default::default()
    };
    let mut broker = Broker::new(configuration).unwrap();
    assert_error_code(
        broker.dispatch(
            "outsider",
            Command::Register {
                service: HOST_LOG_SERVICE.into(),
            },
        ),
        "permission_denied",
    );
    let request = || Command::Request {
        service: HOST_LOG_SERVICE.into(),
        payload: json!({"action":"open"}),
        timeout_ms: 1000,
        operation_id: "log.1".into(),
        parent: None,
    };
    assert_error_code(broker.dispatch("outsider", request()), "permission_denied");
    let id = match broker.dispatch("amaterasu.management", request()) {
        Response::Accepted { id } => id,
        other => panic!("unexpected host-log submission: {other:?}"),
    };
    assert_error_code(
        broker.dispatch("outsider", Command::TakeResult { request: id }),
        "permission_denied",
    );
    assert!(matches!(
        broker.dispatch("amaterasu.management", Command::TakeResult { request: id }),
        Response::Pending
    ));
    assert!(
        matches!(broker.dispatch(HOST_LIFECYCLE_CONTROL_PRINCIPAL, Command::Poll {}),
        Response::Request { id: delivered, service, caller, .. }
        if delivered == id && service == HOST_LOG_SERVICE && caller == "amaterasu.management")
    );
    assert_error_code(
        broker.dispatch(
            "outsider",
            Command::Complete {
                request: id,
                payload: json!({"ok":true}),
            },
        ),
        "permission_denied",
    );
    assert!(matches!(
        broker.dispatch(
            HOST_LIFECYCLE_CONTROL_PRINCIPAL,
            Command::CheckRequest { request: id }
        ),
        Response::Ok
    ));
    assert!(matches!(
        broker.dispatch(
            HOST_LIFECYCLE_CONTROL_PRINCIPAL,
            Command::Complete {
                request: id,
                payload: json!({"ok":true,"length":12})
            }
        ),
        Response::Ok
    ));
    assert!(
        matches!(broker.dispatch("amaterasu.management", Command::TakeResult { request: id }),
        Response::Result { payload } if payload == json!({"ok":true,"length":12}))
    );
    assert_error_code(
        broker.dispatch(
            HOST_LIFECYCLE_CONTROL_PRINCIPAL,
            Command::Complete {
                request: id,
                payload: json!({"ok":true}),
            },
        ),
        "request_ended",
    );
    assert!(matches!(
        broker.dispatch(
            "amaterasu.management",
            Command::ReleaseResult { request: id }
        ),
        Response::Ok
    ));
}

#[test]
fn host_log_service_requires_both_host_control_and_a_management_grant() {
    let without_control = RuntimeConfig {
        principals: [(
            "amaterasu.management".into(),
            Grants {
                call: names(&[HOST_LOG_SERVICE]),
                ..Default::default()
            },
        )]
        .into(),
        ..Default::default()
    };
    let request = || Command::Request {
        service: HOST_LOG_SERVICE.into(),
        payload: json!({"action":"open"}),
        timeout_ms: 1000,
        operation_id: "log.1".into(),
        parent: None,
    };
    let mut broker = Broker::new(without_control).unwrap();
    assert_error_code(
        broker.dispatch("amaterasu.management", request()),
        "unavailable",
    );
    assert_error_code(
        broker.dispatch(HOST_LIFECYCLE_CONTROL_PRINCIPAL, Command::Poll {}),
        "permission_denied",
    );
    let without_grant = RuntimeConfig {
        principals: [
            ("amaterasu.management".into(), Grants::default()),
            (HOST_LIFECYCLE_CONTROL_PRINCIPAL.into(), Grants::default()),
        ]
        .into(),
        host_lifecycle_control: true,
        ..Default::default()
    };
    let mut broker = Broker::new(without_grant).unwrap();
    assert_error_code(
        broker.dispatch("amaterasu.management", request()),
        "permission_denied",
    );
}

#[test]
fn host_log_requests_honor_cancellation_deadline_and_response_bounds() {
    let config = RuntimeConfig {
        principals: [
            (
                "amaterasu.management".into(),
                Grants {
                    call: names(&[HOST_LOG_SERVICE]),
                    ..Default::default()
                },
            ),
            (HOST_LIFECYCLE_CONTROL_PRINCIPAL.into(), Grants::default()),
        ]
        .into(),
        host_lifecycle_control: true,
        ..Default::default()
    };
    let mut broker = Broker::new(config).unwrap();
    let now = Instant::now();
    let request = |operation: &str| Command::Request {
        service: HOST_LOG_SERVICE.into(),
        payload: json!({"action":"open"}),
        timeout_ms: 100,
        operation_id: operation.into(),
        parent: None,
    };
    let id = match broker.dispatch_at("amaterasu.management", request("log.cancel"), now) {
        Response::Accepted { id } => id,
        other => panic!("unexpected request: {other:?}"),
    };
    assert!(matches!(
        broker.dispatch_at(HOST_LIFECYCLE_CONTROL_PRINCIPAL, Command::Poll {}, now),
        Response::Request { .. }
    ));
    assert_error_code(
        broker.dispatch_at(
            HOST_LIFECYCLE_CONTROL_PRINCIPAL,
            Command::Complete {
                request: id,
                payload: json!({"data":"x".repeat(MAX_BROKER_RESULT_BYTES)}),
            },
            now,
        ),
        "invalid_request",
    );
    assert!(matches!(
        broker.dispatch_at("amaterasu.management", Command::Cancel { request: id }, now),
        Response::Ok
    ));
    assert_error_code(
        broker.dispatch_at(
            HOST_LIFECYCLE_CONTROL_PRINCIPAL,
            Command::Complete {
                request: id,
                payload: json!({"ok":true}),
            },
            now,
        ),
        "request_ended",
    );
    let next = match broker.dispatch_at("amaterasu.management", request("log.timeout"), now) {
        Response::Accepted { id } => id,
        other => panic!("unexpected request: {other:?}"),
    };
    assert!(matches!(
        broker.dispatch_at(HOST_LIFECYCLE_CONTROL_PRINCIPAL, Command::Poll {}, now),
        Response::Request { .. }
    ));
    assert_error_code(
        broker.dispatch_at(
            HOST_LIFECYCLE_CONTROL_PRINCIPAL,
            Command::Complete {
                request: next,
                payload: json!({"ok":true}),
            },
            now + Duration::from_millis(101),
        ),
        "request_ended",
    );
}

#[test]
fn product_log_events_are_capped_scoped_and_host_drained() {
    let mut broker = logging_broker();
    assert!(matches!(
        broker.dispatch(
            "producer",
            Command::Log {
                level: 3,
                target: "server".into(),
                message: "listener ready\nnext line".into(),
            }
        ),
        Response::Ok
    ));
    assert_error_code(
        broker.dispatch("producer", Command::DrainLogEvents {}),
        "permission_denied",
    );
    assert_error_code(
        broker.dispatch(
            "producer",
            Command::Log {
                level: 6,
                target: "server".into(),
                message: "bad level".into(),
            },
        ),
        "invalid_log_record",
    );
    assert_error_code(
        broker.dispatch(
            "producer",
            Command::Log {
                level: 2,
                target: "server".into(),
                message: "x".repeat(7_501),
            },
        ),
        "invalid_log_record",
    );

    assert!(matches!(
        broker.dispatch(
            HOST_LIFECYCLE_CONTROL_PRINCIPAL,
            Command::DrainLogEvents {}
        ),
        Response::LogEvents {
            events,
            dropped_records: 0
        } if events == vec![HostLogEvent::Record {
            level: 3,
            target: "producer::server".into(),
            message: "listener ready\nnext line".into(),
        }]
    ));
}

#[test]
fn escaped_log_drain_batch_stays_within_the_runtime_abi_limit() {
    let mut broker = logging_broker();
    for _ in 0..16 {
        assert!(matches!(
            broker.dispatch(
                "producer",
                Command::Log {
                    level: 2,
                    target: "t".repeat(118),
                    message: "\u{1}".repeat(7_500),
                }
            ),
            Response::Ok
        ));
    }

    let response = broker.dispatch(HOST_LIFECYCLE_CONTROL_PRINCIPAL, Command::DrainLogEvents {});
    let encoded = serde_json::to_vec(&response).unwrap();
    assert!(encoded.len() < MAX_MESSAGE, "{} bytes", encoded.len());
}

#[test]
fn only_diagnostics_can_queue_revisioned_logging_policy_updates() {
    let mut broker = logging_broker();
    let update = |revision, log_path: &str| Command::LoggingPolicyChanged {
        revision,
        minimum_level: 2,
        log_path: log_path.into(),
    };
    assert_error_code(
        broker.dispatch("producer", update(1, "logs/first.log")),
        "permission_denied",
    );
    assert!(matches!(
        broker.dispatch(DIAGNOSTICS_COMPONENT, update(0, "logs/bootstrap.log")),
        Response::Ok
    ));
    assert!(matches!(
        broker.dispatch(DIAGNOSTICS_COMPONENT, update(0, "logs/ignored.log")),
        Response::Ok
    ));
    assert!(matches!(
        broker.dispatch(DIAGNOSTICS_COMPONENT, update(1, "logs/first.log")),
        Response::Ok
    ));
    assert!(matches!(
        broker.dispatch(DIAGNOSTICS_COMPONENT, update(1, "logs/ignored.log")),
        Response::Ok
    ));
    assert_error_code(
        broker.dispatch(DIAGNOSTICS_COMPONENT, update(0, "logs/older.log")),
        "stale_revision",
    );
    assert_error_code(
        broker.dispatch(
            DIAGNOSTICS_COMPONENT,
            Command::LoggingPolicyChanged {
                revision: 2,
                minimum_level: 7,
                log_path: "logs/invalid.log".into(),
            },
        ),
        "invalid_logging_policy",
    );
    assert!(matches!(
        broker.dispatch(
            HOST_LIFECYCLE_CONTROL_PRINCIPAL,
            Command::DrainLogEvents {}
        ),
        Response::LogEvents { events, .. }
            if events == vec![
                HostLogEvent::Policy {
                    revision: 0,
                    minimum_level: 2,
                    log_path: "logs/bootstrap.log".into(),
                },
                HostLogEvent::Policy {
                    revision: 1,
                    minimum_level: 2,
                    log_path: "logs/first.log".into(),
                }
            ]
    ));
}

#[test]
fn bounded_log_queue_drops_records_before_rejecting_a_policy_update() {
    let mut broker = logging_broker();
    for index in 0..512 {
        assert!(matches!(
            broker.dispatch(
                "producer",
                Command::Log {
                    level: 2,
                    target: "server".into(),
                    message: format!("record {index}"),
                }
            ),
            Response::Ok
        ));
    }
    assert!(matches!(
        broker.dispatch(
            DIAGNOSTICS_COMPONENT,
            Command::LoggingPolicyChanged {
                revision: 1,
                minimum_level: 3,
                log_path: "logs/current.log".into(),
            }
        ),
        Response::Ok
    ));
    let mut events = Vec::new();
    let mut dropped_records = 0;
    loop {
        let Response::LogEvents {
            events: batch,
            dropped_records: batch_drops,
        } = broker.dispatch(HOST_LIFECYCLE_CONTROL_PRINCIPAL, Command::DrainLogEvents {})
        else {
            panic!("host log drain returned the wrong response");
        };
        assert!(batch.len() <= 16);
        dropped_records += batch_drops;
        if batch.is_empty() {
            break;
        }
        events.extend(batch);
    }
    assert_eq!(events.len(), 512);
    assert_eq!(dropped_records, 1);
    assert!(matches!(
        events.last(),
        Some(HostLogEvent::Policy { revision: 1, .. })
    ));
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
    configuration.principals.insert(
        "example.external".into(),
        Grants {
            call: names(&[HOST_STATE_CONTEXT_SERVICE]),
            ..Default::default()
        },
    );
    configuration.principals.insert(
        "example.unrelated".into(),
        Grants {
            call: names(&[HOST_LIFECYCLE_SERVICE]),
            ..Default::default()
        },
    );
    let requested = Grants {
        call: names(&[HOST_STATE_CONTEXT_SERVICE]),
        ..Default::default()
    };
    configuration.principals.insert(
        "example.unapproved".into(),
        requested.intersect(&Grants::default()),
    );
    configuration
        .principals
        .insert("amaterasu.management".into(), Grants::default());
    configuration.host_context = RuntimeHostContext {
        schema: 1,
        state_root: Some(r"C:\\ProgramData\\Amaterasu\\runtime\\user-state\\S-1-5-21-1".into()),
        package_root: Some(r"C:\\ProgramData\\Amaterasu\\packages".into()),
        running_package_digest: Some("a".repeat(64)),
    };
    let state_root = configuration.host_context.state_root.clone();
    let mut broker = Broker::new(configuration).unwrap();
    let Response::Result { payload } =
        broker.dispatch("amaterasu.state", Command::GetHostContext {})
    else {
        panic!("state service did not receive host context");
    };
    assert_eq!(payload["schema"], 1);
    assert!(payload.get("package_root").is_none());
    let Response::Result { payload } =
        broker.dispatch("example.external", Command::GetHostContext {})
    else {
        panic!("approved external module did not receive host state context");
    };
    assert_eq!(payload, json!({"schema": 1, "state_root": state_root}));
    assert!(matches!(
        broker.dispatch("example.external", Command::GetPackageRoot {}),
        Response::Error { code, .. } if code == "permission_denied"
    ));
    assert!(matches!(
        broker.dispatch("example.external", Command::GetRunningPackageDigest {}),
        Response::Error { code, .. } if code == "permission_denied"
    ));
    assert!(matches!(
        broker.dispatch("example.unrelated", Command::GetHostContext {}),
        Response::Error { code, .. } if code == "permission_denied"
    ));
    assert!(matches!(
        broker.dispatch("example.unapproved", Command::GetHostContext {}),
        Response::Error { code, .. } if code == "permission_denied"
    ));
    assert!(matches!(
        broker.dispatch("unknown", Command::GetHostContext {}),
        Response::Error { code, .. } if code == "permission_denied"
    ));
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
    assert!(matches!(
        broker.dispatch("amaterasu.management", Command::GetHostContext {}),
        Response::Error { code, .. } if code == "permission_denied"
    ));
    let Response::Result { payload } =
        broker.dispatch("amaterasu.management", Command::GetPackageRoot {})
    else {
        panic!("management did not receive the host-owned package root");
    };
    assert_eq!(
        payload["package_root"],
        r"C:\\ProgramData\\Amaterasu\\packages"
    );
    let Response::Result { payload } =
        broker.dispatch("amaterasu.management", Command::GetRunningPackageDigest {})
    else {
        panic!("management did not receive the running package digest");
    };
    assert_eq!(payload, serde_json::json!({"digest":"a".repeat(64)}));
    for caller in ["amaterasu.state", "amaterasu.audio", "other"] {
        assert!(matches!(
            broker.dispatch(caller, Command::GetPackageRoot {}),
            Response::Error { code, .. } if code == "permission_denied"
        ));
        assert!(matches!(
            broker.dispatch(caller, Command::GetRunningPackageDigest {}),
            Response::Error { code, .. } if code == "permission_denied"
        ));
    }
}

#[test]
fn running_package_digest_is_unavailable_without_host_injection() {
    let mut configuration = RuntimeConfig::default();
    configuration
        .principals
        .insert("amaterasu.management".into(), Grants::default());
    let mut broker = Broker::new(configuration).unwrap();
    assert!(matches!(
        broker.dispatch("amaterasu.management", Command::GetRunningPackageDigest {}),
        Response::Error { code, .. } if code == "running_package_unavailable"
    ));
}

#[test]
fn host_lifecycle_requires_a_module_grant_and_the_private_host_identity_to_drain() {
    let configuration = RuntimeConfig {
        principals: [
            ("tray".into(), Grants::default()),
            (HOST_LIFECYCLE_CONTROL_PRINCIPAL.into(), Grants::default()),
        ]
        .into(),
        host_lifecycle_control: true,
        ..Default::default()
    };
    let mut broker = Broker::new(configuration).unwrap();

    assert_error_code(
        broker.dispatch(
            "tray",
            Command::HostLifecycle {
                action: HostLifecycleAction::Quit,
            },
        ),
        "permission_denied",
    );
    assert_error_code(
        broker.dispatch("tray", Command::HostLifecycleDrain {}),
        "permission_denied",
    );
    assert!(matches!(
        broker.dispatch(
            HOST_LIFECYCLE_CONTROL_PRINCIPAL,
            Command::HostLifecycleDrain {}
        ),
        Response::HostLifecycle { action: None }
    ));
}

#[test]
fn host_lifecycle_queue_is_bounded_and_drains_each_authorized_action_once() {
    let configuration = RuntimeConfig {
        principals: [
            (
                "tray".into(),
                Grants {
                    call: names(&[HOST_LIFECYCLE_SERVICE]),
                    ..Default::default()
                },
            ),
            (HOST_LIFECYCLE_CONTROL_PRINCIPAL.into(), Grants::default()),
        ]
        .into(),
        host_lifecycle_control: true,
        ..Default::default()
    };
    let mut broker = Broker::new(configuration).unwrap();

    for index in 0..8 {
        let action = if index % 2 == 0 {
            HostLifecycleAction::Restart
        } else {
            HostLifecycleAction::Quit
        };
        assert!(matches!(
            broker.dispatch("tray", Command::HostLifecycle { action }),
            Response::Ok
        ));
    }
    assert_error_code(
        broker.dispatch(
            "tray",
            Command::HostLifecycle {
                action: HostLifecycleAction::Restart,
            },
        ),
        "capacity_exhausted",
    );

    for index in 0..8 {
        let expected = if index % 2 == 0 {
            HostLifecycleAction::Restart
        } else {
            HostLifecycleAction::Quit
        };
        assert!(matches!(
            broker.dispatch(
                HOST_LIFECYCLE_CONTROL_PRINCIPAL,
                Command::HostLifecycleDrain {}
            ),
            Response::HostLifecycle { action: Some(action) } if action == expected
        ));
    }
    assert!(matches!(
        broker.dispatch(
            HOST_LIFECYCLE_CONTROL_PRINCIPAL,
            Command::HostLifecycleDrain {}
        ),
        Response::HostLifecycle { action: None }
    ));
}

#[test]
fn reserved_host_lifecycle_identity_requires_host_injection_and_empty_grants() {
    let mut package_claim = RuntimeConfig::default();
    package_claim
        .principals
        .insert(HOST_LIFECYCLE_CONTROL_PRINCIPAL.into(), Grants::default());
    assert!(Broker::new(package_claim).is_err());

    let widened_host = RuntimeConfig {
        principals: [(
            HOST_LIFECYCLE_CONTROL_PRINCIPAL.into(),
            Grants {
                call: names(&[HOST_LIFECYCLE_SERVICE]),
                ..Default::default()
            },
        )]
        .into(),
        host_lifecycle_control: true,
        ..Default::default()
    };
    assert!(Broker::new(widened_host).is_err());
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
    let approval = config.api_methods[0].clone();
    let mut broker = Broker::new(config).unwrap();
    let provider = "org.example.provider";
    let client = "org.example.client";
    let outsider = "org.example.outsider";
    let query_approval = || Command::QueryApiApproval {
        service: method.service.clone(),
        method: method.method.clone(),
        interface_major: 1,
        min_minor: 2,
    };
    assert_error_code(
        broker.dispatch(client, query_approval()),
        "permission_denied",
    );
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
        broker.dispatch(outsider, query_approval()),
        "permission_denied",
    );
    assert!(
        matches!(broker.dispatch(client, query_approval()), Response::Result { payload }
        if payload == serde_json::to_value(&approval).unwrap())
    );
    assert_error_code(
        broker.dispatch(
            client,
            Command::QueryApiApproval {
                service: method.service.clone(),
                method: method.method.clone(),
                interface_major: 1,
                min_minor: 3,
            },
        ),
        "incompatible_api",
    );
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
    let lookup = |payload, minor, operation: &str| Command::LookupApiRequest {
        service: method.service.clone(),
        method: "render".into(),
        interface_major: 1,
        interface_minor: minor,
        payload,
        operation_id: operation.into(),
        parent: None,
    };
    assert_error_code(
        broker.dispatch(client, lookup(json!({"size":4}), 2, "valid.call")),
        "not_found",
    );
    assert!(matches!(
        broker.dispatch(provider, Command::Poll {}),
        Response::Pending
    ));
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
    assert_eq!(
        accepted_id(broker.dispatch(client, lookup(json!({"size":4}), 2, "valid.call"))),
        id
    );
    assert_error_code(
        broker.dispatch(client, lookup(json!({"size":5}), 2, "valid.call")),
        "conflict",
    );
    assert_error_code(
        broker.dispatch(client, lookup(json!({"size":4}), 1, "valid.call")),
        "incompatible_api",
    );
    assert_error_code(
        broker.dispatch(outsider, lookup(json!({"size":4}), 2, "valid.call")),
        "permission_denied",
    );
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
    assert_eq!(
        accepted_id(broker.dispatch(client, lookup(json!({"size":4}), 2, "valid.call"))),
        id
    );
    assert_error_code(
        broker.dispatch_at(
            client,
            lookup(json!({"size":4}), 2, "valid.call"),
            Instant::now() + Duration::from_secs(301),
        ),
        "not_found",
    );
    assert!(matches!(
        broker.dispatch(provider, Command::Poll {}),
        Response::Pending
    ));
}

#[test]
fn typed_lookup_conflicts_with_an_ordinary_operation_id() {
    let (mut config, method) = synthetic_api(ApiPrivilege::User);
    config
        .principals
        .get_mut("org.example.client")
        .unwrap()
        .call
        .insert("work/1".into());
    config
        .principals
        .get_mut("org.example.provider")
        .unwrap()
        .provide
        .insert("work/1".into());
    let mut broker = Broker::new(config).unwrap();
    broker.dispatch(
        "org.example.provider",
        Command::RegisterApi {
            method: method.clone(),
        },
    );
    broker.dispatch(
        "org.example.provider",
        Command::Register {
            service: "work/1".into(),
        },
    );
    accepted_id(broker.dispatch(
        "org.example.client",
        Command::Request {
            service: "work/1".into(),
            payload: json!({}),
            timeout_ms: 1000,
            operation_id: "shared".into(),
            parent: None,
        },
    ));
    assert_error_code(
        broker.dispatch(
            "org.example.client",
            Command::LookupApiRequest {
                service: method.service,
                method: method.method,
                interface_major: 1,
                interface_minor: 2,
                payload: json!({"size":1}),
                operation_id: "shared".into(),
                parent: None,
            },
        ),
        "conflict",
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
    let child = accepted_id(broker.dispatch(intermediary, api_call(good_parent)));
    assert_error_code(
        broker.dispatch(
            intermediary,
            Command::LookupRequest {
                service: method.service.clone(),
                payload: json!({"size": 1}),
                operation_id: format!("api.{good_parent}"),
                parent: Some(good_parent),
            },
        ),
        "conflict",
    );
    assert!(matches!(
        broker.dispatch(
            intermediary,
            Command::Complete {
                request: good_parent,
                payload: json!({})
            }
        ),
        Response::Ok
    ));
    assert!(matches!(
        broker.dispatch(
            "org.example.client",
            Command::ReleaseResult {
                request: good_parent
            }
        ),
        Response::Ok
    ));
    assert_eq!(
        accepted_id(broker.dispatch(intermediary, api_call(good_parent))),
        child
    );
    let lookup_child = |parent| Command::LookupApiRequest {
        service: method.service.clone(),
        method: method.method.clone(),
        interface_major: 1,
        interface_minor: 2,
        payload: json!({"size":1}),
        operation_id: format!("api.{good_parent}"),
        parent: Some(parent),
    };
    assert_eq!(
        accepted_id(broker.dispatch(intermediary, lookup_child(good_parent))),
        child
    );
    assert_error_code(
        broker.dispatch(intermediary, lookup_child(good_parent + 1000)),
        "permission_denied",
    );
    assert_error_code(
        broker.dispatch(intermediary, Command::TakeResult { request: child }),
        "cancelled",
    );
    let mut altered = api_call(good_parent);
    if let Command::CallApi {
        ref mut payload, ..
    } = altered
    {
        *payload = json!({"size": 2});
    }
    assert_error_code(broker.dispatch(intermediary, altered), "conflict");
    let mut changed_method = api_call(good_parent);
    if let Command::CallApi { ref mut method, .. } = changed_method {
        *method = "other".into();
    }
    assert_error_code(
        broker.dispatch(intermediary, changed_method),
        "permission_denied",
    );
    let mut changed_parent = api_call(good_parent);
    if let Command::CallApi { ref mut parent, .. } = changed_parent {
        *parent = Some(good_parent + 1_000);
    }
    assert_error_code(
        broker.dispatch(intermediary, changed_parent),
        "permission_denied",
    );
    assert_error_code(
        broker.dispatch("org.example.client", api_call(good_parent)),
        "permission_denied",
    );
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
fn processing_compositions_are_isolated_and_do_not_grant_service_access() {
    use amaterasu_contracts::processing_composition::{CapabilityBinding, ProcessingComposition};
    let mut configuration = RuntimeConfig::default();
    configuration.principals.insert(
        "effect".into(),
        Grants {
            native_provide: names(&["effect/1"]),
            ..Default::default()
        },
    );
    for principal in ["consumer", "other"] {
        configuration
            .principals
            .insert(principal.into(), Grants::default());
    }
    let binding = CapabilityBinding {
        principal: "effect".into(),
        service: "effect/1".into(),
        contract: "amaterasu.frame-processor.native/1".into(),
        interface_major: 1,
        interface_minor: 0,
    };
    let selected = ProcessingComposition {
        processor: Some(binding.clone()),
        ..Default::default()
    };
    configuration
        .processing_compositions
        .insert("consumer".into(), selected.clone());
    let mut broker = Broker::new(configuration.clone()).unwrap();
    assert!(matches!(
        broker.dispatch("consumer", Command::GetProcessingComposition {}),
        Response::Result { payload } if payload == serde_json::to_value(&selected).unwrap()
    ));
    assert_error_code(
        broker.dispatch("other", Command::GetProcessingComposition {}),
        "not_found",
    );
    assert_error_code(
        broker.dispatch("unknown", Command::GetProcessingComposition {}),
        "permission_denied",
    );
    assert!(
        serde_json::from_value::<Command>(
            json!({"op":"get_processing_composition","principal":"consumer"})
        )
        .is_err()
    );
    assert!(
        !configuration.principals["consumer"]
            .native_use
            .contains("effect/1")
    );
    configuration
        .principals
        .get_mut("effect")
        .unwrap()
        .native_provide
        .clear();
    assert!(Broker::new(configuration).is_err());
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
fn reliable_poll_repeats_offers_until_accepted_and_fences_mode_changes() {
    let mut broker = test_broker();
    let now = Instant::now();
    let first = accepted_id(broker.dispatch_at("client", request("reliable.first"), now));
    let second = accepted_id(broker.dispatch_at("client", request("reliable.second"), now));
    for _ in 0..3 {
        assert!(
            matches!(broker.dispatch_at("provider", Command::PollV2 { receiver: "worker".into() }, now), Response::Request { id, .. } if id == first)
        );
    }
    assert_error_code(
        broker.dispatch_at(
            "provider",
            Command::SelectRequestDeliveryMode { reliable: false },
            now,
        ),
        "busy",
    );
    assert_error_code(
        broker.dispatch_at("provider", Command::Poll {}, now),
        "delivery_mode_mismatch",
    );
    assert_error_code(
        broker.dispatch_at(
            "provider",
            Command::AcceptRequest {
                request: second,
                receiver: "worker".into(),
            },
            now,
        ),
        "permission_denied",
    );
    assert_error_code(
        broker.dispatch_at(
            "secret",
            Command::AcceptRequest {
                request: first,
                receiver: "worker".into(),
            },
            now,
        ),
        "delivery_mode_mismatch",
    );
    assert!(matches!(
        broker.dispatch_at(
            "provider",
            Command::AcceptRequest {
                request: first,
                receiver: "worker".into()
            },
            now
        ),
        Response::Ok
    ));
    assert!(matches!(
        broker.dispatch_at(
            "provider",
            Command::AcceptRequest {
                request: first,
                receiver: "worker".into()
            },
            now
        ),
        Response::Ok
    ));
    assert!(
        matches!(broker.dispatch_at("provider", Command::PollV2 { receiver: "worker".into() }, now), Response::Request { id, .. } if id == second)
    );
    assert!(matches!(
        broker.dispatch_at(
            "provider",
            Command::AcceptRequest {
                request: second,
                receiver: "worker".into()
            },
            now
        ),
        Response::Ok
    ));
    assert_error_code(
        broker.dispatch_at(
            "provider",
            Command::SelectRequestDeliveryMode { reliable: false },
            now,
        ),
        "busy",
    );
    for id in [first, second] {
        assert!(matches!(
            broker.dispatch_at(
                "provider",
                Command::Complete {
                    request: id,
                    payload: json!({})
                },
                now
            ),
            Response::Ok
        ));
        assert!(matches!(
            broker.dispatch_at(
                "provider",
                Command::RetireRequestDelivery {
                    request: id,
                    receiver: "worker".into()
                },
                now
            ),
            Response::Ok
        ));
    }
    assert!(matches!(
        broker.dispatch_at(
            "provider",
            Command::SelectRequestDeliveryMode { reliable: false },
            now
        ),
        Response::Ok
    ));
    assert!(matches!(
        broker.dispatch_at("provider", Command::Poll {}, now),
        Response::Pending
    ));
}

#[test]
fn reliable_retirement_survives_cancel_release_and_result_expiry() {
    let mut broker = test_broker();
    let now = Instant::now();
    let id = accepted_id(broker.dispatch_at("client", request("retire.active"), now));
    broker.dispatch_at(
        "provider",
        Command::PollV2 {
            receiver: "old".into(),
        },
        now,
    );
    assert!(matches!(
        broker.dispatch_at(
            "provider",
            Command::AcceptRequest {
                request: id,
                receiver: "old".into()
            },
            now
        ),
        Response::Ok
    ));
    assert_error_code(
        broker.dispatch_at(
            "provider",
            Command::RetireRequestDelivery {
                request: id,
                receiver: "old".into(),
            },
            now,
        ),
        "request_pending",
    );
    broker.dispatch_at("client", Command::Cancel { request: id }, now);
    broker.dispatch_at("client", Command::ReleaseResult { request: id }, now);
    let later = now + Duration::from_secs(301);
    assert_error_code(
        broker.dispatch_at(
            "provider",
            Command::PollV2 {
                receiver: "new".into(),
            },
            later,
        ),
        "busy",
    );
    assert_error_code(
        broker.dispatch_at(
            "provider",
            Command::SelectRequestDeliveryMode { reliable: false },
            later,
        ),
        "busy",
    );
    assert_error_code(
        broker.dispatch_at(
            "provider",
            Command::RetireRequestDelivery {
                request: id,
                receiver: "new".into(),
            },
            later,
        ),
        "permission_denied",
    );
    assert!(matches!(
        broker.dispatch_at(
            "provider",
            Command::RetireRequestDelivery {
                request: id,
                receiver: "old".into()
            },
            later
        ),
        Response::Ok
    ));
    assert_error_code(
        broker.dispatch_at(
            "provider",
            Command::RetireRequestDelivery {
                request: id,
                receiver: "old".into(),
            },
            later,
        ),
        "not_found",
    );
    assert!(matches!(
        broker.dispatch_at(
            "provider",
            Command::SelectRequestDeliveryMode { reliable: false },
            later
        ),
        Response::Ok
    ));
}

#[test]
fn sdk_retires_lost_acceptance_after_the_caller_cancels_and_releases() {
    use amaterasu_abi::{
        AbiHeader, BorrowedBytes, HostApi, ReplySink, STATUS_INTERNAL_ERROR, STATUS_OK, Status,
    };
    use amaterasu_module_sdk::{RuntimeClient, requests::ReliableRequestReceiver};
    use std::ffi::c_void;
    use std::sync::Mutex;

    struct Endpoint {
        broker: Mutex<Broker>,
        // 1 loses the reply before acceptance; 2 loses it after acceptance.
        lose_acceptance: std::sync::atomic::AtomicUsize,
    }

    unsafe extern "C" fn invoke(
        context: *mut c_void,
        input: BorrowedBytes,
        reply: ReplySink,
    ) -> Status {
        // SAFETY: the fixture and request bytes are kept alive for this call.
        let endpoint = unsafe { &*context.cast::<Endpoint>() };
        let Ok(bytes) = (unsafe { input.read() }) else {
            return STATUS_INTERNAL_ERROR;
        };
        let Ok(command) = serde_json::from_slice::<Command>(bytes) else {
            return STATUS_INTERNAL_ERROR;
        };
        let failure = if matches!(command, Command::AcceptRequest { .. }) {
            endpoint
                .lose_acceptance
                .swap(0, std::sync::atomic::Ordering::SeqCst)
        } else {
            0
        };
        if failure == 1 {
            return STATUS_INTERNAL_ERROR;
        }
        let Ok(mut broker) = endpoint.broker.lock() else {
            return STATUS_INTERNAL_ERROR;
        };
        let response = broker.dispatch("provider", command);
        drop(broker);
        if failure == 2 {
            return STATUS_INTERNAL_ERROR;
        }
        let Ok(bytes) = serde_json::to_vec(&response) else {
            return STATUS_INTERNAL_ERROR;
        };
        // SAFETY: the SDK's synchronous reply sink copies these response bytes.
        unsafe { reply.send(&bytes) };
        STATUS_OK
    }

    for failure in [1, 2] {
        let endpoint = Endpoint {
            broker: Mutex::new(test_broker()),
            lose_acceptance: std::sync::atomic::AtomicUsize::new(failure),
        };
        let api = HostApi {
            header: AbiHeader::of::<HostApi>(),
            context: (&endpoint as *const Endpoint).cast_mut().cast(),
            invoke,
        };
        // SAFETY: the fixture outlives the client and serializes broker calls.
        let runtime = unsafe { RuntimeClient::from_endpoint(api) };
        let id = accepted_id(
            endpoint
                .broker
                .lock()
                .unwrap()
                .dispatch("client", request("lost.accept")),
        );
        let mut receiver = ReliableRequestReceiver::new().unwrap();
        receiver.select(&runtime).unwrap();
        assert!(receiver.poll(&runtime).is_err());
        {
            let mut broker = endpoint.broker.lock().unwrap();
            assert!(matches!(
                broker.dispatch("client", Command::Cancel { request: id }),
                Response::Ok
            ));
            assert!(matches!(
                broker.dispatch("client", Command::ReleaseResult { request: id }),
                Response::Ok
            ));
        }
        assert!(receiver.poll(&runtime).unwrap().is_none());
        assert!(!receiver.is_idle());
        assert!(receiver.drain(&runtime).unwrap().is_none());
        assert!(receiver.is_idle());
        receiver.deselect(&runtime).unwrap();
    }
}

#[test]
fn reliable_unretired_capacity_applies_backpressure() {
    let mut broker = test_broker();
    for index in 0..512 {
        let id = accepted_id(broker.dispatch("client", request(&format!("unretired.{index}"))));
        broker.dispatch(
            "provider",
            Command::PollV2 {
                receiver: "worker".into(),
            },
        );
        assert!(matches!(
            broker.dispatch(
                "provider",
                Command::AcceptRequest {
                    request: id,
                    receiver: "worker".into()
                }
            ),
            Response::Ok
        ));
        broker.dispatch("client", Command::Cancel { request: id });
        broker.dispatch("client", Command::ReleaseResult { request: id });
    }
    let id = accepted_id(broker.dispatch("client", request("unretired.overflow")));
    broker.dispatch(
        "provider",
        Command::PollV2 {
            receiver: "worker".into(),
        },
    );
    assert_error_code(
        broker.dispatch(
            "provider",
            Command::AcceptRequest {
                request: id,
                receiver: "worker".into(),
            },
        ),
        "resource_exhausted",
    );
}

#[test]
fn reliable_offers_cannot_be_accepted_after_cancel_or_deadline() {
    let mut broker = test_broker();
    let now = Instant::now();
    let first = accepted_id(broker.dispatch_at("client", request("cancel.offer"), now));
    assert!(
        matches!(broker.dispatch_at("provider", Command::PollV2 { receiver: "worker".into() }, now), Response::Request { id, .. } if id == first)
    );
    broker.dispatch_at("client", Command::Cancel { request: first }, now);
    assert_error_code(
        broker.dispatch_at(
            "provider",
            Command::AcceptRequest {
                request: first,
                receiver: "worker".into(),
            },
            now,
        ),
        "request_ended",
    );
    let second = accepted_id(broker.dispatch_at("client", request("deadline.offer"), now));
    assert!(
        matches!(broker.dispatch_at("provider", Command::PollV2 { receiver: "worker".into() }, now), Response::Request { id, .. } if id == second)
    );
    assert_error_code(
        broker.dispatch_at(
            "provider",
            Command::AcceptRequest {
                request: second,
                receiver: "worker".into(),
            },
            now + Duration::from_millis(101),
        ),
        "request_ended",
    );
    assert!(matches!(
        broker.dispatch_at(
            "provider",
            Command::PollV2 {
                receiver: "worker".into()
            },
            now + Duration::from_millis(101)
        ),
        Response::Pending
    ));
}

#[test]
fn legacy_delivery_cannot_switch_modes_until_the_active_request_ends() {
    let mut broker = test_broker();
    let id = accepted_id(broker.dispatch("client", request("legacy.active")));
    assert!(
        matches!(broker.dispatch("provider", Command::Poll {}), Response::Request { id: delivered, .. } if delivered == id)
    );
    assert_error_code(
        broker.dispatch(
            "provider",
            Command::PollV2 {
                receiver: "worker".into(),
            },
        ),
        "delivery_mode_mismatch",
    );
    assert_error_code(
        broker.dispatch(
            "provider",
            Command::SelectRequestDeliveryMode { reliable: true },
        ),
        "busy",
    );
    broker.dispatch("client", Command::Cancel { request: id });
    assert_error_code(
        broker.dispatch(
            "provider",
            Command::SelectRequestDeliveryMode { reliable: true },
        ),
        "busy",
    );
}

#[test]
fn legacy_success_allows_mode_switch_but_timeout_taints_past_result_retention() {
    let mut successful = test_broker();
    let now = Instant::now();
    let id = accepted_id(successful.dispatch_at("client", request("legacy.success"), now));
    successful.dispatch_at("provider", Command::Poll {}, now);
    assert!(matches!(
        successful.dispatch_at(
            "provider",
            Command::Complete {
                request: id,
                payload: json!({})
            },
            now
        ),
        Response::Ok
    ));
    assert!(matches!(
        successful.dispatch_at(
            "provider",
            Command::SelectRequestDeliveryMode { reliable: true },
            now
        ),
        Response::Ok
    ));
    let mut expired = test_broker();
    accepted_id(expired.dispatch_at("client", request("legacy.expired"), now));
    expired.dispatch_at("provider", Command::Poll {}, now);
    assert_error_code(
        expired.dispatch_at(
            "provider",
            Command::SelectRequestDeliveryMode { reliable: true },
            now + Duration::from_secs(301),
        ),
        "busy",
    );
    assert_error_code(
        expired.dispatch_at(
            "provider",
            Command::SelectRequestDeliveryMode { reliable: true },
            now + Duration::from_secs(602),
        ),
        "busy",
    );
}

#[test]
fn inspecting_offers_never_delivers_queued_work_and_observes_retirement() {
    let mut broker = test_broker();
    let now = Instant::now();
    let inspect = |receiver: &str| Command::InspectRequestOffer {
        receiver: receiver.into(),
    };
    let id = accepted_id(broker.dispatch_at("client", request("inspect.offer"), now));
    assert!(matches!(
        broker.dispatch_at("provider", inspect("worker"), now),
        Response::Pending
    ));
    assert!(matches!(
        broker.dispatch_at(
            "provider",
            Command::SelectRequestDeliveryMode { reliable: true },
            now
        ),
        Response::Ok
    ));
    assert!(matches!(
        broker.dispatch_at("provider", inspect("worker"), now),
        Response::Pending
    ));
    assert!(
        matches!(broker.dispatch_at("provider", Command::PollV2 { receiver: "worker".into() }, now), Response::Request { id: offered, .. } if offered == id)
    );
    assert!(
        matches!(broker.dispatch_at("provider", inspect("worker"), now + Duration::from_millis(50)), Response::Request { id: offered, remaining_ms, .. } if offered == id && remaining_ms <= 50)
    );
    assert!(matches!(
        broker.dispatch_at("provider", inspect("other"), now),
        Response::Pending
    ));
    broker.dispatch_at("client", Command::Cancel { request: id }, now);
    assert!(matches!(
        broker.dispatch_at("provider", inspect("worker"), now),
        Response::Pending
    ));
    let expired = accepted_id(broker.dispatch_at("client", request("inspect.deadline"), now));
    assert!(
        matches!(broker.dispatch_at("provider", Command::PollV2 { receiver: "worker".into() }, now), Response::Request { id: offered, .. } if offered == expired)
    );
    assert!(matches!(
        broker.dispatch_at(
            "provider",
            inspect("worker"),
            now + Duration::from_millis(101)
        ),
        Response::Pending
    ));
    assert!(matches!(
        broker.dispatch_at(
            "provider",
            Command::SelectRequestDeliveryMode { reliable: false },
            now + Duration::from_millis(101)
        ),
        Response::Ok
    ));
}

#[test]
fn replacement_receiver_waits_for_original_offer_to_end() {
    let mut broker = test_broker();
    let now = Instant::now();
    let first = accepted_id(broker.dispatch_at("client", request("old.receiver"), now));
    assert!(
        matches!(broker.dispatch_at("provider", Command::PollV2 { receiver: "old".into() }, now), Response::Request { id, .. } if id == first)
    );
    assert_error_code(
        broker.dispatch_at(
            "provider",
            Command::PollV2 {
                receiver: "new".into(),
            },
            now,
        ),
        "busy",
    );
    assert_error_code(
        broker.dispatch_at(
            "provider",
            Command::SelectRequestDeliveryMode { reliable: false },
            now,
        ),
        "busy",
    );
    let later = now + Duration::from_millis(101);
    let second = accepted_id(broker.dispatch_at("client", request("new.receiver"), later));
    assert!(
        matches!(broker.dispatch_at("provider", Command::PollV2 { receiver: "new".into() }, later), Response::Request { id, .. } if id == second)
    );
    assert_error_code(
        broker.dispatch_at(
            "provider",
            Command::AcceptRequest {
                request: first,
                receiver: "old".into(),
            },
            later,
        ),
        "request_ended",
    );
}

#[test]
fn concurrent_reliable_polls_observe_one_offer() {
    let broker = std::sync::Arc::new(std::sync::Mutex::new(test_broker()));
    let id = accepted_id(
        broker
            .lock()
            .unwrap()
            .dispatch("client", request("parallel.offer")),
    );
    let threads: Vec<_> = (0..8)
        .map(|index| {
            let broker = broker.clone();
            std::thread::spawn(move || {
                let receiver = format!("worker.{index}");
                let response = broker.lock().unwrap().dispatch(
                    "provider",
                    Command::PollV2 {
                        receiver: receiver.clone(),
                    },
                );
                (receiver, response)
            })
        })
        .collect();
    let mut winner = None;
    for thread in threads {
        let (receiver, response) = thread.join().unwrap();
        match response {
            Response::Request { id: offered, .. } => {
                assert_eq!(offered, id);
                assert!(winner.replace(receiver).is_none());
            }
            Response::Error { code, .. } => assert_eq!(code, "busy"),
            other => panic!("unexpected poll response: {other:?}"),
        }
    }
    let winner = winner.unwrap();
    assert_error_code(
        broker.lock().unwrap().dispatch(
            "provider",
            Command::AcceptRequest {
                request: id,
                receiver: "other".into(),
            },
        ),
        "permission_denied",
    );
    assert!(matches!(
        broker.lock().unwrap().dispatch(
            "provider",
            Command::AcceptRequest {
                request: id,
                receiver: winner.clone()
            }
        ),
        Response::Ok
    ));
    assert_error_code(
        broker.lock().unwrap().dispatch(
            "provider",
            Command::AcceptRequest {
                request: id,
                receiver: "other".into(),
            },
        ),
        "permission_denied",
    );
    assert!(matches!(
        broker.lock().unwrap().dispatch(
            "provider",
            Command::AcceptRequest {
                request: id,
                receiver: winner
            }
        ),
        Response::Ok
    ));
}

#[test]
fn request_lookup_is_caller_scoped_and_never_admits_work() {
    let mut configuration = RuntimeConfig::default();
    for caller in ["owner", "other"] {
        configuration.principals.insert(
            caller.into(),
            Grants {
                call: names(&["work/1"]),
                ..Default::default()
            },
        );
    }
    configuration.principals.insert(
        "provider".into(),
        Grants {
            provide: names(&["work/1"]),
            ..Default::default()
        },
    );
    let mut broker = Broker::new(configuration).unwrap();
    broker.dispatch(
        "provider",
        Command::Register {
            service: "work/1".into(),
        },
    );
    let now = Instant::now();
    let lookup = |payload, parent| Command::LookupRequest {
        service: "work/1".into(),
        payload,
        operation_id: "unique".into(),
        parent,
    };
    assert_error_code(
        broker.dispatch_at("owner", lookup(json!(1), None), now),
        "not_found",
    );
    assert!(matches!(
        broker.dispatch_at("provider", Command::Poll {}, now),
        Response::Pending
    ));
    let id = accepted_id(broker.dispatch_at(
        "owner",
        Command::Request {
            service: "work/1".into(),
            payload: json!(1),
            timeout_ms: 1000,
            operation_id: "unique".into(),
            parent: None,
        },
        now,
    ));
    assert_eq!(
        accepted_id(broker.dispatch_at("owner", lookup(json!(1), None), now)),
        id
    );
    assert_error_code(
        broker.dispatch_at("owner", lookup(json!(2), None), now),
        "conflict",
    );
    assert_error_code(
        broker.dispatch_at("owner", lookup(json!(1), Some(1)), now),
        "conflict",
    );
    assert_error_code(
        broker.dispatch_at("other", lookup(json!(1), None), now),
        "not_found",
    );
    assert!(
        matches!(broker.dispatch_at("provider", Command::Poll {}, now), Response::Request { id: delivered, .. } if delivered == id)
    );
    assert!(matches!(
        broker.dispatch_at("provider", Command::Poll {}, now),
        Response::Pending
    ));
    broker.dispatch_at(
        "provider",
        Command::Complete {
            request: id,
            payload: json!(true),
        },
        now,
    );
    assert_eq!(
        accepted_id(broker.dispatch_at("owner", lookup(json!(1), None), now)),
        id
    );
    assert_error_code(
        broker.dispatch_at(
            "owner",
            lookup(json!(1), None),
            now + Duration::from_secs(301),
        ),
        "not_found",
    );
    assert!(matches!(
        broker.dispatch_at("provider", Command::Poll {}, now + Duration::from_secs(301)),
        Response::Pending
    ));
}

#[test]
fn cancelling_a_hook_cancels_delegated_children_before_releasing_the_participant() {
    for cleanup in [false, true] {
        let mut configuration = RuntimeConfig::default();
        for principal in ["owner", "participant", "leaf"] {
            configuration.principals.insert(
                principal.into(),
                Grants {
                    call: names(&["work/1"]),
                    provide: names(&["work/1"]),
                    hook: names(&["prepare/1"]),
                    ..Default::default()
                },
            );
        }
        configuration.hooks.insert(
            "prepare/1".into(),
            HookSpec {
                owner: "owner".into(),
                participants: vec!["participant".into()],
                timeout_ms: 30_000,
            },
        );
        let mut broker = Broker::new(configuration).unwrap();
        let now = Instant::now();
        broker.dispatch_at(
            "leaf",
            Command::Register {
                service: "work/1".into(),
            },
            now,
        );
        broker.dispatch_at(
            "participant",
            Command::RegisterHook {
                hook: "prepare/1".into(),
            },
            now,
        );
        let invocation = accepted_id(broker.dispatch_at(
            "owner",
            Command::RunHookOperation {
                hook: "prepare/1".into(),
                payload: json!({}),
                operation_id: "cancel.delegated.hook".into(),
                cleanup,
                chain: false,
            },
            now,
        ));
        assert!(matches!(
            broker.dispatch_at("owner", Command::HookResult { invocation }, now),
            Response::Pending
        ));
        let Response::Request {
            id: participant, ..
        } = broker.dispatch_at("participant", Command::Poll {}, now)
        else {
            panic!("hook participant was not delivered");
        };
        let delegated = |operation: &str, parent| Command::Request {
            service: "work/1".into(),
            payload: json!({}),
            timeout_ms: 30_000,
            operation_id: operation.into(),
            parent: Some(parent),
        };
        let child =
            accepted_id(broker.dispatch_at("participant", delegated("child", participant), now));
        assert!(matches!(
            broker.dispatch_at("leaf", Command::Poll {}, now),
            Response::Request { id, .. } if id == child
        ));
        let grandchild =
            accepted_id(broker.dispatch_at("leaf", delegated("grandchild", child), now));
        let unrelated_parent = accepted_id(broker.dispatch_at(
            "owner",
            Command::Request {
                service: "work/1".into(),
                payload: json!({}),
                timeout_ms: 30_000,
                operation_id: "unrelated-parent".into(),
                parent: None,
            },
            now,
        ));
        assert!(matches!(
            broker.dispatch_at("leaf", Command::Poll {}, now),
            Response::Request { id, .. } if id == grandchild
        ));
        assert!(matches!(
            broker.dispatch_at("leaf", Command::Poll {}, now),
            Response::Request { id, .. } if id == unrelated_parent
        ));
        let unrelated_child = accepted_id(broker.dispatch_at(
            "leaf",
            delegated("unrelated-child", unrelated_parent),
            now,
        ));
        let queued_child =
            accepted_id(broker.dispatch_at("leaf", delegated("queued-child", child), now));

        assert!(matches!(
            broker.dispatch_at("owner", Command::CancelHook { invocation }, now),
            Response::Ok
        ));
        assert_error_code(
            broker.dispatch_at("leaf", Command::CheckRequest { request: child }, now),
            "request_ended",
        );
        assert_error_code(
            broker.dispatch_at("participant", Command::TakeResult { request: child }, now),
            "cancelled",
        );
        assert_error_code(
            broker.dispatch_at(
                "leaf",
                Command::TakeResult {
                    request: grandchild,
                },
                now,
            ),
            "cancelled",
        );
        assert_error_code(
            broker.dispatch_at(
                "leaf",
                Command::TakeResult {
                    request: queued_child,
                },
                now,
            ),
            "cancelled",
        );
        assert!(matches!(
            broker.dispatch_at(
                "leaf",
                Command::CheckRequest {
                    request: unrelated_parent
                },
                now
            ),
            Response::Ok
        ));
        assert!(matches!(
            broker.dispatch_at("leaf", Command::Poll {}, now),
            Response::Request { id, .. } if id == unrelated_child
        ));
        assert!(matches!(
            broker.dispatch_at(
                "leaf",
                Command::CheckRequest {
                    request: unrelated_child
                },
                now
            ),
            Response::Ok
        ));
        assert!(matches!(
            broker.dispatch_at("leaf", Command::Poll {}, now),
            Response::Pending
        ));
        assert_error_code(
            broker.dispatch_at("participant", delegated("late-child", participant), now),
            "permission_denied",
        );
        for (caller, request) in [
            ("participant", child),
            ("leaf", grandchild),
            ("leaf", queued_child),
        ] {
            assert!(matches!(
                broker.dispatch_at(caller, Command::ReleaseResult { request }, now),
                Response::Ok
            ));
            assert_error_code(
                broker.dispatch_at(caller, Command::TakeResult { request }, now),
                "not_found",
            );
        }
    }
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
    assert_error_code(
        broker.dispatch(
            "client",
            Command::LookupRequest {
                service: "echo/1".into(),
                payload: json!({}),
                operation_id: format!("hook.{invocation}.provider"),
                parent: None,
            },
        ),
        "not_found",
    );
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

#[test]
fn hook_operation_lookup_cancel_and_expired_identity_are_owner_scoped() {
    let mut broker = test_broker();
    broker.dispatch(
        "provider",
        Command::RegisterHook {
            hook: "prepare/1".into(),
        },
    );
    let start = Instant::now();
    let operation = || Command::RunHookOperation {
        hook: "prepare/1".into(),
        payload: json!({"x":1}),
        operation_id: "op.1".into(),
        cleanup: false,
        chain: false,
    };
    let id = accepted_id(broker.dispatch_at("client", operation(), start));
    assert_eq!(
        accepted_id(broker.dispatch_at("client", operation(), start)),
        id
    );
    assert_error_code(
        broker.dispatch(
            "client",
            Command::RunHookOperation {
                hook: "prepare/1".into(),
                payload: json!({"x":2}),
                operation_id: "op.1".into(),
                cleanup: false,
                chain: false,
            },
        ),
        "conflict",
    );
    assert_error_code(
        broker.dispatch(
            "outsider",
            Command::LookupHook {
                operation_id: "op.1".into(),
            },
        ),
        "not_found",
    );
    assert!(matches!(
        broker.dispatch_at("client", Command::HookResult { invocation: id }, start),
        Response::Pending
    ));
    let Response::Request { id: request, .. } =
        broker.dispatch_at("provider", Command::Poll {}, start)
    else {
        panic!()
    };
    assert_error_code(
        broker.dispatch_at("outsider", Command::CancelHook { invocation: id }, start),
        "permission_denied",
    );
    assert!(matches!(
        broker.dispatch_at("client", Command::CancelHook { invocation: id }, start),
        Response::Ok
    ));
    assert_error_code(
        broker.dispatch_at(
            "provider",
            Command::Complete {
                request,
                payload: json!({}),
            },
            start,
        ),
        "not_found",
    );
    assert_error_code(
        broker.dispatch_at("client", Command::HookResult { invocation: id }, start),
        "cancelled",
    );
    assert!(matches!(
        broker.dispatch_at(
            "client",
            Command::ReleaseHook {
                invocation: id,
                forget: false,
            },
            start
        ),
        Response::Ok
    ));
    assert_error_code(
        broker.dispatch_at("client", operation(), start),
        "operation_expired",
    );
}

#[test]
fn cleanup_hook_continues_after_absent_participant() {
    let mut config = RuntimeConfig::default();
    config.principals.insert("client".into(), Grants::default());
    for participant in ["missing", "provider"] {
        config.principals.insert(
            participant.into(),
            Grants {
                hook: names(&["release/1"]),
                ..Default::default()
            },
        );
    }
    config.hooks.insert(
        "release/1".into(),
        HookSpec {
            owner: "client".into(),
            participants: vec!["missing".into(), "provider".into()],
            timeout_ms: 1000,
        },
    );
    let mut broker = Broker::new(config).unwrap();
    broker.dispatch(
        "provider",
        Command::RegisterHook {
            hook: "release/1".into(),
        },
    );
    let id = accepted_id(broker.dispatch(
        "client",
        Command::RunHookOperation {
            hook: "release/1".into(),
            payload: json!({}),
            operation_id: "cleanup.1".into(),
            cleanup: true,
            chain: false,
        },
    ));
    assert!(matches!(
        broker.dispatch("client", Command::HookResult { invocation: id }),
        Response::Pending
    ));
    let Response::Request { id: request, .. } = broker.dispatch("provider", Command::Poll {})
    else {
        panic!()
    };
    broker.dispatch(
        "provider",
        Command::Complete {
            request,
            payload: json!({"released":true}),
        },
    );
    let Response::HookCleanupResult { outcomes } =
        broker.dispatch("client", Command::HookResult { invocation: id })
    else {
        panic!()
    };
    assert_eq!(outcomes.len(), 2);
    assert_eq!(outcomes[0].participant, "missing");
    assert!(matches!(&*outcomes[0].result, Response::Error { code, .. } if code == "unavailable"));
    assert_eq!(outcomes[1].participant, "provider");
    assert!(matches!(&*outcomes[1].result, Response::Result { .. }));
    assert!(matches!(
        broker.dispatch("client", Command::HookResult { invocation: id }),
        Response::HookCleanupResult { .. }
    ));
}

#[test]
fn api_approval_discovery_intersects_delegation_and_fences_pages() {
    let (mut config, method) = synthetic_api(ApiPrivilege::User);
    let mut second = method.clone();
    second.method = "resize".into();
    config.api_methods[0]
        .callers
        .insert("org.example.registry".into());
    let mut second_approval = config.api_methods[0].clone();
    second_approval.method = second.clone();
    config.api_methods.push(second_approval);
    config.principals.insert(
        "org.example.registry".into(),
        Grants {
            provide: names(&["registry/1"]),
            call: names(&["org.example.paint/1"]),
            ..Default::default()
        },
    );
    for principal in ["org.example.client", "org.example.outsider"] {
        config
            .principals
            .get_mut(principal)
            .unwrap()
            .call
            .insert("registry/1".into());
    }
    let mut broker = Broker::new(config).unwrap();
    broker.dispatch(
        "org.example.registry",
        Command::Register {
            service: "registry/1".into(),
        },
    );
    broker.dispatch(
        "org.example.provider",
        Command::RegisterApi {
            method: method.clone(),
        },
    );
    let list = |parent, after| Command::ListApiApprovals {
        parent,
        after,
        limit: 1,
    };
    assert!(
        matches!(broker.dispatch("org.example.outsider", list(None, None)), Response::ApiApprovals { approvals, .. } if approvals.is_empty())
    );
    let request = accepted_id(broker.dispatch(
        "org.example.outsider",
        Command::Request {
            service: "registry/1".into(),
            payload: json!({}),
            timeout_ms: 1000,
            operation_id: "parent.1".into(),
            parent: None,
        },
    ));
    assert!(
        matches!(broker.dispatch("org.example.registry", Command::Poll {}), Response::Request { id, .. } if id == request)
    );
    assert!(
        matches!(broker.dispatch("org.example.registry", list(Some(request), None)), Response::ApiApprovals { approvals, .. } if approvals.is_empty())
    );
    assert_error_code(
        broker.dispatch("org.example.provider", list(Some(request), None)),
        "permission_denied",
    );
    broker.dispatch(
        "org.example.provider",
        Command::RegisterApi { method: second },
    );
    let Response::ApiApprovals {
        approvals,
        next: Some(cursor),
        ..
    } = broker.dispatch("org.example.client", list(None, None))
    else {
        panic!()
    };
    assert_eq!(approvals.len(), 1);
    assert!(
        matches!(broker.dispatch("org.example.client", list(None, Some(cursor.clone()))), Response::ApiApprovals { approvals, next: None, .. } if approvals.len() == 1)
    );
    let mut stale = cursor;
    stale.revision -= 1;
    assert_error_code(
        broker.dispatch("org.example.client", list(None, Some(stale))),
        "stale_revision",
    );
}

#[test]
fn cleanup_deadline_reports_unknown_and_unattempted_participants() {
    let mut config = RuntimeConfig::default();
    config.principals.insert("client".into(), Grants::default());
    for participant in ["first", "second"] {
        config.principals.insert(
            participant.into(),
            Grants {
                hook: names(&["release/1"]),
                ..Default::default()
            },
        );
    }
    config.hooks.insert(
        "release/1".into(),
        HookSpec {
            owner: "client".into(),
            participants: vec!["first".into(), "second".into()],
            timeout_ms: 100,
        },
    );
    let mut broker = Broker::new(config).unwrap();
    for participant in ["first", "second"] {
        broker.dispatch(
            participant,
            Command::RegisterHook {
                hook: "release/1".into(),
            },
        );
    }
    let now = Instant::now();
    let id = accepted_id(broker.dispatch_at(
        "client",
        Command::RunHookOperation {
            hook: "release/1".into(),
            payload: json!({}),
            operation_id: "cleanup.2".into(),
            cleanup: true,
            chain: false,
        },
        now,
    ));
    assert!(matches!(
        broker.dispatch_at("client", Command::HookResult { invocation: id }, now),
        Response::Pending
    ));
    let Response::HookCleanupResult { outcomes } = broker.dispatch_at(
        "client",
        Command::HookResult { invocation: id },
        now + Duration::from_millis(100),
    ) else {
        panic!()
    };
    assert_eq!(outcomes.len(), 2);
    assert!(
        matches!(&*outcomes[0].result, Response::Error { message, .. } if message.contains("unknown"))
    );
    assert!(
        matches!(&*outcomes[1].result, Response::Error { message, .. } if message.contains("not attempted"))
    );
}

#[test]
fn completed_hook_result_survives_a_poll_after_the_deadline() {
    let mut broker = test_broker();
    broker.dispatch(
        "provider",
        Command::RegisterHook {
            hook: "prepare/1".into(),
        },
    );
    let now = Instant::now();
    let invocation = accepted_id(broker.dispatch_at(
        "client",
        Command::RunHookOperation {
            hook: "prepare/1".into(),
            payload: json!({}),
            operation_id: "delayed.poll".into(),
            cleanup: true,
            chain: false,
        },
        now,
    ));
    assert!(matches!(
        broker.dispatch_at("client", Command::HookResult { invocation }, now),
        Response::Pending
    ));
    let Response::Request { id, .. } = broker.dispatch_at("provider", Command::Poll {}, now) else {
        panic!()
    };
    assert!(matches!(
        broker.dispatch_at(
            "provider",
            Command::Complete {
                request: id,
                payload: json!({"released":true})
            },
            now + Duration::from_millis(50)
        ),
        Response::Ok
    ));
    let Response::HookCleanupResult { outcomes } = broker.dispatch_at(
        "client",
        Command::HookResult { invocation },
        now + Duration::from_millis(101),
    ) else {
        panic!()
    };
    assert!(
        matches!(&*outcomes[0].result, Response::Result { payload } if payload == &json!({"released":true}))
    );
}

#[test]
fn legacy_native_contract_approval_requires_valid_names_and_provider_grant() {
    let mut config = RuntimeConfig::default();
    config.principals.insert(
        "provider".into(),
        Grants {
            native_provide: ["capture/1".into()].into(),
            ..Default::default()
        },
    );
    let approvals = |principal: &str, service: &str, contract: &str| {
        [(principal.into(), [(service.into(), contract.into())].into())].into()
    };
    config.legacy_native_contracts = approvals("provider", "capture/1", "capture.native/1");
    assert!(Broker::new(config.clone()).is_ok());
    for (principal, service, contract) in [
        ("unknown", "capture/1", "capture.native/1"),
        ("provider", "other/1", "capture.native/1"),
        ("provider", "capture/1", "INVALID"),
        ("provider", "invalid service", "capture.native/1"),
    ] {
        config.legacy_native_contracts = approvals(principal, service, contract);
        assert!(Broker::new(config.clone()).is_err());
    }
}

#[test]
fn package_activation_requires_host_capability_management_grant_and_bounded_queue() {
    let mut config = RuntimeConfig {
        host_lifecycle_control: true,
        host_package_activation_control: true,
        ..Default::default()
    };
    config
        .principals
        .insert(HOST_LIFECYCLE_CONTROL_PRINCIPAL.into(), Grants::default());
    let grant = Grants {
        call: names(&[HOST_PACKAGE_ACTIVATION_SERVICE]),
        ..Default::default()
    };
    config
        .principals
        .insert("amaterasu.management".into(), grant.clone());
    config.principals.insert("other".into(), grant);
    config.principals.insert(
        "example.stream-provider".into(),
        Grants {
            call: names(&[HOST_PACKAGE_BARRIER_SERVICE]),
            ..Default::default()
        },
    );
    config.principals.insert(
        "example.session-provider".into(),
        Grants {
            call: names(&[HOST_PACKAGE_BARRIER_SERVICE]),
            ..Default::default()
        },
    );
    let mut broker = Broker::new(config.clone()).unwrap();
    let request = HostPackageActivation {
        repository_id: "approved-repository".into(),
        catalog_digest: "1".repeat(64),
        manifest_digest: "2".repeat(64),
        expected_active: "3".repeat(64),
        expected_revision: "4".repeat(64),
    };
    let command = || Command::HostPackageActivate {
        request: request.clone(),
    };
    assert!(broker.dispatch("other", command()).is_error());
    assert!(
        broker
            .dispatch("amaterasu.management", command())
            .is_error()
    );
    assert!(
        broker
            .dispatch("other", Command::HostPackageBarrierRegister {})
            .is_error()
    );
    assert!(
        !broker
            .dispatch(
                "example.stream-provider",
                Command::HostPackageBarrierRegister {}
            )
            .is_error()
    );
    assert!(
        broker
            .dispatch("amaterasu.management", command())
            .is_error()
    );
    assert!(
        !broker
            .dispatch(
                "example.session-provider",
                Command::HostPackageBarrierRegister {}
            )
            .is_error()
    );
    assert!(
        broker
            .dispatch(
                "amaterasu.management",
                Command::HostPackageActivationDrain {}
            )
            .is_error()
    );
    assert!(
        !broker
            .dispatch("amaterasu.management", command())
            .is_error()
    );
    assert!(matches!(
        broker.dispatch(
            HOST_LIFECYCLE_CONTROL_PRINCIPAL,
            Command::HostPackageActivationDrain {}
        ),
        Response::HostPackageActivation { request: None }
    ));
    assert!(matches!(
        broker.dispatch(
            "example.stream-provider",
            Command::HostPackageBarrierPending {}
        ),
        Response::HostPackageActivationState {
            pending: true,
            started: false
        }
    ));
    assert!(
        broker
            .dispatch("other", Command::HostPackageBarrierReady { ready: true })
            .is_error()
    );
    assert!(
        !broker
            .dispatch(
                "example.stream-provider",
                Command::HostPackageBarrierReady { ready: true }
            )
            .is_error()
    );
    assert!(matches!(
        broker.dispatch(
            HOST_LIFECYCLE_CONTROL_PRINCIPAL,
            Command::HostPackageActivationDrain {}
        ),
        Response::HostPackageActivation { request: None }
    ));
    assert!(
        !broker
            .dispatch(
                "example.session-provider",
                Command::HostPackageBarrierReady { ready: true }
            )
            .is_error()
    );
    assert!(
        broker
            .dispatch("amaterasu.management", command())
            .is_error()
    );
    assert!(
        matches!(broker.dispatch(HOST_LIFECYCLE_CONTROL_PRINCIPAL, Command::HostPackageActivationDrain {}), Response::HostPackageActivation { request: Some(value) } if *value == request)
    );
    assert!(matches!(
        broker.dispatch(
            "example.stream-provider",
            Command::HostPackageBarrierPending {}
        ),
        Response::HostPackageActivationState {
            pending: true,
            started: true
        }
    ));
    assert!(
        broker
            .dispatch(
                "amaterasu.management",
                Command::HostPackageActivationCancel {}
            )
            .is_error()
    );
    let mut invalid = request.clone();
    invalid.manifest_digest = "bad".into();
    assert!(
        broker
            .dispatch(
                "amaterasu.management",
                Command::HostPackageActivate { request: invalid }
            )
            .is_error()
    );
    let mut cancellable = Broker::new(config.clone()).unwrap();
    assert!(
        !cancellable
            .dispatch(
                "example.stream-provider",
                Command::HostPackageBarrierRegister {}
            )
            .is_error()
    );
    assert!(
        !cancellable
            .dispatch(
                "example.session-provider",
                Command::HostPackageBarrierRegister {}
            )
            .is_error()
    );
    assert!(
        !cancellable
            .dispatch("amaterasu.management", command())
            .is_error()
    );
    assert!(
        !cancellable
            .dispatch(
                "amaterasu.management",
                Command::HostPackageActivationCancel {}
            )
            .is_error()
    );
    assert!(matches!(
        cancellable.dispatch(
            "example.stream-provider",
            Command::HostPackageBarrierPending {}
        ),
        Response::HostPackageActivationState {
            pending: false,
            started: false
        }
    ));
    config.host_package_activation_control = false;
    let mut disabled = Broker::new(config).unwrap();
    assert!(
        disabled
            .dispatch("amaterasu.management", command())
            .is_error()
    );
}

#[test]
fn operation_api_consumers_remain_unchanged_when_the_provider_is_replaced() {
    use amaterasu_module_sdk::operation_api;
    let mut contracts = vec![operation_api::method(
        "example.telemetry/1",
        "snapshot",
        &[],
        &[],
    )];
    for (schema, name) in [
        (
            include_str!("../../../modules/display/metadata/api.json"),
            "snapshot",
        ),
        (
            include_str!("../../../modules/catalog/metadata/api.json"),
            "get",
        ),
        (
            include_str!("../../../modules/nvhttp/metadata/api.json"),
            "read",
        ),
    ] {
        let methods: Vec<amaterasu_contracts::ApiMethod> = serde_json::from_str(schema).unwrap();
        contracts.push(
            methods
                .into_iter()
                .find(|method| method.method == name)
                .unwrap(),
        );
    }
    for method in contracts {
        let service = method.service.as_str();
        let arguments = if method.method == "get" {
            json!({"id":"app-0000000000000001"})
        } else {
            json!({})
        };
        // This consumer knows only the versioned API, never either implementation.
        let read = || Command::CallApi {
            service: service.into(),
            method: method.method.clone(),
            interface_major: 1,
            interface_minor: 0,
            payload: arguments.clone(),
            timeout_ms: 1000,
            operation_id: "sample".into(),
            parent: None,
        };
        for (provider, reading) in [("vendor.primary", 42), ("vendor.replacement", 73)] {
            let mut config = RuntimeConfig::default();
            config.principals.insert(
                provider.into(),
                Grants {
                    provide: names(&[service]),
                    ..Default::default()
                },
            );
            config.principals.insert(
                "consumer".into(),
                Grants {
                    call: names(&[service]),
                    ..Default::default()
                },
            );
            config.api_methods.push(ApiMethodApproval {
                provider: provider.into(),
                callers: names(&["consumer"]),
                method: method.clone(),
            });
            let mut broker = Broker::new(config).unwrap();
            assert!(matches!(
                broker.dispatch(
                    provider,
                    Command::RegisterApi {
                        method: method.clone()
                    }
                ),
                Response::Ok
            ));
            let Response::Accepted { id } = broker.dispatch("consumer", read()) else {
                panic!("admission");
            };
            let Response::Request { payload, .. } = broker.dispatch(provider, Command::Poll {})
            else {
                panic!("delivery");
            };
            assert_eq!(
                operation_api::decode_delivery(
                    payload,
                    service,
                    "legacy/1",
                    std::slice::from_ref(&method)
                )
                .unwrap(),
                {
                    let mut expected = arguments.clone();
                    expected["action"] = method.method.clone().into();
                    expected
                }
            );
            assert!(matches!(
                broker.dispatch(
                    provider,
                    Command::Complete {
                        request: id,
                        payload: json!({"ok":true,"value":reading})
                    }
                ),
                Response::Ok
            ));
            assert!(
                matches!(broker.dispatch("consumer",Command::TakeResult {request:id}),Response::Result {payload} if payload["value"]==reading)
            );
            assert!(matches!(
                broker.dispatch("consumer", Command::ReleaseResult { request: id }),
                Response::Ok
            ));
            assert!(
                operation_api::decode_delivery(
                    json!({"action":"write"}),
                    service,
                    "legacy/1",
                    std::slice::from_ref(&method)
                )
                .is_err()
            );
        }
    }
}
