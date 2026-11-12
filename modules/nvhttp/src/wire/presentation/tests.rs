use super::*;

#[test]
fn broker_call_retains_mutation_result_when_release_acknowledgement_is_lost() {
    let mut call = BrokerCall {
        command: Command::Request {
            service: "example/1".into(),
            payload: serde_json::json!({}),
            operation_id: "example".into(),
            timeout_ms: 1000,
            parent: None,
        },
        request: None,
        cancelling: false,
        terminal: None,
        first_sent: None,
        admission_uncertain: false,
    };
    let mut step = 0;
    let mut send = |command| {
        step += 1;
        match (step, command) {
            (1, Command::Request { .. }) => Ok(Response::Accepted { id: 3 }),
            (2, Command::TakeResult { request: 3 }) => Ok(Response::Result {
                payload: serde_json::json!({"owned": 42}),
            }),
            (3, Command::ReleaseResult { request: 3 }) => Err("lost acknowledgement".into()),
            (4, Command::ReleaseResult { request: 3 }) => {
                Ok(Response::error("not_found", "released"))
            }
            _ => panic!("unexpected command"),
        }
    };
    assert!(matches!(call.poll(&mut send), CallOutcome::Waiting));
    assert!(matches!(call.poll(&mut send), CallOutcome::Waiting));
    // The caller may disconnect after the provider's result was observed but
    // before its broker release was acknowledged. Its identity is still needed
    // to retire the just-created projection.
    call.cancelling = true;
    assert!(
        matches!(call.poll(&mut send), CallOutcome::Result(payload) if payload == serde_json::json!({"owned": 42}))
    );
}

#[test]
fn cancelled_mutation_waits_for_provider_and_never_replays_unknown_admission() {
    let command = Command::Request {
        service: "example/1".into(),
        payload: serde_json::json!({}),
        operation_id: "operation".into(),
        timeout_ms: 1000,
        parent: None,
    };
    let mut call = BrokerCall {
        command,
        request: None,
        cancelling: false,
        terminal: None,
        first_sent: None,
        admission_uncertain: false,
    };
    assert!(matches!(
        call.poll(&mut |_| Err("lost admission".into())),
        CallOutcome::Waiting
    ));
    call.cancelling = true;
    assert!(matches!(
        call.poll(&mut |command| {
            assert!(matches!(command, Command::LookupRequest { .. }));
            Ok(Response::error("not_found", "admission not yet visible"))
        }),
        CallOutcome::Waiting
    ));
    assert!(matches!(
        call.poll(&mut |command| {
            assert!(matches!(command, Command::LookupRequest { .. }));
            Ok(Response::Accepted { id: 9 })
        }),
        CallOutcome::Waiting
    ));
    assert!(matches!(
        call.poll(&mut |command| {
            assert!(matches!(command, Command::TakeResult { request: 9 }));
            Ok(Response::error(
                "deadline_exceeded",
                "unknown provider state",
            ))
        }),
        CallOutcome::Waiting
    ));
    let mut calls = 0;
    assert!(matches!(
        call.poll(&mut |command| {
            calls += 1;
            match command {
                Command::TakeResult { request: 9 } => Ok(Response::Result {
                    payload: serde_json::json!({"projection_id":"owned"}),
                }),
                Command::ReleaseResult { request: 9 } => Ok(Response::Ok),
                _ => panic!("must not cancel or resubmit"),
            }
        }),
        CallOutcome::Result(_)
    ));
    assert_eq!(calls, 2);
}

#[test]
fn lookup_crossing_retention_horizon_does_not_adopt_a_reused_request_id() {
    let start = Instant::now();
    let mut call = BrokerCall {
        command: Command::Request {
            service: "example/1".into(),
            payload: serde_json::json!({}),
            operation_id: "operation".into(),
            timeout_ms: 1000,
            parent: None,
        },
        request: None,
        cancelling: true,
        terminal: None,
        first_sent: Some(start),
        admission_uncertain: true,
    };
    let mut times = [
        start + Duration::from_secs(299),
        start + Duration::from_secs(300),
    ]
    .into_iter();
    assert!(matches!(
        call.poll_with_clock(
            &mut |command| {
                assert!(matches!(command, Command::LookupRequest { .. }));
                Ok(Response::Accepted { id: 9 })
            },
            &mut || times.next().unwrap()
        ),
        CallOutcome::Waiting
    ));
    assert!(call.request.is_none());
}
use crate::wire::{Request as HttpRequest, Router};
use session::{Application, Presentation};
use std::collections::BTreeSet;

mod broker_tests;

struct Provider {
    title: String,
    next: u64,
    requests: BTreeMap<u64, (String, Value)>,
    calls: Vec<(String, Value)>,
    cancelled: BTreeSet<u64>,
    hold_dispatch: bool,
    hold_begin_release: bool,
    observation_failure: bool,
    timeouts: Vec<(String, u64)>,
}

impl Provider {
    fn new(title: &str) -> Self {
        Self {
            title: title.into(),
            next: 0,
            requests: BTreeMap::new(),
            calls: Vec::new(),
            cancelled: BTreeSet::new(),
            hold_dispatch: false,
            hold_begin_release: false,
            observation_failure: false,
            timeouts: Vec::new(),
        }
    }

    fn send(&mut self, command: Command) -> ModuleResult<Response> {
        Ok(match command {
            Command::CallApi {
                service,
                method,
                interface_major,
                interface_minor,
                payload,
                timeout_ms,
                ..
            } => {
                assert_eq!(service, session::SERVICE);
                assert_eq!((interface_major, interface_minor), (1, 0));
                self.timeouts.push((method.clone(), timeout_ms));
                self.admit(method, payload)
            }
            Command::Request {
                service, payload, ..
            } => {
                let method = payload["action"].as_str().unwrap().to_owned();
                assert!(matches!(
                    service.as_str(),
                    LIBRARY_SERVICE | super::super::PAIRING_SERVICE
                ));
                self.admit(method, payload)
            }
            Command::TakeResult { request } => {
                if self.cancelled.contains(&request) {
                    return Ok(Response::Error {
                        code: "cancelled".into(),
                        message: String::new(),
                    });
                }
                let (method, payload) = self.requests.get(&request).unwrap().clone();
                if self.hold_dispatch && method == "dispatch" {
                    return Ok(Response::Pending);
                }
                Response::Result {
                    payload: self.result(&method, &payload),
                }
            }
            Command::Cancel { request } => {
                self.cancelled.insert(request);
                Response::Ok
            }
            Command::ReleaseResult { request } => {
                if self.hold_begin_release
                    && self
                        .requests
                        .get(&request)
                        .is_some_and(|(method, _)| method == "begin_project")
                {
                    return Err("lost release acknowledgement".into());
                }
                Response::Ok
            }
            other => panic!("unexpected presentation broker command: {other:?}"),
        })
    }

    fn admit(&mut self, method: String, payload: Value) -> Response {
        self.next += 1;
        self.calls.push((method.clone(), payload.clone()));
        self.requests.insert(self.next, (method, payload));
        Response::Accepted { id: self.next }
    }

    fn result(&self, method: &str, payload: &Value) -> Value {
        match method {
            "identity" => json!({"ok":true,"host_id":"host-instance"}),
            "check_client_certificate" => {
                let pem = payload["certificate_pem"].as_str().unwrap();
                let (_, der) = pem_rfc7468::decode_vec(pem.as_bytes()).unwrap();
                json!({"ok":true,"paired":1,"client_id":"paired-client", "certificate_id":fingerprint(&der),"config_overrides":{}})
            }
            "identity_inventory" => json!({"ok":true,"revision":7,"entries":[],"next_cursor":null}),
            "recovery_work" => json!(session::Reply::RecoveryWork {
                items: vec![],
                next_cursor: None
            }),
            "unpair_presented_client" => json!({"ok":true,"unpaired":1}),
            "begin_project" => json!(session::Reply::Started {
                projection_id: "projection-1".into()
            }),
            "append_project" => json!(session::Reply::Appended {
                complete: payload["final_page"] == true
            }),
            "read_project" => json!(session::Reply::Projected {
                presentation: Presentation {
                    busy: false,
                    current_game: None,
                    current_game_uuid: None
                },
                entries: vec![Application {
                    id: "42".into(),
                    uuid: "provider-tile".into(),
                    title: self.title.clone(),
                    aliases: vec![],
                    visible: true,
                    monitor_profile: false,
                    properties: json!({})
                }],
                next_offset: None,
            }),
            "release_project" | "cancel_dispatch" | "revoke_client" => {
                json!(session::Reply::Acknowledged {})
            }
            "info" => json!(session::Reply::Info {
                presentation: Presentation {
                    busy: false,
                    current_game: None,
                    current_game_uuid: None
                }
            }),
            "dispatch" => json!(session::Reply::Dispatched {
                decision: Decision::Complete {
                    status_code: 410,
                    message: format!("{} completed", self.title)
                }
            }),
            "artwork" => json!(session::Reply::Artwork {
                artwork: Artwork::InlinePng {
                    bytes: b"\x89PNG\r\n\x1a\nexample".to_vec()
                }
            }),
            "observe" if self.observation_failure => json!({"unexpected":"not an acknowledgment"}),
            "observe" => json!(session::Reply::Acknowledged {}),
            other => panic!("unhandled presentation method {other}"),
        }
    }
}

#[test]
fn revoked_catalog_releases_projection_with_lost_release_acknowledgement() {
    let mut router = router();
    let mut provider = Provider::new("policy");
    provider.hold_begin_release = true;
    let now = Instant::now();
    assert!(dispatch(&mut router, "/applist", peer(), now).is_none());
    for _ in 0..40 {
        poll(&mut router, &mut provider, now);
        if provider
            .calls
            .iter()
            .any(|(method, _)| method == "begin_project")
        {
            break;
        }
    }
    assert!(
        provider
            .calls
            .iter()
            .any(|(method, _)| method == "begin_project")
    );
    // Observe the provider result while release acknowledgement remains lost.
    poll(&mut router, &mut provider, now);
    provider.hold_begin_release = false;
    for _ in 0..20 {
        router.poll_with_identity(
            now,
            &mut |command| provider.send(command),
            |_, permission| permission != Permission::List,
        );
        if provider
            .calls
            .iter()
            .any(|(method, _)| method == "release_project")
        {
            break;
        }
    }
    assert!(
        provider
            .calls
            .iter()
            .any(|(method, payload)| method == "release_project"
                && payload["projection_id"] == "projection-1")
    );
}

fn router() -> Router {
    let mut router = Router::new(
        RouterConfig {
            hostname: "test".into(),
            http_port: 1,
            https_port: 2,
        },
        Controller::new(None).unwrap(),
    );
    router.presentation = Some(PresentationRouter::new().unwrap());
    router
}

fn peer() -> RequestContext {
    RequestContext {
        secure: true,
        remote: "127.0.0.1".parse().unwrap(),
        peer_certificate: Some(vec![1, 2, 3]),
        peer_trusted: true,
        list_allowed: true,
        launch_allowed: true,
        view_allowed: true,
        terminate_allowed: true,
        entitlement_allowed: false,
    }
}

fn dispatch(
    router: &mut Router,
    target: &str,
    context: RequestContext,
    now: Instant,
) -> Option<Reply> {
    router.dispatch_with(
        1,
        HttpRequest {
            method: "GET".into(),
            target: target.into(),
            body: vec![],
        },
        context,
        now,
        &mut |_| panic!("dispatch performs no broker effects"),
    )
}

fn poll(router: &mut Router, provider: &mut Provider, now: Instant) -> Vec<(u64, Reply)> {
    router.poll_with_identity(now, &mut |command| provider.send(command), |_, _| true)
}

fn complete(router: &mut Router, provider: &mut Provider, now: Instant) -> Reply {
    for step in 0..100 {
        let mut replies = poll(router, provider, now + Duration::from_millis(step * 10));
        if !replies.is_empty() {
            return replies.remove(0).1;
        }
    }
    panic!("presentation did not complete")
}

#[test]
fn unchanged_wire_consumer_uses_each_selected_policy_implementation() {
    for title in ["First provider tile", "Replacement provider tile"] {
        let mut router = router();
        let mut provider = Provider::new(title);
        let now = Instant::now();
        assert!(dispatch(&mut router, "/applist?uniqueid=forged-owner", peer(), now).is_none());
        let reply = complete(&mut router, &mut provider, now);
        assert_eq!(reply.status, 200);
        let body = String::from_utf8(reply.body).unwrap();
        assert!(body.contains(title));
        assert!(body.contains("<ID>42</ID>"));
        assert!(matches!(reply.permission, Some(Permission::List)));
        assert_eq!(provider.calls.last().unwrap().0, "release_project");
        let begin = &provider
            .calls
            .iter()
            .find(|call| call.0 == "begin_project")
            .unwrap()
            .1;
        assert_eq!(begin["context"]["client"]["id"], fingerprint(&[1, 2, 3]));
        assert_ne!(begin["context"]["client"]["id"], "forged-owner");
        assert!(router.presentation.as_ref().unwrap().jobs.is_empty());
    }
}

#[test]
fn list_only_clients_receive_catalog_and_artwork_without_stream_view_authority() {
    for target in ["/applist", "/appasset?appid=42"] {
        let mut router = router();
        let mut provider = Provider::new("List-only tile");
        let now = Instant::now();
        let context = RequestContext {
            list_allowed: true,
            view_allowed: false,
            launch_allowed: false,
            terminate_allowed: false,
            ..peer()
        };
        assert!(dispatch(&mut router, target, context, now).is_none());
        let reply = complete(&mut router, &mut provider, now);
        assert_eq!(reply.status, 200);
        assert_eq!(reply.permission, Some(Permission::List));
        let begin = &provider
            .calls
            .iter()
            .find(|(method, _)| method == "begin_project")
            .unwrap()
            .1;
        assert_eq!(begin["context"]["client"]["may_list"], true);
        assert_eq!(begin["context"]["client"]["may_view"], false);
        if target == "/applist" {
            assert!(String::from_utf8_lossy(&reply.body).contains("List-only tile"));
        } else {
            assert_eq!(reply.content_type, "image/png");
        }
    }
}

#[test]
fn clients_without_effective_list_authority_cannot_project_or_read_artwork() {
    for target in ["/applist", "/appasset?appid=42"] {
        let mut router = router();
        let now = Instant::now();
        let context = RequestContext {
            list_allowed: false,
            view_allowed: false,
            launch_allowed: false,
            terminate_allowed: false,
            ..peer()
        };
        assert_eq!(
            dispatch(&mut router, target, context, now).unwrap().status,
            403
        );
        assert!(router.presentation.as_ref().unwrap().jobs.is_empty());
        assert!(router.jobs.is_empty());
    }
}

#[test]
fn generic_view_only_cached_resume_refreshes_identities_without_reading_a_user_app_list() {
    let mut router = router();
    let mut provider = Provider::new("resume");
    let now = Instant::now();
    let context = RequestContext {
        list_allowed: false,
        view_allowed: true,
        launch_allowed: false,
        terminate_allowed: false,
        ..peer()
    };
    let target = format!(
        "/resume?appid=42&mode=1920x1080x60&rikey={}&rikeyid=7",
        "11".repeat(16)
    );
    assert!(dispatch(&mut router, &target, context, now).is_none());
    let reply = complete(&mut router, &mut provider, now);
    assert_eq!(reply.status, 200);
    let methods: Vec<_> = provider
        .calls
        .iter()
        .map(|(method, _)| method.as_str())
        .collect();
    for expected in [
        "identity_inventory",
        "begin_project",
        "append_project",
        "release_project",
        "dispatch",
    ] {
        assert!(methods.contains(&expected));
    }
    assert!(!methods.contains(&"read_project"));
    let request = &provider
        .calls
        .iter()
        .find(|(method, _)| method == "dispatch")
        .unwrap()
        .1;
    assert_eq!(request["intent"], "resume");
    assert_eq!(request["context"]["client"]["may_list"], false);
    assert_eq!(request["context"]["client"]["may_view"], true);
}

#[test]
fn paired_server_info_uses_the_policy_projection_and_retains_transport_authorization() {
    let mut router = router();
    let mut provider = Provider::new("policy");
    let now = Instant::now();
    assert!(dispatch(&mut router, "/serverinfo", peer(), now).is_none());
    let reply = complete(&mut router, &mut provider, now);
    let body = String::from_utf8(reply.body).unwrap();
    assert!(body.contains("<currentgame>0</currentgame>"));
    assert!(body.contains("AMATERASU_SERVER_FREE"));
    assert!(reply.requires_paired_tls);
    assert!(provider.calls.iter().any(|call| call.0 == "info"));
}

#[test]
fn unpaired_server_info_does_not_query_a_policy_with_authenticated_context() {
    let mut router = router();
    let mut provider = Provider::new("policy");
    let now = Instant::now();
    let context = RequestContext {
        secure: false,
        peer_trusted: false,
        peer_certificate: None,
        ..peer()
    };
    assert!(dispatch(&mut router, "/serverinfo", context, now).is_none());
    let reply = complete(&mut router, &mut provider, now);
    assert!(
        String::from_utf8(reply.body)
            .unwrap()
            .contains("<currentgame>0</currentgame>")
    );
    assert!(!provider.calls.iter().any(|call| call.0 == "info"));
}

#[test]
fn synthetic_launch_controls_can_use_termination_without_launch_permission() {
    let mut router = router();
    let mut provider = Provider::new("control");
    let now = Instant::now();
    let context = RequestContext {
        launch_allowed: false,
        ..peer()
    };
    let target = format!(
        "/launch?appid=42&mode=1920x1080x60&rikey={}&rikeyid=7",
        "11".repeat(16)
    );
    assert!(dispatch(&mut router, &target, context, now).is_none());
    let reply = complete(&mut router, &mut provider, now);
    assert_eq!(reply.status, 200);
    assert!(String::from_utf8_lossy(&reply.body).contains("status_code=\"410\""));
    assert!(
        String::from_utf8(reply.body)
            .unwrap()
            .contains("control completed")
    );
    assert!(matches!(reply.permission, Some(Permission::Paired)));
    let request = &provider
        .calls
        .iter()
        .find(|call| call.0 == "dispatch")
        .unwrap()
        .1;
    assert_eq!(request["context"]["client"]["may_launch"], false);
    assert_eq!(request["context"]["client"]["may_terminate"], true);
}

#[test]
fn cached_tile_actions_refresh_complete_inventory_before_dispatch() {
    let mut router = router();
    let mut provider = Provider::new("control");
    let now = Instant::now();
    let target = format!(
        "/launch?appid=42&mode=1920x1080x60&rikey={}&rikeyid=7",
        "11".repeat(16)
    );
    assert!(dispatch(&mut router, &target, peer(), now).is_none());
    let reply = complete(&mut router, &mut provider, now);
    assert_eq!(reply.status, 200);
    assert!(String::from_utf8_lossy(&reply.body).contains("status_code=\"410\""));
    let names: Vec<_> = provider.calls.iter().map(|call| call.0.as_str()).collect();
    let inventory = names
        .iter()
        .position(|name| *name == "identity_inventory")
        .unwrap();
    let release = names
        .iter()
        .position(|name| *name == "release_project")
        .unwrap();
    let decision = names.iter().position(|name| *name == "dispatch").unwrap();
    assert!(inventory < release && release < decision);
}

#[test]
fn revoked_grants_cancel_a_pending_decision_before_any_consumer_effect() {
    let mut router = router();
    let mut provider = Provider::new("control");
    let now = Instant::now();
    provider.hold_dispatch = true;
    let target = format!(
        "/launch?appid=42&mode=1920x1080x60&rikey={}&rikeyid=7",
        "11".repeat(16)
    );
    assert!(dispatch(&mut router, &target, peer(), now).is_none());
    for _ in 0..40 {
        poll(&mut router, &mut provider, now);
        if provider.calls.iter().any(|call| call.0 == "dispatch") {
            break;
        }
    }
    assert!(provider.calls.iter().any(|call| call.0 == "dispatch"));
    let mut observed_replies = Vec::new();
    for _ in 0..8 {
        let replies = router.poll_with_identity(
            now,
            &mut |command| provider.send(command),
            |_, permission| permission != Permission::Terminate,
        );
        assert!(!replies.iter().any(|(_, reply)| {
            String::from_utf8_lossy(&reply.body).contains("status_code=\"410\"")
        }));
        observed_replies.extend(replies);
    }
    assert!(observed_replies.is_empty());
    assert!(
        !provider
            .calls
            .iter()
            .any(|call| call.0 == "cancel_dispatch")
    );
    provider.hold_dispatch = false;
    for _ in 0..8 {
        observed_replies.extend(router.poll_with_identity(
            now,
            &mut |command| provider.send(command),
            |_, permission| permission != Permission::Terminate,
        ));
    }
    assert!(
        observed_replies
            .iter()
            .any(|(_, reply)| reply.status == 403)
    );
    assert!(
        provider
            .calls
            .iter()
            .any(|call| call.0 == "cancel_dispatch")
    );
    assert!(!router.launches.busy());
}

#[test]
fn inline_artwork_uses_the_selected_policy_and_list_permission() {
    let mut router = router();
    let mut provider = Provider::new("artwork");
    let now = Instant::now();
    assert!(dispatch(&mut router, "/appasset?appid=42", peer(), now).is_none());
    let reply = complete(&mut router, &mut provider, now);
    assert_eq!(reply.content_type, "image/png");
    assert!(reply.body.starts_with(b"\x89PNG\r\n\x1a\n"));
    assert!(reply.requires_paired_tls);
    assert!(matches!(reply.permission, Some(Permission::List)));
}

#[test]
fn malformed_cleanup_acknowledgment_keeps_the_resource_obligation() {
    let mut router = router();
    let mut provider = Provider::new("cleanup");
    let now = Instant::now();
    provider.observation_failure = true;
    let presentation = router.presentation.as_mut().unwrap();
    presentation.pending_observations.push_back(Observation {
        client_id: fingerprint(&[1, 2, 3]),
        session_id: "retained-session".into(),
        generation: 1,
        reservation_id: "admission".into(),
        transport_id: "retired-transport".into(),
        transport_epoch: Some("runtime-epoch".into()),
        event: SessionEvent::StreamEnded,
    });
    for step in 0..8 {
        assert!(
            poll(
                &mut router,
                &mut provider,
                now + Duration::from_millis(step * 100)
            )
            .is_empty()
        );
    }
    assert!(!router.presentation.as_ref().unwrap().jobs.is_empty());
    provider.observation_failure = false;
    for step in 8..14 {
        poll(
            &mut router,
            &mut provider,
            now + Duration::from_millis(step * 100),
        );
    }
    assert!(router.presentation.as_ref().unwrap().jobs.is_empty());
}

#[test]
fn revoked_or_unpaired_clients_never_query_catalog_or_presentation() {
    for context in [
        RequestContext {
            peer_trusted: false,
            ..peer()
        },
        RequestContext {
            list_allowed: false,
            ..peer()
        },
    ] {
        let mut router = router();
        let now = Instant::now();
        let reply = dispatch(&mut router, "/applist", context, now).unwrap();
        assert!(matches!(reply.status, 401 | 403));
        assert!(router.presentation.as_ref().unwrap().jobs.is_empty());
        assert!(router.jobs.is_empty());
    }
}

#[test]
fn expired_dispatch_releases_with_a_fresh_cleanup_budget_and_answers_the_open_request() {
    let mut router = router();
    let mut provider = Provider::new("control");
    let now = Instant::now();
    provider.hold_dispatch = true;
    let target = format!(
        "/launch?appid=42&mode=1920x1080x60&rikey={}&rikeyid=7",
        "11".repeat(16)
    );
    assert!(dispatch(&mut router, &target, peer(), now).is_none());
    for _ in 0..40 {
        poll(&mut router, &mut provider, now);
        if provider.calls.iter().any(|call| call.0 == "dispatch") {
            break;
        }
    }
    let expired = now + WORK_TIMEOUT + Duration::from_secs(1);
    for _ in 0..4 {
        assert!(poll(&mut router, &mut provider, expired).is_empty());
    }
    assert!(
        !provider
            .calls
            .iter()
            .any(|call| call.0 == "cancel_dispatch")
    );
    provider.hold_dispatch = false;
    let reply = complete(&mut router, &mut provider, expired);
    assert_eq!(reply.status, 504);
    assert!(
        provider
            .timeouts
            .iter()
            .any(|(method, timeout)| method == "cancel_dispatch" && *timeout >= 1000)
    );
}

#[test]
fn committed_unpair_uses_client_scoped_revocation_without_a_fabricated_session() {
    let mut router = router();
    let mut provider = Provider::new("policy");
    let now = Instant::now();
    assert!(dispatch(&mut router, "/unpair?uniqueid=forged", peer(), now).is_none());
    let reply = complete(&mut router, &mut provider, now);
    assert!(
        String::from_utf8(reply.body)
            .unwrap()
            .contains("<unpaired>1</unpaired>")
    );
    for _ in 0..8 {
        poll(&mut router, &mut provider, now);
    }
    let revoke = &provider
        .calls
        .iter()
        .find(|call| call.0 == "revoke_client")
        .unwrap()
        .1;
    assert_eq!(revoke["client_id"], fingerprint(&[1, 2, 3]));
    assert!(revoke.get("session_id").is_none());
    assert!(router.presentation.as_ref().unwrap().jobs.is_empty());
}

#[test]
fn accepted_disconnect_keeps_its_cleanup_obligation_after_the_http_socket_closes() {
    let mut router = router();
    let mut provider = Provider::new("policy");
    let now = Instant::now();
    let client = PresentationRouter::client(&peer()).unwrap();
    let context = Context { client, game: None };
    let presentation = router.presentation.as_mut().unwrap();
    let id = presentation
        .queue(
            Some(1),
            vec![1, 2, 3],
            context,
            Stage::AwaitStop {
                target: StopTarget::Auxiliary {
                    session_id: "retained-monitor".into(),
                    generation: 1,
                },
                completion: Completion::Control(ControlCompletion {
                    status_code: 410,
                    message: "Monitor disconnected".into(),
                }),
            },
            Some(DispatchIntent::Launch),
            now,
        )
        .unwrap();
    presentation.jobs.get_mut(&id).unwrap().dispatch_operation =
        Some("disconnect-operation".into());
    presentation.cancel(1);
    for _ in 0..8 {
        assert!(poll(&mut router, &mut provider, now).is_empty());
    }
    let observe = &provider
        .calls
        .iter()
        .find(|call| call.0 == "observe")
        .unwrap()
        .1;
    assert_eq!(observe["event"], "explicit_disconnect");
    assert!(
        !provider
            .calls
            .iter()
            .any(|call| call.0 == "cancel_dispatch")
    );
    assert!(router.presentation.as_ref().unwrap().jobs.is_empty());
}

#[test]
fn expired_disconnect_still_retires_the_monitor_before_answering_a_timeout() {
    let mut router = router();
    let mut provider = Provider::new("policy");
    let now = Instant::now();
    let context = Context {
        client: PresentationRouter::client(&peer()).unwrap(),
        game: None,
    };
    let presentation = router.presentation.as_mut().unwrap();
    let id = presentation
        .queue(
            Some(1),
            vec![1, 2, 3],
            context,
            Stage::AwaitStop {
                target: StopTarget::Auxiliary {
                    session_id: "retained-monitor".into(),
                    generation: 1,
                },
                completion: Completion::Control(ControlCompletion {
                    status_code: 410,
                    message: "Monitor disconnected".into(),
                }),
            },
            Some(DispatchIntent::Launch),
            now,
        )
        .unwrap();
    presentation.jobs.get_mut(&id).unwrap().dispatch_operation =
        Some("disconnect-operation".into());
    let reply = complete(
        &mut router,
        &mut provider,
        now + WORK_TIMEOUT + Duration::from_secs(1),
    );
    assert_eq!(reply.status, 504);
    assert!(
        provider
            .calls
            .iter()
            .any(|(method, payload)| method == "observe"
                && payload["event"] == "explicit_disconnect")
    );
}

fn stream_request() -> LaunchRequest {
    super::super::streaming::launch_request(
        &super::super::Query::parse(&format!(
            "/launch?appid=42&mode=1920x1080x60&rikey={}&rikeyid=7",
            "11".repeat(16)
        ))
        .unwrap(),
    )
    .unwrap()
}

fn paired_identity() -> PeerIdentity {
    PeerIdentity::new("127.0.0.1".parse().unwrap(), vec![1, 2, 3])
        .unwrap()
        .with_paired_identity(
            "paired-client".into(),
            fingerprint(&[1, 2, 3]),
            super::super::ClientConfigOverrides::default(),
        )
        .unwrap()
}

#[test]
fn protocol_specific_replacement_validation_happens_before_game_stop_admission() {
    let mut launches = Controller::new(None).unwrap();
    let mut presentation = PresentationRouter::new().unwrap();
    let now = Instant::now();
    let mut context = Context {
        client: PresentationRouter::client(&peer()).unwrap(),
        game: Some(session::Game {
            application: Application {
                id: "1".into(),
                uuid: "game-uuid".into(),
                title: "Running game".into(),
                aliases: vec![],
                visible: true,
                monitor_profile: false,
                properties: json!({}),
            },
            owner_client_id: fingerprint(&[1, 2, 3]),
            generation: 1,
            source: None,
        }),
    };
    // Opaque IDs are valid in the SDK. Moonlight must reject an incompatible
    // replacement before invoking the execution provider for the current game.
    for request in [Some(stream_request()), None] {
        let id = presentation
            .queue(
                Some(1),
                vec![1, 2, 3],
                context.clone(),
                Stage::Info {
                    host_id: "host".into(),
                },
                Some(DispatchIntent::Launch),
                now,
            )
            .unwrap();
        let mut work = presentation.jobs.remove(&id).unwrap();
        work.dispatch_operation = Some("replacement-operation".into());
        assert!(
            presentation
                .apply_decision(
                    &mut work,
                    &mut launches,
                    paired_identity(),
                    request,
                    Decision::ReplaceApplication {
                        application_id: "opaque-game-id".into(),
                        expected_current_generation: 1,
                        source: None
                    },
                    now
                )
                .is_none()
        );
        let Stage::Release {
            completion: Some(reply),
            ..
        } = work.stage
        else {
            panic!("invalid replacement must release its reservation")
        };
        assert_eq!(reply.status, 502);
        context.game.as_mut().unwrap().generation += 1;
    }
}

#[test]
fn auxiliary_resume_uses_semantic_view_permission_even_from_an_applist_launch() {
    let now = Instant::now();
    for resume in [false, true] {
        let mut launches = Controller::new(None).unwrap();
        let mut presentation = PresentationRouter::new().unwrap();
        let context = Context {
            client: Client {
                may_launch: false,
                ..PresentationRouter::client(&peer()).unwrap()
            },
            game: None,
        };
        let id = presentation
            .queue(
                Some(1),
                vec![1, 2, 3],
                context,
                Stage::Info {
                    host_id: "host".into(),
                },
                Some(DispatchIntent::Launch),
                now,
            )
            .unwrap();
        let mut work = presentation.jobs.remove(&id).unwrap();
        work.dispatch_operation = Some("resume-operation".into());
        let plan = session::AuxiliaryStreamPlan {
            session_id: "retained-session".into(),
            generation: 1,
            reservation_id: "resume-operation".into(),
            capture: session::CaptureSource::SyntheticBlack,
            source_mode: None,
            mode: session::Mode {
                width: 1920,
                height: 1080,
                refresh_millihz: 60000,
                hdr: false,
            },
            stream_audio: false,
            host_audio: false,
        };
        let decision = if resume {
            Decision::ResumeAuxiliary { plan }
        } else {
            Decision::Auxiliary { plan }
        };
        assert!(
            presentation
                .apply_decision(
                    &mut work,
                    &mut launches,
                    paired_identity(),
                    Some(stream_request()),
                    decision,
                    now
                )
                .is_none()
        );
        let Stage::Release {
            completion: Some(reply),
            ..
        } = work.stage
        else {
            panic!("unused admission must release its reservation")
        };
        // The resumed decision reaches the explicitly unconfigured backend;
        // creation is rejected by the independent launch grant first.
        assert_eq!(reply.status, if resume { 503 } else { 403 });
    }
}
