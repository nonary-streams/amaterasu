use super::*;
use amaterasu_module_sdk::requests::{CallOutcome, OutboundCall, RequestOptions};
use amaterasu_module_sdk::{Command, Response};
use amaterasu_module_testkit::{ScriptedHost, TestHost, grants, settle};
use serde_json::{Value, json};

const MODULE: &str = "amaterasu.session";

fn session_call(operation_id: &str, payload: Value) -> OutboundCall {
    OutboundCall::request(
        RequestOptions {
            service: SERVICE,
            timeout_ms: 5_000,
            operation_id,
            parent: None,
        },
        &payload,
    )
    .unwrap()
}

#[test]
fn rejected_publication_keeps_state_and_binds_ownership_to_caller() {
    // The module holds no publish grant, so the runtime refuses its change
    // notification; the committed session must survive that.
    let host = TestHost::builder()
        .principal(MODULE, grants().provide(SERVICE))
        .principal("alice", grants().call(SERVICE))
        .principal("bob", grants().call(SERVICE))
        .build();
    let mut module = host.load::<SessionModule>(MODULE);
    let (alice, bob) = (host.client("alice"), host.client("bob"));
    let mut ask = |client, operation_id, payload| {
        let outcome = settle(client, &mut session_call(operation_id, payload), || {
            module.tick_times(1)
        });
        let CallOutcome::Completed(reply) = outcome else {
            panic!("session call {operation_id} did not complete: {outcome:?}");
        };
        reply
    };

    let created = ask(
        &alice,
        "test.create",
        json!({"action":"create","key":"secret-key"}),
    );
    assert_eq!(created["ok"], true);
    let foreign = ask(
        &bob,
        "test.foreign-snapshot",
        json!({"action":"snapshot","session_id":1}),
    );
    assert_eq!(foreign["code"], "session_not_found");
    let owned = ask(
        &alice,
        "test.snapshot",
        json!({"action":"snapshot","session_id":1}),
    );
    assert_eq!(created, owned);
    let repeated = ask(
        &alice,
        "test.create-again",
        json!({"action":"create","key":"secret-key"}),
    );
    assert_eq!(created, repeated);
}

#[test]
fn a_committed_change_is_published_once_and_reads_publish_nothing() {
    let host = TestHost::builder()
        .principal(MODULE, grants().provide(SERVICE).publish(CHANGED_TOPIC))
        .principal("alice", grants().call(SERVICE))
        .principal("observer", grants().subscribe(CHANGED_TOPIC))
        .build();
    let mut module = host.load::<SessionModule>(MODULE);
    let (alice, observer) = (host.client("alice"), host.client("observer"));
    let Response::Accepted { id: subscription } = observer
        .send(Command::Subscribe {
            topic: CHANGED_TOPIC.into(),
            capacity: 8,
            coalesce: false,
        })
        .unwrap()
    else {
        panic!("subscription was not accepted");
    };
    let next_event = || observer.send(Command::NextEvent { subscription }).unwrap();

    for (operation_id, payload) in [
        ("test.create", json!({"action":"create","key":"key"})),
        ("test.snapshot", json!({"action":"snapshot","session_id":1})),
        ("test.create-again", json!({"action":"create","key":"key"})),
    ] {
        let mut call = session_call(operation_id, payload);
        settle(&alice, &mut call, || module.tick_times(1));
    }
    let Response::Event {
        publisher, payload, ..
    } = next_event()
    else {
        panic!("the committed change was not published");
    };
    assert_eq!(
        (publisher.as_str(), payload),
        (MODULE, json!({"revision":1}))
    );
    assert!(matches!(next_event(), Response::Pending));
}

/// A layer in front of the session service: it declines one caller and
/// passes every other call on unchanged. Two modules cannot export their
/// lifecycle from one test binary, so this stands in for an installed plugin.
struct Guard {
    runtime: RuntimeClient,
    relay: amaterasu_module_sdk::requests::Relay,
}

impl ManagedModule for Guard {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        runtime.wrap(SERVICE)?;
        Ok(Self {
            runtime,
            relay: Default::default(),
        })
    }

    fn tick(&mut self) -> ModuleResult<()> {
        while let Some(request) = self.runtime.next_request()? {
            if request.caller == "mallory" {
                request.refuse(&self.runtime, "permission_denied", "not admitted")?;
            } else {
                self.relay.forward(request)?;
            }
        }
        self.relay.poll(&self.runtime, |_request, reply| reply)?;
        Ok(())
    }
}

#[test]
fn a_layer_stacked_on_the_service_leaves_ownership_with_each_caller() {
    use amaterasu_module_sdk::events::{Backlog, Delivery, Subscription};
    use amaterasu_module_sdk::{ErrorCode, ErrorOrigin};

    const GUARD: &str = "example.guard";
    // The same module and the same callers as above. Policy alone puts a
    // layer in front of the service.
    let host = TestHost::builder()
        .principal(MODULE, grants().provide(SERVICE).publish(CHANGED_TOPIC))
        .principal(GUARD, grants().call(SERVICE))
        .principal("alice", grants().call(SERVICE))
        .principal("bob", grants().call(SERVICE))
        .principal("mallory", grants().call(SERVICE))
        .principal("observer", grants().subscribe(CHANGED_TOPIC))
        .layers(SERVICE, &[GUARD])
        .build();
    let mut module = host.load::<SessionModule>(MODULE);
    let mut guard = host.load::<Guard>(GUARD);
    let observer = host.client("observer");
    let mut changes =
        Subscription::open(&observer, &SESSION_CHANGED, Backlog::Complete(8)).unwrap();
    let mut ask = |principal: &str, operation_id, payload| {
        settle(
            &host.client(principal),
            &mut session_call(operation_id, payload),
            || {
                guard.tick_times(1);
                module.tick_times(1);
            },
        )
    };

    let CallOutcome::Completed(created) = ask(
        "alice",
        "test.create",
        json!({"action":"create","key":"secret-key"}),
    ) else {
        panic!("the admitted caller's session was not created");
    };
    assert_eq!(created["ok"], true);
    // The session belongs to alice, not to the guard that forwarded her call.
    let CallOutcome::Completed(owned) = ask(
        "alice",
        "test.snapshot",
        json!({"action":"snapshot","session_id":1}),
    ) else {
        panic!("the owner could not read her session");
    };
    assert_eq!(created, owned);
    let CallOutcome::Completed(foreign) = ask(
        "bob",
        "test.foreign-snapshot",
        json!({"action":"snapshot","session_id":1}),
    ) else {
        panic!("another admitted caller got no reply");
    };
    assert_eq!(foreign["code"], "session_not_found");

    let CallOutcome::Refused(refusal) = ask(
        "mallory",
        "test.excluded",
        json!({"action":"create","key":"other-key"}),
    ) else {
        panic!("the excluded caller was not declined");
    };
    assert_eq!(refusal.code, ErrorCode::PermissionDenied);
    assert_eq!(refusal.origin, ErrorOrigin::Provider);

    // Only alice's create changed anything, and it was announced once.
    assert!(matches!(
        changes.next(&observer).unwrap(),
        Some(Delivery::Event(event)) if event.publisher == MODULE && event.payload.revision == 1
    ));
    assert_eq!(changes.next(&observer).unwrap(), None);
}

#[derive(Default)]
struct Scripted {
    delivery: Option<Response>,
    completion_error: Option<&'static str>,
    completions: usize,
}

fn scripted_module(completion_error: &'static str) -> (ScriptedHost<Scripted>, SessionModule) {
    let host = ScriptedHost::new(
        Scripted {
            delivery: Some(Response::Request {
                id: 1,
                caller: "alice".into(),
                initiator: "alice".into(),
                service: SERVICE.into(),
                payload: json!({"action":"create","key":"retry"}),
                remaining_ms: 1,
            }),
            completion_error: Some(completion_error),
            completions: 0,
        },
        |scripted, command| match command {
            Command::Register { .. } => Response::Ok,
            Command::Poll {} => scripted.delivery.take().unwrap_or(Response::Pending),
            Command::Publish { .. } => Response::Ok,
            Command::Complete { .. } => {
                scripted.completions += 1;
                match scripted.completion_error {
                    Some(code) => Response::error(code, "deliberate completion failure"),
                    None => Response::Ok,
                }
            }
            _ => Response::error("unexpected", "test endpoint command not supported"),
        },
    );
    let module = SessionModule::start(host.client()).unwrap();
    (host, module)
}

#[test]
fn ended_requests_keep_committed_state_but_unexpected_completion_errors_surface() {
    for code in ["request_ended", "not_found", "permission_denied"] {
        let (host, mut module) = scripted_module(code);
        assert_eq!(module.tick().is_ok(), code != "permission_denied");
        assert_eq!(host.state().completions, 1);
        assert_eq!(module.coordinator.revision(), 1);
        assert_eq!(
            module
                .coordinator
                .handle("alice", json!({"action":"snapshot","session_id":1}))["ok"],
            true
        );
    }
}
