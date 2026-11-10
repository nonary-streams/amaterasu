use super::*;
use amaterasu_contracts::HookSpec;

const HOOK: &str = "example.launch.environment/1";

/// An owner and two participants, `first` before `second`. Each participant
/// that is named in `registered` has joined the hook.
fn hook_broker(registered: &[&str]) -> Broker {
    let mut config = RuntimeConfig::default();
    config.principals.insert("owner".into(), Grants::default());
    config
        .principals
        .insert("outsider".into(), Grants::default());
    for participant in ["first", "second"] {
        config.principals.insert(
            participant.into(),
            Grants {
                hook: names(&[HOOK]),
                ..Default::default()
            },
        );
    }
    config.hooks.insert(
        HOOK.into(),
        HookSpec {
            owner: "owner".into(),
            participants: vec!["first".into(), "second".into()],
            timeout_ms: 1_000,
        },
    );
    let mut broker = Broker::new(config).unwrap();
    for participant in registered {
        assert!(matches!(
            broker.dispatch(participant, Command::RegisterHook { hook: HOOK.into() }),
            Response::Ok
        ));
    }
    broker
}

fn run(operation: &str, payload: Value, cleanup: bool, chain: bool) -> Command {
    Command::RunHookOperation {
        hook: HOOK.into(),
        payload,
        operation_id: operation.into(),
        cleanup,
        chain,
    }
}

fn advance(broker: &mut Broker, invocation: u64, now: Instant) -> Response {
    broker.dispatch_at("owner", Command::HookResult { invocation }, now)
}

/// Runs the owner's side until `participant` is offered its request.
fn offered_to(broker: &mut Broker, invocation: u64, participant: &str, now: Instant) -> Offered {
    assert!(matches!(
        advance(broker, invocation, now),
        Response::Pending
    ));
    poll(broker, participant, now).expect("the participant is offered the hook request")
}

#[test]
fn a_chained_hook_hands_each_result_to_the_next_participant() {
    let mut broker = hook_broker(&["first", "second"]);
    let now = Instant::now();
    let invocation = accepted(broker.dispatch_at(
        "owner",
        run("launch.1", json!({ "path": ["base"] }), false, true),
        now,
    ));

    let first = offered_to(&mut broker, invocation, "first", now);
    assert_eq!(first.caller, "owner");
    assert_eq!(first.payload, json!({ "path": ["base"] }));
    complete(
        &mut broker,
        "first",
        first.id,
        json!({ "path": ["base", "one"] }),
    );

    let second = offered_to(&mut broker, invocation, "second", now);
    assert_eq!(second.payload, json!({ "path": ["base", "one"] }));
    complete(
        &mut broker,
        "second",
        second.id,
        json!({ "path": ["base", "one", "two"] }),
    );

    match advance(&mut broker, invocation, now) {
        Response::HookResult { proposals } => {
            let trail: Vec<_> = proposals
                .iter()
                .map(|proposal| (proposal.participant.as_str(), &proposal.payload))
                .collect();
            assert_eq!(
                trail,
                [
                    ("first", &json!({ "path": ["base", "one"] })),
                    ("second", &json!({ "path": ["base", "one", "two"] })),
                ]
            );
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn an_unchained_hook_gives_every_participant_the_owners_input() {
    let mut broker = hook_broker(&["first", "second"]);
    let now = Instant::now();
    let invocation = accepted(broker.dispatch_at(
        "owner",
        run("launch.1", json!({ "path": ["base"] }), false, false),
        now,
    ));
    let first = offered_to(&mut broker, invocation, "first", now);
    complete(
        &mut broker,
        "first",
        first.id,
        json!({ "path": ["changed"] }),
    );
    let second = offered_to(&mut broker, invocation, "second", now);
    assert_eq!(second.payload, json!({ "path": ["base"] }));
}

#[test]
fn a_best_effort_chain_passes_the_value_over_a_missing_participant() {
    // `first` never joined.
    let mut broker = hook_broker(&["second"]);
    let now = Instant::now();
    let invocation = accepted(broker.dispatch_at(
        "owner",
        run("launch.1", json!({ "path": ["base"] }), true, true),
        now,
    ));

    let second = offered_to(&mut broker, invocation, "second", now);
    assert_eq!(second.payload, json!({ "path": ["base"] }));
    complete(
        &mut broker,
        "second",
        second.id,
        json!({ "path": ["base", "two"] }),
    );

    match advance(&mut broker, invocation, now) {
        Response::HookCleanupResult { outcomes } => {
            assert_eq!(outcomes[0].participant, "first");
            assert_eq!(error_code(*outcomes[0].result.clone()), "unavailable");
            assert_eq!(outcomes[1].participant, "second");
            assert!(matches!(
                &*outcomes[1].result,
                Response::Result { payload } if *payload == json!({ "path": ["base", "two"] })
            ));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_chained_value_too_large_to_deliver_fails_its_participant() {
    let oversized = json!({ "path": "x".repeat(20_000) });

    // Required: the invocation ends at the participant that produced it.
    let mut broker = hook_broker(&["first", "second"]);
    let now = Instant::now();
    let invocation =
        accepted(broker.dispatch_at("owner", run("gate.1", json!({}), false, true), now));
    let first = offered_to(&mut broker, invocation, "first", now);
    assert!(matches!(
        complete(&mut broker, "first", first.id, oversized.clone()),
        Response::Ok
    ));
    assert_eq!(
        error_code(advance(&mut broker, invocation, now)),
        "invalid_request"
    );
    assert!(poll(&mut broker, "second", now).is_none());

    // Best effort: the next participant still receives the last good value.
    let mut broker = hook_broker(&["first", "second"]);
    let invocation = accepted(broker.dispatch_at(
        "owner",
        run("effort.1", json!({ "path": "base" }), true, true),
        now,
    ));
    let first = offered_to(&mut broker, invocation, "first", now);
    complete(&mut broker, "first", first.id, oversized);
    let second = offered_to(&mut broker, invocation, "second", now);
    assert_eq!(second.payload, json!({ "path": "base" }));
}

#[test]
fn a_participants_refusal_stops_a_required_hook() {
    let mut broker = hook_broker(&["first", "second"]);
    let now = Instant::now();
    let invocation =
        accepted(broker.dispatch_at("owner", run("gate.1", json!({}), false, false), now));
    let first = offered_to(&mut broker, invocation, "first", now);
    assert!(matches!(
        broker.dispatch(
            "first",
            Command::Refuse {
                request: first.id,
                code: "stream_active".into(),
                message: "a stream is running".into(),
            },
        ),
        Response::Ok
    ));
    match advance(&mut broker, invocation, now) {
        Response::Error { code, message } => {
            assert_eq!(code, "refused.stream_active");
            // The owner can say which participant stopped the operation.
            assert_eq!(message, "first: a stream is running");
        }
        other => panic!("{other:?}"),
    }
    assert!(poll(&mut broker, "second", now).is_none());
}

#[test]
fn the_same_operation_cannot_change_between_chained_and_unchained() {
    let mut broker = hook_broker(&["first", "second"]);
    let now = Instant::now();
    let invocation =
        accepted(broker.dispatch_at("owner", run("launch.1", json!({}), false, true), now));
    assert_eq!(
        accepted(broker.dispatch_at("owner", run("launch.1", json!({}), false, true), now)),
        invocation
    );
    assert_eq!(
        error_code(broker.dispatch_at("owner", run("launch.1", json!({}), false, false), now)),
        "conflict"
    );
}

#[test]
fn forgetting_a_released_operation_lets_its_identifier_run_again() {
    let mut broker = hook_broker(&["first", "second"]);
    let now = Instant::now();
    let finish = |broker: &mut Broker, operation: &str| {
        let invocation =
            accepted(broker.dispatch_at("owner", run(operation, json!({}), false, false), now));
        for participant in ["first", "second"] {
            let offered = offered_to(broker, invocation, participant, now);
            complete(broker, participant, offered.id, json!({}));
        }
        assert!(matches!(
            advance(broker, invocation, now),
            Response::HookResult { .. }
        ));
        invocation
    };
    let release = |broker: &mut Broker, principal: &str, invocation: u64, forget: bool| {
        broker.dispatch_at(principal, Command::ReleaseHook { invocation, forget }, now)
    };

    // Without `forget` the identifier stays fenced, as before.
    let kept = finish(&mut broker, "kept.1");
    assert!(matches!(
        release(&mut broker, "owner", kept, false),
        Response::Ok
    ));
    assert_eq!(
        error_code(broker.dispatch_at("owner", run("kept.1", json!({}), false, false), now)),
        "operation_expired"
    );

    let forgotten = finish(&mut broker, "forgotten.1");
    assert_eq!(
        error_code(release(&mut broker, "outsider", forgotten, true)),
        "permission_denied"
    );
    assert!(matches!(
        release(&mut broker, "owner", forgotten, true),
        Response::Ok
    ));
    assert_eq!(
        error_code(broker.dispatch_at(
            "owner",
            Command::LookupHook {
                operation_id: "forgotten.1".into()
            },
            now
        )),
        "not_found"
    );
    let again =
        accepted(broker.dispatch_at("owner", run("forgotten.1", json!({}), false, false), now));
    assert_ne!(again, forgotten);
}

#[test]
fn a_recurring_extension_point_outlives_the_fenced_identity_budget() {
    let mut broker = hook_broker(&["first", "second"]);
    let now = Instant::now();
    // More runs than the broker retains fenced operation identities.
    for run_number in 0..600 {
        let invocation = accepted(broker.dispatch_at(
            "owner",
            run(&format!("launch.{run_number}"), json!({}), true, false),
            now,
        ));
        for participant in ["first", "second"] {
            let offered = offered_to(&mut broker, invocation, participant, now);
            complete(&mut broker, participant, offered.id, json!({}));
        }
        assert!(matches!(
            advance(&mut broker, invocation, now),
            Response::HookCleanupResult { .. }
        ));
        assert!(matches!(
            broker.dispatch_at(
                "owner",
                Command::ReleaseHook {
                    invocation,
                    forget: true
                },
                now
            ),
            Response::Ok
        ));
    }
}
