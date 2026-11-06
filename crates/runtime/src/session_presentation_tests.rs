//! The protocol consumer remains identical when trusted composition selects a
//! different implementation of the public session-presentation contract.
use super::Broker;
use amaterasu_contracts::{ApiMethod, ApiMethodApproval, Command, Grants, Response, RuntimeConfig};
use amaterasu_module_sdk::requests::RequestOptions;
use amaterasu_module_sdk::session_presentation::{
    self, Application, Client, Context, Game, Presentation, Reply, Request, SERVICE,
};
use serde_json::{Value, json};

const CONSUMER: &str = "org.example.protocol";
const OUTSIDER: &str = "org.example.outsider";
const UNSELECTED: &str = "org.example.unselected";

fn context() -> Context {
    Context {
        client: Client {
            id: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".into(),
            paired: true,
            may_view: true,
            may_launch: true,
            may_terminate: false,
        },
        game: Some(Game {
            application: Application {
                id: "game-opaque-id".into(),
                uuid: "b063b169-a0c7-47de-99c6-bd945008876f".into(),
                title: "Running game".into(),
                aliases: Vec::new(),
                visible: true,
                monitor_profile: false,
                properties: json!({}),
            },
            owner_client_id: "another-authenticated-client".into(),
            generation: 17,
        }),
    }
}

// This consumer only knows the versioned public contract and authenticated
// context. It has no branch for either provider principal or product policy.
fn info_command(operation_id: &str) -> Command {
    session_presentation::command(
        RequestOptions {
            service: SERVICE,
            timeout_ms: 1_000,
            operation_id,
            parent: None,
        },
        &Request::Info { context: context() },
    )
    .unwrap()
}

fn selected_broker(provider: &str) -> Broker {
    let mut policy = RuntimeConfig::default();
    for caller in [CONSUMER, OUTSIDER] {
        policy.principals.insert(
            caller.into(),
            Grants {
                call: [SERVICE.into()].into(),
                ..Default::default()
            },
        );
    }
    for principal in [provider, UNSELECTED] {
        policy.principals.insert(
            principal.into(),
            Grants {
                provide: [SERVICE.into()].into(),
                ..Default::default()
            },
        );
    }
    for method in session_presentation::methods() {
        policy.api_methods.push(ApiMethodApproval {
            provider: provider.into(),
            callers: [CONSUMER.into()].into(),
            method,
        });
    }
    Broker::new(policy).unwrap()
}

fn register(broker: &mut Broker, provider: &str) {
    for method in session_presentation::methods() {
        assert_ok(broker.dispatch(provider, Command::RegisterApi { method }));
    }
}

fn info_method() -> ApiMethod {
    session_presentation::methods()
        .into_iter()
        .find(|method| method.method == "info")
        .unwrap()
}

fn accepted(response: Response) -> u64 {
    match response {
        Response::Accepted { id } => id,
        other => panic!("expected request admission, got {other:?}"),
    }
}

fn assert_ok(response: Response) {
    assert!(matches!(response, Response::Ok), "{response:?}");
}

fn assert_error(response: Response, expected: &str) {
    assert!(
        matches!(&response, Response::Error { code, .. } if code == expected),
        "expected {expected}, got {response:?}"
    );
}

fn deliver_info(broker: &mut Broker, provider: &str, request: u64) {
    let Response::Request {
        id,
        caller,
        initiator,
        service,
        payload,
        ..
    } = broker.dispatch(provider, Command::Poll {})
    else {
        panic!("selected provider did not receive the typed API request");
    };
    assert_eq!(id, request);
    assert_eq!(caller, CONSUMER);
    assert_eq!(initiator, CONSUMER);
    assert_eq!(service, SERVICE);
    assert_eq!(
        session_presentation::decode(&service, payload).unwrap(),
        Request::Info { context: context() }
    );
}

fn complete_info(broker: &mut Broker, provider: &str, request: u64, presentation: Presentation) {
    assert_ok(broker.dispatch(
        provider,
        Command::Complete {
            request,
            payload: serde_json::to_value(Reply::Info { presentation }).unwrap(),
        },
    ));
}

fn consume_info(broker: &mut Broker, request: u64) -> Presentation {
    let Response::Result { payload } = broker.dispatch(CONSUMER, Command::TakeResult { request })
    else {
        panic!("consumer did not receive the provider result");
    };
    let Reply::Info { presentation } = session_presentation::decode_reply(payload).unwrap() else {
        panic!("provider returned an incompatible method result");
    };
    assert_ok(broker.dispatch(CONSUMER, Command::ReleaseResult { request }));
    presentation
}

#[test]
fn session_presentation_consumer_survives_selected_provider_replacement() {
    let unchanged_call = serde_json::to_value(info_command("presentation-info")).unwrap();
    let presentations = [
        Presentation {
            busy: false,
            current_game: None,
            current_game_uuid: None,
        },
        Presentation {
            busy: true,
            current_game: Some("game-opaque-id".into()),
            current_game_uuid: Some("b063b169-a0c7-47de-99c6-bd945008876f".into()),
        },
    ];
    let mut actual = Vec::new();
    for (provider, presentation) in ["vendor.primary", "vendor.replacement"]
        .into_iter()
        .zip(presentations.iter())
    {
        let mut broker = selected_broker(provider);
        register(&mut broker, provider);
        let Response::Result { payload } = broker.dispatch(
            CONSUMER,
            Command::QueryApiApproval {
                service: SERVICE.into(),
                method: "info".into(),
                interface_major: 1,
                min_minor: 0,
            },
        ) else {
            panic!("consumer could not discover its selected API approval");
        };
        let approval: ApiMethodApproval = serde_json::from_value(payload).unwrap();
        assert_eq!(approval.provider, provider);
        assert_eq!(approval.method, info_method());
        assert_eq!(
            serde_json::to_value(info_command("presentation-info")).unwrap(),
            unchanged_call
        );
        let request = accepted(broker.dispatch(CONSUMER, info_command("presentation-info")));
        deliver_info(&mut broker, provider, request);
        assert!(matches!(
            broker.dispatch(UNSELECTED, Command::Poll {}),
            Response::Pending
        ));
        complete_info(&mut broker, provider, request, presentation.clone());
        actual.push(consume_info(&mut broker, request));
        assert_error(
            broker.dispatch(CONSUMER, Command::TakeResult { request }),
            "not_found",
        );
    }
    assert_eq!(actual, presentations);
}

#[test]
fn session_presentation_schema_identity_and_selected_grants_are_enforced() {
    let provider = "vendor.primary";
    let mut broker = selected_broker(provider);
    let method = info_method();
    // An unavailable selected provider never causes an implicit raw-service
    // fallback to another component that happens to hold a provide grant.
    assert_error(
        broker.dispatch(CONSUMER, info_command("provider-unavailable")),
        "permission_denied",
    );
    assert_error(
        broker.dispatch(
            UNSELECTED,
            Command::Register {
                service: SERVICE.into(),
            },
        ),
        "permission_denied",
    );
    assert_error(
        broker.dispatch(
            UNSELECTED,
            Command::RegisterApi {
                method: method.clone(),
            },
        ),
        "permission_denied",
    );
    let mut altered = method.clone();
    altered.request.allow_extra = true;
    assert_error(
        broker.dispatch(provider, Command::RegisterApi { method: altered }),
        "permission_denied",
    );
    // The same service name and version do not approve a different response
    // schema either. Composition selects the full published method contract.
    let mut altered = method.clone();
    altered.response.required.clear();
    assert_error(
        broker.dispatch(provider, Command::RegisterApi { method: altered }),
        "permission_denied",
    );
    register(&mut broker, provider);
    assert_error(
        broker.dispatch(OUTSIDER, info_command("unapproved-consumer")),
        "permission_denied",
    );
    let mut wrong_major = info_command("wrong-major");
    let Command::CallApi {
        interface_major, ..
    } = &mut wrong_major
    else {
        panic!("typed helper did not build an API invocation");
    };
    *interface_major = 2;
    // A different major identifies another contract and has no installed
    // approval. A minor mismatch within an approved major is incompatible.
    assert_error(broker.dispatch(CONSUMER, wrong_major), "permission_denied");
    let mut wrong_minor = info_command("wrong-minor");
    let Command::CallApi {
        interface_minor, ..
    } = &mut wrong_minor
    else {
        unreachable!();
    };
    *interface_minor = 1;
    assert_error(broker.dispatch(CONSUMER, wrong_minor), "incompatible_api");
    let mut extra_field = info_command("extra-field");
    let Command::CallApi { payload, .. } = &mut extra_field else {
        unreachable!();
    };
    payload["provider"] = json!(UNSELECTED);
    assert_error(broker.dispatch(CONSUMER, extra_field), "invalid_request");
    assert_error(
        broker.dispatch(
            CONSUMER,
            Command::Request {
                service: SERVICE.into(),
                payload: serde_json::to_value(Request::Info { context: context() }).unwrap(),
                timeout_ms: 1_000,
                operation_id: "untyped-route".into(),
                parent: None,
            },
        ),
        "permission_denied",
    );
    assert!(
        session_presentation::decode(
            SERVICE,
            serde_json::to_value(Request::Info { context: context() }).unwrap()
        )
        .is_err()
    );
}

#[test]
fn session_presentation_results_and_cancellation_retain_exact_caller_authority() {
    let provider = "vendor.primary";
    let mut broker = selected_broker(provider);
    register(&mut broker, provider);
    let request = accepted(broker.dispatch(CONSUMER, info_command("owned-result")));
    assert_eq!(
        accepted(broker.dispatch(CONSUMER, info_command("owned-result"))),
        request
    );
    deliver_info(&mut broker, provider, request);
    for outsider in [provider, OUTSIDER] {
        assert_error(
            broker.dispatch(outsider, Command::TakeResult { request }),
            "permission_denied",
        );
        assert_error(
            broker.dispatch(outsider, Command::ReleaseResult { request }),
            "permission_denied",
        );
        assert_error(
            broker.dispatch(outsider, Command::Cancel { request }),
            "permission_denied",
        );
    }
    assert_error(
        broker.dispatch(CONSUMER, Command::ReleaseResult { request }),
        "request_pending",
    );
    complete_info(
        &mut broker,
        provider,
        request,
        Presentation {
            busy: false,
            current_game: None,
            current_game_uuid: None,
        },
    );
    for outsider in [provider, OUTSIDER] {
        assert_error(
            broker.dispatch(outsider, Command::ReleaseResult { request }),
            "permission_denied",
        );
    }
    consume_info(&mut broker, request);
    let replacement = accepted(broker.dispatch(CONSUMER, info_command("owned-result")));
    assert!(replacement > request);
    deliver_info(&mut broker, provider, replacement);
    assert_ok(broker.dispatch(
        CONSUMER,
        Command::Cancel {
            request: replacement,
        },
    ));
    assert_error(
        broker.dispatch(
            provider,
            Command::CheckRequest {
                request: replacement,
            },
        ),
        "request_ended",
    );
    assert_error(
        broker.dispatch(
            provider,
            Command::Complete {
                request: replacement,
                payload: serde_json::to_value(Reply::Acknowledged {}).unwrap(),
            },
        ),
        "request_ended",
    );
    assert_ok(broker.dispatch(
        CONSUMER,
        Command::ReleaseResult {
            request: replacement,
        },
    ));
}

#[test]
fn session_presentation_consumed_and_cancelled_calls_do_not_exhaust_broker_capacity() {
    let provider = "vendor.replacement";
    let mut broker = selected_broker(provider);
    register(&mut broker, provider);
    // The broker retains at most 512 requests. More than twice that number
    // exercises the SDK consumer's normal consumption and cancellation paths.
    for sequence in 0..1_100 {
        let operation_id = format!("presentation-{sequence}");
        let request = accepted(broker.dispatch(CONSUMER, info_command(&operation_id)));
        if sequence % 2 == 0 {
            deliver_info(&mut broker, provider, request);
            complete_info(
                &mut broker,
                provider,
                request,
                Presentation {
                    busy: false,
                    current_game: None,
                    current_game_uuid: None,
                },
            );
            consume_info(&mut broker, request);
        } else {
            assert_ok(broker.dispatch(CONSUMER, Command::Cancel { request }));
            assert_ok(broker.dispatch(CONSUMER, Command::ReleaseResult { request }));
        }
    }
    assert!(matches!(
        broker.dispatch(provider, Command::Poll {}),
        Response::Pending
    ));
}

#[test]
fn session_presentation_response_schema_is_checked_before_result_completion() {
    let provider = "vendor.primary";
    let mut broker = selected_broker(provider);
    register(&mut broker, provider);
    let request = accepted(broker.dispatch(CONSUMER, info_command("response-schema")));
    deliver_info(&mut broker, provider, request);
    for invalid in [json!({}), json!({"result":false}), Value::Null] {
        assert_error(
            broker.dispatch(
                provider,
                Command::Complete {
                    request,
                    payload: invalid,
                },
            ),
            "invalid_request",
        );
    }
    complete_info(
        &mut broker,
        provider,
        request,
        Presentation {
            busy: false,
            current_game: None,
            current_game_uuid: None,
        },
    );
    consume_info(&mut broker, request);
}
