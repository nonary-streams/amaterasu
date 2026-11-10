fn retirement_cleanup_fixture(now: Instant) -> (Controller, Value, String) {
    let mut controller = Controller::new(Some(config())).unwrap();
    assert!(controller.launch(10, request(1), peer(1), now).is_none());
    let target =
        json!({"kind":"session","host_epoch":"epoch","session_id":"session","generation":1});
    let record = controller.records.values_mut().next().unwrap();
    record.lifecycle_session_target = serde_json::from_value(target.clone()).ok();
    record.lifecycle_prepare_dispatched = true;
    record.lifecycle_application_bound = true;
    record.lifecycle_retirement_launch = Some(42);
    record.launch_connection = None;
    record.request_cleanup(None, now);
    let operation = record.application_operation.clone();
    (controller, target, operation)
}

fn lifecycle_commands(broker: &Broker) -> Vec<(&str, &str)> {
    broker
        .commands
        .iter()
        .filter_map(|command| match command {
            Command::Request {
                service, payload, ..
            } if service == "amaterasu.session-lifecycle/1" => Some((
                payload["operation"].as_str().unwrap(),
                payload["operation_id"].as_str().unwrap(),
            )),
            _ => None,
        })
        .collect()
}

#[test]
fn pending_application_retirement_is_observed_before_polling_session_end() {
    let now = Instant::now();
    let (mut controller, target, application) = retirement_cleanup_fixture(now);
    let mut broker = Broker::default();
    broker.lifecycle_replies.extend([
        json!({"status":"pending","target":target,"fenced":true}),
        json!({"status":"pending","target":target}),
        json!({"status":"pending","target":target}),
        json!({"status":"succeeded","target":target}),
        json!({"status":"succeeded","target":target}),
    ]);
    // Both admitted operations settle at the same clock time. A pending
    // verification is neither retirement proof nor a reason to poll End yet.
    for _ in 0..12 {
        controller.poll(now, &mut |command| broker.send(command));
    }
    assert!(!controller.records[&1].application_retirement_verified());
    assert_eq!(lifecycle_commands(&broker).len(), 2);
    for _ in 0..8 {
        controller.poll(now + CLEANUP_POLL_INTERVAL / 2, &mut |command| {
            broker.send(command)
        });
    }
    assert_eq!(
        lifecycle_commands(&broker).len(),
        2,
        "respect the frozen poll interval"
    );
    for tick in 1..=20 {
        let at = now + CLEANUP_POLL_INTERVAL * tick;
        for _ in 0..4 {
            controller.poll(at, &mut |command| broker.send(command));
        }
        if controller.records[&1].state == State::Stopped {
            break;
        }
    }
    assert_eq!(controller.records[&1].state, State::Stopped);
    let retirement = format!("{application}-lifecycle-app-retired");
    let end = format!("{application}-lifecycle-end");
    assert_eq!(
        lifecycle_commands(&broker),
        [
            ("end_session", end.as_str()),
            ("application_retired", retirement.as_str()),
            ("lookup", retirement.as_str()),
            ("lookup", retirement.as_str()),
            ("lookup", end.as_str()),
        ]
    );
}

#[test]
fn failed_application_retirement_requires_explicit_retry_with_a_fresh_proof_operation() {
    let now = Instant::now();
    let (mut controller, target, application) = retirement_cleanup_fixture(now);
    let mut broker = Broker::default();
    broker.lifecycle_replies.extend([
        json!({"status":"pending","target":target,"fenced":true}),
        json!({"status":"pending","target":target}),
        // The coordinator preserves the operation identity, but its terminal
        // verification failure intentionally does not contain a target.
        json!({"status":"recovery_required","reason":"application_retirement_unconfirmed"}),
    ]);
    for tick in 0..20 {
        for _ in 0..4 {
            controller.poll(now + CLEANUP_POLL_INTERVAL * tick, &mut |command| {
                broker.send(command)
            });
        }
        if controller.records[&1].state == State::CleanupFailed {
            break;
        }
    }
    assert_eq!(controller.records[&1].state, State::CleanupFailed);
    assert!(!controller.records[&1].application_retirement_verified());
    let proofs_before_retry = lifecycle_commands(&broker)
        .iter()
        .filter(|(operation, _)| *operation == "application_retired")
        .count();
    for _ in 0..8 {
        controller.poll(now + Duration::from_secs(3), &mut |command| {
            broker.send(command)
        });
    }
    // The automatic pass may read the receipt again; it never asks for a
    // fresh proof on its own.
    assert_eq!(
        lifecycle_commands(&broker)
            .iter()
            .filter(|(operation, _)| *operation == "application_retired")
            .count(),
        proofs_before_retry,
        "do not automatically request a new proof"
    );
    broker.lifecycle_replies.extend([
        json!({"status":"pending","target":target}),
        json!({"status":"succeeded","target":target}),
        json!({"status":"succeeded","target":target}),
    ]);
    let retry_at = now + Duration::from_secs(4);
    assert!(controller.cancel(20, &peer(1), retry_at).is_none());
    let mut replies = Vec::new();
    for tick in 0..20 {
        for _ in 0..4 {
            replies.extend(
                controller.poll(retry_at + CLEANUP_POLL_INTERVAL * tick, &mut |command| {
                    broker.send(command)
                }),
            );
        }
        if controller.records[&1].state == State::Stopped {
            break;
        }
    }
    assert_eq!(controller.records[&1].state, State::Stopped);
    assert!(
        replies
            .iter()
            .any(|(connection, reply)| *connection == 20 && reply.status == 200)
    );
    let operations = lifecycle_commands(&broker);
    let proofs: Vec<_> = operations
        .iter()
        .filter(|(operation, _)| *operation == "application_retired")
        .collect();
    assert_eq!(proofs.len(), 2);
    assert_ne!(proofs[0].1, proofs[1].1);
    assert_eq!(
        operations
            .iter()
            .filter(|(operation, _)| *operation == "end_session")
            .count(),
        1
    );
    assert_eq!(
        operations.last(),
        Some(&("lookup", format!("{application}-lifecycle-end").as_str()))
    );
    let proof_payloads: Vec<_> = broker
        .commands
        .iter()
        .filter_map(|command| match command {
            Command::Request {
                service, payload, ..
            } if service == "amaterasu.session-lifecycle/1"
                && payload["operation"] == "application_retired" =>
            {
                Some(payload)
            }
            _ => None,
        })
        .collect();
    assert_eq!(proof_payloads[0]["target"], proof_payloads[1]["target"]);
    assert_eq!(
        proof_payloads[0]["retirement_receipt"],
        proof_payloads[1]["retirement_receipt"]
    );
}

#[test]
fn timed_out_application_retirement_lookup_retries_the_original_pending_proof() {
    let now = Instant::now();
    let (mut controller, target, application) = retirement_cleanup_fixture(now);
    let retirement = format!("{application}-lifecycle-app-retired");
    let mut broker = Broker::default();
    broker.lifecycle_replies.extend([
        json!({"status":"pending","target":target,"fenced":true}),
        json!({"status":"pending","target":target}),
    ]);
    for _ in 0..12 {
        controller.poll(now, &mut |command| broker.send(command));
    }
    let mut held = None;
    for _ in 0..12 {
        controller.poll(now + CLEANUP_POLL_INTERVAL, &mut |command| {
            if matches!(&command, Command::TakeResult { request } if held == Some(*request)) {
                broker.commands.push(command);
                return Ok(Response::Pending);
            }
            let hold = matches!(&command, Command::Request { service, payload, .. }
                if service == "amaterasu.session-lifecycle/1" && payload["operation"] == "lookup"
                    && payload["operation_id"] == retirement);
            let response = broker.send(command);
            if hold && let Ok(Response::Accepted { id }) = &response {
                held = Some(*id);
            }
            response
        });
    }
    assert!(held.is_some());
    let deadline = controller.records[&1].deadline;
    for _ in 0..4 {
        controller.poll(deadline, &mut |command| broker.send(command));
    }
    assert_eq!(controller.records[&1].state, State::CleanupFailed);
    assert!(!controller.records[&1].application_retirement_verified());
    broker.lifecycle_replies.extend([
        json!({"status":"succeeded","target":target}),
        json!({"status":"succeeded","target":target}),
    ]);
    let retry_at = deadline + Duration::from_secs(1);
    assert!(controller.cancel(20, &peer(1), retry_at).is_none());
    for tick in 0..20 {
        for _ in 0..4 {
            controller.poll(retry_at + CLEANUP_POLL_INTERVAL * tick, &mut |command| {
                broker.send(command)
            });
        }
        if controller.records[&1].state == State::Stopped {
            break;
        }
    }
    assert_eq!(controller.records[&1].state, State::Stopped);
    let operations = lifecycle_commands(&broker);
    assert_eq!(
        operations
            .iter()
            .filter(|(operation, _)| *operation == "application_retired")
            .count(),
        1
    );
    let proof_lookups: Vec<_> = operations
        .iter()
        .filter(|(operation, id)| *operation == "lookup" && *id == retirement)
        .collect();
    assert_eq!(
        proof_lookups.len(),
        2,
        "uncertainty must never allocate a new proof operation"
    );
}

#[test]
fn retirement_and_end_positive_receipts_require_exact_operation_and_target() {
    for phase in [
        workflow::Phase::ReportApplicationRetired,
        workflow::Phase::LookupApplicationRetirement,
        workflow::Phase::EndLifecycle,
        workflow::Phase::AwaitLifecycleEnd,
    ] {
        for status in ["pending", "succeeded"] {
            for mismatch in [
                "operation",
                "missing_operation",
                "session",
                "generation",
                "epoch",
                "missing_target",
            ] {
                let now = Instant::now();
                let (mut controller, target, application) = retirement_cleanup_fixture(now);
                let record = controller.records.get_mut(&1).unwrap();
                record.lifecycle_application_retirement =
                    record::ApplicationRetirementState::Pending;
                let end = matches!(
                    phase,
                    workflow::Phase::EndLifecycle | workflow::Phase::AwaitLifecycleEnd
                );
                let operation = if end {
                    format!("{application}-lifecycle-end")
                } else {
                    record.application_retirement_operation()
                };
                let mut payload =
                    json!({"status":status,"target":target,"operation_id":operation,"fenced":true});
                match mismatch {
                    "operation" => payload["operation_id"] = json!("different-operation"),
                    "missing_operation" => {
                        payload.as_object_mut().unwrap().remove("operation_id");
                    }
                    "session" => payload["target"]["session_id"] = json!("other-session"),
                    "generation" => payload["target"]["generation"] = json!(2),
                    "epoch" => payload["target"]["host_epoch"] = json!("other-epoch"),
                    "missing_target" => {
                        payload.as_object_mut().unwrap().remove("target");
                    }
                    _ => unreachable!(),
                }
                let job = workflow::Job {
                    record: 1,
                    phase,
                    call: call::Call::new("unused", json!({}), "unused".into(), 1_000),
                    deadline: now + Duration::from_secs(5),
                };
                controller.consume(&job, payload, now, &mut Vec::new());
                let record = &controller.records[&1];
                assert_eq!(
                    record.state,
                    State::CleanupFailed,
                    "{phase:?}/{status}/{mismatch}"
                );
                assert!(!record.application_retirement_verified());
                assert!(!record.lifecycle_end_complete);
                assert!(!record.lifecycle_end_fenced);
            }
        }
    }
}

#[test]
fn explicit_retirement_retry_settles_unknown_broker_ownership_before_same_id_lookup() {
    let now = Instant::now();
    let (mut controller, target, application) = retirement_cleanup_fixture(now);
    let mut broker = Broker::default();
    broker.lifecycle_replies.extend([
        json!({"status":"pending","target":target,"fenced":true}),
        json!({"status":"pending","target":target}),
        json!({"status":"succeeded","target":target}),
        json!({"status":"succeeded","target":target}),
    ]);
    for _ in 0..8 {
        controller.poll(now, &mut |command| broker.send(command));
        if controller.records[&1].lifecycle_end_fenced {
            break;
        }
    }
    broker.lifecycle_release_failure = true;
    for _ in 0..8 {
        controller.poll(now, &mut |command| broker.send(command));
        if controller.records[&1].state == State::CleanupFailed {
            break;
        }
    }
    assert_eq!(controller.records[&1].state, State::CleanupFailed);
    assert_eq!(controller.records[&1].uncertain.len(), 1);
    assert!(!controller.records[&1].application_retirement_verified());
    broker.lifecycle_release_failure = false;
    let retry_at = now + Duration::from_secs(1);
    assert!(controller.cancel(20, &peer(1), retry_at).is_none());
    for tick in 0..20 {
        for _ in 0..4 {
            controller.poll(retry_at + CLEANUP_POLL_INTERVAL * tick, &mut |command| {
                broker.send(command)
            });
        }
        if controller.records[&1].state == State::Stopped {
            break;
        }
    }
    assert_eq!(controller.records[&1].state, State::Stopped);
    assert!(controller.records[&1].uncertain.is_empty());
    let operation = format!("{application}-lifecycle-app-retired");
    let operations = lifecycle_commands(&broker);
    assert_eq!(
        operations
            .iter()
            .filter(|(action, _)| *action == "application_retired")
            .count(),
        1
    );
    assert!(operations.contains(&("lookup", operation.as_str())));
    let proof_lookup_index = broker
        .commands
        .iter()
        .position(|command| {
            matches!(command,
        Command::Request { service, payload, .. } if service == "amaterasu.session-lifecycle/1"
            && payload["operation"] == "lookup" && payload["operation_id"] == operation)
        })
        .unwrap();
    assert!(broker.commands[..proof_lookup_index].windows(2).any(|commands|
        matches!((&commands[0], &commands[1]),
            (Command::TakeResult { request: result }, Command::ReleaseResult { request: release }) if result == release)));
}

#[test]
fn retirement_failure_without_exact_identity_cannot_authorize_a_fresh_attempt() {
    for mismatch in ["operation", "target"] {
        let now = Instant::now();
        let (mut controller, target, _) = retirement_cleanup_fixture(now);
        let record = controller.records.get_mut(&1).unwrap();
        record.lifecycle_application_retirement = record::ApplicationRetirementState::Pending;
        let operation = record.application_retirement_operation();
        let mut payload = json!({"status":"recovery_required","operation_id":operation});
        if mismatch == "operation" {
            payload["operation_id"] = json!("other-operation");
        } else {
            payload["target"] = target;
            payload["target"]["generation"] = json!(2);
        }
        let job = workflow::Job {
            record: 1,
            phase: workflow::Phase::LookupApplicationRetirement,
            call: call::Call::new("unused", json!({}), "unused".into(), 1_000),
            deadline: now + Duration::from_secs(5),
        };
        controller.consume(&job, payload, now, &mut Vec::new());
        assert!(controller.cancel(20, &peer(1), now).is_none());
        assert_eq!(
            controller.records[&1].application_retirement_operation(),
            operation
        );
        assert_eq!(
            workflow::next_phase(&controller.records[&1]),
            workflow::Phase::EndLifecycle
        );
        // Once the original end fence is present, reconciliation remains a
        // lookup of the same proof, even after the explicit retry.
        controller.records.get_mut(&1).unwrap().lifecycle_end_fenced = true;
        assert_eq!(
            workflow::next_phase(&controller.records[&1]),
            workflow::Phase::LookupApplicationRetirement
        );
    }
}

#[test]
fn retirement_attempt_ids_stay_bounded_and_counter_exhaustion_is_reported() {
    let now = Instant::now();
    let (mut controller, _, _) = retirement_cleanup_fixture(now);
    let record = controller.records.get_mut(&1).unwrap();
    record.application_operation =
        format!("ale1:{}:{}", "a".repeat(32), record.application_operation);
    assert_eq!(record.application_retirement_operation().len(), 128);
    record.lifecycle_application_retirement_attempt = u32::MAX;
    let operation = record.application_retirement_operation();
    assert!(operation.len() <= 128);
    assert!(operation.is_ascii());
    record.lifecycle_application_retirement = record::ApplicationRetirementState::Failed;
    record.fail_cleanup("lifecycle_application_retirement_unconfirmed");
    let status = controller.management_status(None);
    let launch = &status["launch"];
    let reply = controller.management_stop(
        launch["reference"].as_str().unwrap(),
        launch["revision"].as_str().unwrap(),
        now,
    );
    assert_eq!(
        reply,
        json!({"ok":false,"code":"application_retirement_attempts_exhausted"})
    );
    assert_eq!(controller.records[&1].state, State::CleanupFailed);
    assert_eq!(
        controller.records[&1].application_retirement_operation(),
        operation
    );
    assert_eq!(controller.cancel(20, &peer(1), now).unwrap().status, 503);
    assert!(controller.records[&1].cancel_connection.is_none());
}
