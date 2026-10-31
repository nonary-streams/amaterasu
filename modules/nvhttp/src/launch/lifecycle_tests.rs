#[test]
fn application_preparation_can_exceed_startup_deadline_before_rtsp_admission() {
    let started = Instant::now();
    let mut controller = Controller::new(Some(config())).unwrap();
    let mut broker = Broker {
        application_state: "starting",
        ..Broker::default()
    };

    assert!(
        controller
            .launch(10, request(1), peer(1), started)
            .is_none()
    );
    for _ in 0..128 {
        assert!(
            controller
                .poll(started, &mut |command| broker.send(command))
                .is_empty()
        );
        if controller.records.values().next().unwrap().stage == Stage::AwaitApplicationReady {
            break;
        }
    }
    assert_eq!(
        controller.records.values().next().unwrap().stage,
        Stage::AwaitApplicationReady
    );

    let launch_export = broker
        .commands
        .iter()
        .find_map(|command| match command {
            Command::Request {
                service, payload, ..
            } if service == "amaterasu.launch/1" && payload["action"] == "launch_export" => {
                Some(payload)
            }
            _ => None,
        })
        .expect("launch export request");
    assert!(
        launch_export["return_on_admission"]
            .as_bool()
            .unwrap_or(false)
    );

    for elapsed_seconds in 1..=50 {
        let now = started + Duration::from_secs(elapsed_seconds);
        for _ in 0..3 {
            assert!(
                controller
                    .poll(now, &mut |command| broker.send(command))
                    .is_empty()
            );
        }
    }

    let record = controller.records.values().next().unwrap();
    assert_eq!(record.stage, Stage::AwaitApplicationReady);
    assert_eq!(record.state, super::record::State::Starting);
    assert!(controller.busy());
    assert!(!broker.actions().contains(&("amaterasu.rtsp/1", "open")));
    let status_timeouts: Vec<_> = broker
        .commands
        .iter()
        .filter_map(|command| match command {
            Command::Request {
                service,
                payload,
                timeout_ms,
                ..
            } if service == "amaterasu.launch/1" && payload["action"] == "status" => {
                Some(*timeout_ms)
            }
            _ => None,
        })
        .collect();
    assert!(status_timeouts.len() >= 20);
    assert!(status_timeouts.iter().all(|timeout| *timeout == 5_000));

    broker.application_state = "active";
    let ready_at = started + Duration::from_secs(51);
    let (connection, reply) = poll_until_reply(&mut controller, &mut broker, ready_at);
    assert_eq!(connection, 10);
    assert_eq!(reply.status, 200);
    assert!(broker.actions().contains(&("amaterasu.rtsp/1", "open")));
    assert_eq!(
        controller.records.values().next().unwrap().deadline,
        ready_at + INITIAL_DEADLINE
    );
}

#[test]
fn explicit_cancel_stops_application_while_preparation_is_still_running() {
    let started = Instant::now();
    let mut controller = Controller::new(Some(config())).unwrap();
    let mut broker = Broker {
        application_state: "starting",
        ..Broker::default()
    };

    assert!(
        controller
            .launch(10, request(1), peer(1), started)
            .is_none()
    );
    for _ in 0..128 {
        assert!(
            controller
                .poll(started, &mut |command| broker.send(command))
                .is_empty()
        );
        if controller.records.values().next().unwrap().stage == Stage::AwaitApplicationReady {
            break;
        }
    }
    assert_eq!(
        controller.records.values().next().unwrap().stage,
        Stage::AwaitApplicationReady
    );

    assert!(
        controller
            .cancel(20, &peer(1), started + Duration::from_secs(1))
            .is_none()
    );
    let (connection, reply) = poll_until_reply(
        &mut controller,
        &mut broker,
        started + Duration::from_secs(1),
    );
    assert_eq!(connection, 20);
    assert_eq!(reply.status, 200);
    assert!(broker.actions().contains(&("amaterasu.launch/1", "stop")));
    assert!(!broker.actions().contains(&("amaterasu.rtsp/1", "open")));
    assert_eq!(controller.current_game(&peer(1)), 0);
}

#[test]
fn application_readiness_reuses_an_existing_rtsp_ticket() {
    let started = Instant::now();
    let mut controller = Controller::new(Some(config())).unwrap();
    let mut broker = Broker {
        application_state: "starting",
        ..Broker::default()
    };

    assert!(
        controller
            .launch(10, request(1), peer(1), started)
            .is_none()
    );
    for _ in 0..128 {
        assert!(
            controller
                .poll(started, &mut |command| broker.send(command))
                .is_empty()
        );
        if controller.records.values().next().unwrap().stage == Stage::AwaitApplicationReady {
            break;
        }
    }
    assert_eq!(
        controller.records.values().next().unwrap().stage,
        Stage::AwaitApplicationReady
    );

    let record = controller.records.values_mut().next().unwrap();
    record.rtsp_ticket = Some(77);
    record.rtsp_url = Some("rtspenc://127.0.0.1:48010/existing".into());
    record.rtsp_requested = true;
    broker.application_state = "active";

    let (connection, reply) = poll_until_reply(
        &mut controller,
        &mut broker,
        started + Duration::from_secs(1),
    );

    assert_eq!(connection, 10);
    assert_eq!(reply.status, 200);
    assert!(!broker.actions().contains(&("amaterasu.rtsp/1", "open")));
}

#[test]
fn required_extension_lifecycle_polls_pending_before_application_launch() {
    let now = Instant::now();
    let mut controller = Controller::new(Some(config())).unwrap();
    controller.set_session_lifecycle_required(true);
    controller.set_global_display_policy(7, json!({}));
    let mut broker = Broker::default();
    broker.lifecycle_replies.push_back(json!({"status":"pending"}));
    broker.lifecycle_replies.push_back(json!({"status":"succeeded", "target":{"kind":"session","host_epoch":"epoch","session_id":"session","generation":1}}));
    assert!(controller.launch(10, request(1), peer(1), now).is_none());
    for tick in 0..100 {
        controller.poll(now + Duration::from_millis(tick * 100), &mut |command| broker.send(command));
        if broker.actions().contains(&("amaterasu.launch/1", "launch_export")) { break; }
    }
    let lifecycle: Vec<_> = broker.commands.iter().filter_map(|command| match command {
        Command::Request { service, payload, .. } if service == "amaterasu.session-lifecycle/1" => Some(payload),
        _ => None,
    }).collect();
    assert_eq!(lifecycle.len(), 2);
    assert_eq!(lifecycle[0]["operation"], "prepare");
    assert_eq!(lifecycle[1]["operation"], "lookup");
    assert_eq!(lifecycle[0]["operation_id"], lifecycle[1]["operation_id"]);
    let released = broker.commands.iter().filter(|command| matches!(command,
        Command::ReleaseResult { request } if broker.lifecycle_requests.contains(request))).count();
    assert_eq!(released, 2);
    assert!(broker.actions().contains(&( "amaterasu.launch/1", "launch_export")));
    assert!(!broker.actions().contains(&( "amaterasu.rtsp/1", "open")));
    let record = controller.records.values().next().unwrap();
    assert_eq!(record.state, State::Starting);
    assert!(record.lifecycle_prepare_dispatched);
    assert!(record.lifecycle_session_target.is_some());
}

#[test]
fn unavailable_extension_lifecycle_refuses_before_application_or_rtsp() {
    let now = Instant::now();
    let mut controller = Controller::new(Some(config())).unwrap();
    controller.set_session_lifecycle_required(true);
    controller.set_global_display_policy(7, json!({}));
    let mut broker = Broker { lifecycle_unavailable: true, ..Default::default() };
    assert!(controller.launch(10, request(1), peer(1), now).is_none());
    let (_, reply) = poll_until_reply(&mut controller, &mut broker, now);
    assert_eq!(reply.status, 503);
    assert!(!broker.actions().contains(&( "amaterasu.launch/1", "launch_export")));
    assert!(!broker.actions().contains(&( "amaterasu.rtsp/1", "open")));
}

#[test]
fn pending_extension_lifecycle_deadline_keeps_session_fenced() {
    let now = Instant::now();
    let mut controller = Controller::new(Some(config())).unwrap();
    controller.set_session_lifecycle_required(true);
    controller.set_global_display_policy(7, json!({}));
    let mut broker = Broker::default();
    assert!(controller.launch(10, request(1), peer(1), now).is_none());
    for _ in 0..32 {
        controller.poll(now, &mut |command| broker.send(command));
        if controller.records.values().next().unwrap().lifecycle_prepare_pending { break; }
    }
    assert!(controller.records.values().next().unwrap().lifecycle_prepare_pending);
    let expiry = now + INITIAL_DEADLINE + Duration::from_secs(1);
    let mut replies = controller.poll(expiry, &mut |command| broker.send(command));
    replies.extend(controller.poll(expiry, &mut |command| broker.send(command)));
    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0].1.status, 503);
    assert_eq!(controller.records.values().next().unwrap().state, State::CleanupFailed);
    assert_eq!(controller.launch(12, request(2), peer(2), expiry).unwrap().status, 503);
    assert!(!broker.actions().contains(&("amaterasu.launch/1", "launch_export")));
}

#[test]
fn extension_lifecycle_result_release_uncertainty_remains_fenced() {
    let now = Instant::now();
    let mut controller = Controller::new(Some(config())).unwrap();
    controller.set_session_lifecycle_required(true);
    controller.set_global_display_policy(7, json!({}));
    let mut broker = Broker { lifecycle_release_failure: true, ..Default::default() };
    assert!(controller.launch(10, request(1), peer(1), now).is_none());
    let (_, reply) = poll_until_reply(&mut controller, &mut broker, now);
    assert_eq!(reply.status, 503);
    let record = controller.records.values().next().unwrap();
    assert_eq!(record.state, State::CleanupFailed);
    assert_eq!(record.cleanup_error, Some("lifecycle_result_release_unconfirmed"));
    assert_eq!(record.uncertain.len(), 1);
    assert!(!broker.actions().contains(&("amaterasu.launch/1", "launch_export")));
}

#[test]
fn lifecycle_target_requires_a_bounded_tagged_session_identity() {
    let payload = json!({"target":{"kind":"session","host_epoch":"epoch-1","session_id":"session-1","generation":1}});
    assert!(workflow::lifecycle_session_target(&payload).is_some());
    for target in [
        json!({"kind":"session","host_epoch":"epoch","session_id":"","generation":1}),
        json!({"kind":"session","host_epoch":"epoch","session_id":"session","generation":0}),
        json!({"kind":"stream","host_epoch":"epoch","session_id":"session","stream_id":"stream","generation":1}),
        json!({"kind":"session","host_epoch":"epoch","session_id":"session","generation":1,"extra":true}),
    ] {
        assert!(workflow::lifecycle_session_target(&json!({"target":target})).is_none());
    }
}

#[test]
fn paused_snapshot_requires_exact_epoch_session_and_generation() {
    let now = Instant::now();
    let mut controller = Controller::new(Some(config())).unwrap();
    assert!(controller.launch(10, request(1), peer(1), now).is_none());
    let record = controller.records.values_mut().next().unwrap();
    record.lifecycle_session_target = serde_json::from_value(json!({"kind":"session","host_epoch":"e","session_id":"s","generation":2})).ok();
    let mut snapshot = json!({"status":"ok","host_epoch":"e","sessions":[{"session_id":"s","generation":2,"phase":"paused","streams":[]}]});
    assert!(workflow::lifecycle_session_is_paused(record, &snapshot));
    snapshot["sessions"][0]["generation"] = json!(3);
    assert!(!workflow::lifecycle_session_is_paused(record, &snapshot));
    snapshot["sessions"][0]["generation"] = json!(2);
    snapshot["host_epoch"] = json!("other");
    assert!(!workflow::lifecycle_session_is_paused(record, &snapshot));
}

#[test]
fn lifecycle_end_fences_before_retirement_and_waits_for_release() {
    let now = Instant::now();
    let mut controller = Controller::new(Some(config())).unwrap();
    assert!(controller.launch(10, request(1), peer(1), now).is_none());
    let target = json!({"kind":"session","host_epoch":"e","session_id":"s","generation":1});
    let record = controller.records.values_mut().next().unwrap();
    record.lifecycle_session_target = serde_json::from_value(target.clone()).ok();
    record.lifecycle_prepare_dispatched = true;
    record.desktop = true;
    record.launch_connection = None;
    record.request_cleanup(None, now);
    let mut broker = Broker::default();
    broker.lifecycle_replies.push_back(json!({"status":"pending","target":target,"fenced":true}));
    broker.lifecycle_replies.push_back(json!({"status":"succeeded","target":target}));
    broker.lifecycle_replies.push_back(json!({"status":"succeeded","target":target}));
    for tick in 0..100 {
        controller.poll(now + Duration::from_millis(tick * 100), &mut |command| broker.send(command));
        if controller.records.values().next().unwrap().state == State::Stopped { break; }
    }
    assert_eq!(controller.records.values().next().unwrap().state, State::Stopped);
    let operations: Vec<_> = broker.commands.iter().filter_map(|command| match command {
        Command::Request {service, payload, ..} if service == "amaterasu.session-lifecycle/1" => payload["operation"].as_str(),
        _ => None,
    }).collect();
    assert_eq!(operations, ["end_session", "application_retired", "lookup"]);
}

#[test]
fn retained_resume_uses_fresh_rtsp_operation_and_rechecks_paused_session() {
    let now = Instant::now();
    let mut controller = Controller::new(Some(config())).unwrap();
    assert!(controller.launch(10, request(1), peer(1), now).is_none());
    let record = controller.records.values_mut().next().unwrap();
    record.lifecycle_required = true;
    record.lifecycle_prepare_dispatched = true;
    record.lifecycle_session_target = serde_json::from_value(json!({"kind":"session","host_epoch":"e","session_id":"s","generation":1})).ok();
    record.state = State::Paused;
    record.stage = Stage::Live;
    record.adopted = true;
    record.launch_connection = None;
    let old_operation = record.rtsp_operation.clone();
    assert!(controller.resume(11, request(1), &peer(1), now).is_none());
    let record = controller.records.values().next().unwrap();
    assert_ne!(record.rtsp_operation, old_operation);
    assert!(!record.adopted);
    assert_eq!(record.stage, Stage::VerifyResumeSession);
    assert_eq!(record.application_operation, operation_id(&controller.nonce, record.id, "application"));
}

#[test]
fn lifecycle_application_cleanup_failure_remains_fenced() {
    let now = Instant::now();
    let mut controller = Controller::new(Some(config())).unwrap();
    assert!(controller.launch(10, request(1), peer(1), now).is_none());
    let record = controller.records.values_mut().next().unwrap();
    let id = record.id;
    record.lifecycle_session_target = serde_json::from_value(json!({"kind":"session","host_epoch":"e","session_id":"s","generation":1})).ok();
    record.state = State::Stopping;
    record.stage = Stage::Cleanup;
    record.lifecycle_end_fenced = true;
    record.application_requested = true;
    record.application_cleanup_blocked = true;
    controller.finish_or_retain(id, now, &mut Vec::new());
    assert_eq!(controller.records[&id].state, State::CleanupFailed);
    assert_eq!(controller.records[&id].cleanup_error, Some("application_cleanup_unconfirmed"));
}

#[test]
fn lifecycle_binds_known_application_before_reply_or_cleanup() {
    let now = Instant::now();
    let mut controller = Controller::new(Some(config())).unwrap();
    assert!(controller.launch(10, request(1), peer(1), now).is_none());
    let record = controller.records.values_mut().next().unwrap();
    record.lifecycle_required = true;
    record.lifecycle_launch_epoch_ready = true;
    record.lifecycle_session_target = serde_json::from_value(json!({"kind":"session","host_epoch":"e","session_id":"s","generation":1})).ok();
    record.application_launch = Some(42);
    record.rtsp_ticket = Some(71);
    record.stage = Stage::OpenRtsp;
    assert_eq!(workflow::next_phase(record), workflow::Phase::BindApplication);
    record.state = State::Stopping;
    record.stage = Stage::Cleanup;
    assert_eq!(workflow::next_phase(record), workflow::Phase::BindApplication);
}

#[test]
fn rejected_resume_retains_application_and_releases_new_history() {
    let now = Instant::now();
    let mut controller = Controller::new(Some(config())).unwrap();
    assert!(controller.launch(10, request(1), peer(1), now).is_none());
    let record = controller.records.values_mut().next().unwrap();
    record.application_launch = Some(42);
    record.application_requested = true;
    record.history_release_required = true;
    let mut replies = Vec::new();
    workflow::reject_resume(record, &mut replies);
    assert_eq!(replies.len(), 1);
    assert_eq!(record.stage, Stage::ReleaseRejectedResumeHistory);
    assert_eq!(record.application_launch, Some(42));
    assert!(record.application_requested);
    assert!(record.adopted);
}
