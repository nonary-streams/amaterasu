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
    let mut broker = Broker {
        application_executable_path: Some("/games/example".into()),
        application_input_overrides: Some(json!({"vendor_option":{"quality":"high"}})),
        ..Default::default()
    };
    broker.lifecycle_replies.push_back(json!({"status":"pending"}));
    broker.lifecycle_replies.push_back(json!({"status":"succeeded", "target":{"kind":"session","host_epoch":"epoch","session_id":"session","generation":1}}));
    let mut client = peer_with_display_mode(1, "1920x1080x59.94").unwrap();
    client.client_properties = json!({"vendor_option":{"enabled":false,"rate":0}});
    assert!(controller.launch(10, request(1), client, now).is_none());
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
    assert_eq!(lifecycle[0]["client_context"]["client_ref"], "client-1");
    assert_eq!(lifecycle[0]["client_context"]["display_mode"]["refresh_millihz"], 59940);
    assert_eq!(lifecycle[0]["client_context"]["properties"], json!({"vendor_option":{"enabled":false,"rate":0}}));
    assert_eq!(lifecycle[0]["application_ref"]["properties"]["vendor_option"], json!({"quality":"high"}));
    assert_eq!(lifecycle[0]["application_ref"]["executable_path"], "/games/example");
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
fn failed_lifecycle_with_confirmed_cleanup_allows_another_launch() {
    let now = Instant::now();
    let mut controller = Controller::new(Some(config())).unwrap();
    controller.set_session_lifecycle_required(true);
    controller.set_global_display_policy(7, json!({}));
    let mut broker = Broker::default();
    broker.lifecycle_replies.push_back(json!({"status":"pending"}));
    broker.lifecycle_replies.push_back(json!({"status":"failed", "cleanup_completed":true,
        "reason":"processing_output_range_mismatch",
        "target":{"kind":"session","host_epoch":"epoch","session_id":"session","generation":1}}));
    assert!(controller.launch(10, request(1), peer(1), now).is_none());
    let reply = (0..100).find_map(|tick| {
        controller
            .poll(now + Duration::from_millis(tick * 100), &mut |command| broker.send(command))
            .into_iter()
            .next()
    }).expect("failed launch reply").1;
    assert_eq!(reply.status, 503);
    for tick in 0..100 {
        controller.poll(now + Duration::from_secs(2) + Duration::from_millis(tick * 100), &mut |command| broker.send(command));
        if !controller.busy() { break; }
    }
    assert!(!controller.busy());
    assert!(!broker.actions().contains(&("amaterasu.launch/1", "launch_export")));
    assert!(!broker.actions().contains(&("amaterasu.rtsp/1", "open")));
    assert!(controller.launch(11, request(2), peer(1), now + Duration::from_secs(20)).is_none());
}

#[test]
fn pending_extension_lifecycle_deadline_does_not_block_the_next_launch() {
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
    let failed = *controller.records.keys().next().unwrap();
    assert_eq!(controller.records[&failed].state, State::CleanupFailed);
    // The next launch waits for the failed one instead of being refused.
    assert!(controller.launch(12, request(2), peer(2), expiry).is_none());
    assert!(controller.deferred_launch.is_some());
    // No session identity exists to retire, so the first automatic pass
    // abandons the record and the waiting launch is admitted.
    for tick in 0..40 {
        let time = expiry + Duration::from_millis(tick * 100);
        assert!(controller.poll(time, &mut |command| broker.send(command)).is_empty());
        if controller.deferred_launch.is_none() { break; }
    }
    assert_eq!(controller.records[&failed].state, State::CleanupAbandoned);
    assert_eq!(controller.records[&failed].cleanup_error, Some("lifecycle_retirement_unavailable"));
    assert!(controller.deferred_launch.is_none());
    assert!(controller.records.values().any(|record| {
        record.launch_connection == Some(12) && record.state == State::Starting
    }));
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
    assert_eq!(record.cleanup_error, Some("lifecycle_prepare_unconfirmed"));
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
fn retained_application_resumes_on_another_paired_client_without_relaunch() {
    let now = Instant::now();
    let mut controller = Controller::new(Some(config())).unwrap();
    controller.set_global_display_policy(7, json!({}));
    let mut broker = Broker::default();
    assert!(controller.launch(10, request(1), peer(1), now).is_none());
    assert_eq!(poll_until_reply(&mut controller, &mut broker, now).1.status, 200);
    controller.reply_delivered(10, now);
    let target = json!({"kind":"session","host_epoch":"epoch","session_id":"session","generation":1});
    let record = controller.records.values_mut().next().unwrap();
    record.lifecycle_required = true;
    record.lifecycle_prepare_dispatched = true;
    record.lifecycle_application_bound = true;
    record.lifecycle_launch_epoch_ready = true;
    record.lifecycle_session_target = serde_json::from_value(target).ok();
    record.application_display_overrides = Some(json!({}));
    record.state = State::Paused;
    record.stage = Stage::Live;
    record.rtsp_requested = false;
    record.rtsp_ticket = None;
    record.rtsp_url = None;
    record.history_release_required = false;
    let application_operation = record.application_operation.clone();
    let application_launch = record.application_launch;
    let old_rtsp_operation = record.rtsp_operation.clone();
    broker.commands.clear();
    broker.lifecycle_replies.push_back(json!({"status":"ok","host_epoch":"epoch","sessions":[{"session_id":"session","generation":1,"phase":"paused"}]}));

    assert_eq!(controller.current_game(&peer(2)), 1);
    assert!(controller.resume(11, request(2), &peer(2), now).is_none());
    let (connection, reply) = poll_until_reply(&mut controller, &mut broker, now);
    assert_eq!(connection, 11);
    assert_eq!(reply.status, 200, "{} failure={:?} cleanup={:?} commands={:?}", String::from_utf8_lossy(&reply.body),
        controller.records.values().next().unwrap().failure,
        controller.records.values().next().unwrap().cleanup_error, broker.actions());
    assert!(String::from_utf8_lossy(&reply.body).contains("<resume>1</resume>"));
    controller.reply_delivered(11, now);
    let record = controller.records.values().next().unwrap();
    assert_eq!(record.state, State::Ready);
    assert_eq!(record.peer.client_id.as_deref(), Some("client-2"));
    assert_eq!(record.request.remote_input_key, request(2).remote_input_key);
    assert_eq!(record.application_operation, application_operation);
    assert_eq!(record.application_launch, application_launch);
    assert_ne!(record.rtsp_operation, old_rtsp_operation);
    assert!(!broker.actions().contains(&("amaterasu.launch/1", "launch_export")));
    assert!(!broker.actions().contains(&("amaterasu.launch/1", "stop")));
    let open = broker.commands.iter().find_map(|command| match command {
        Command::Request { service, payload, .. } if service == "amaterasu.rtsp/1" && payload["action"] == "open" => Some(payload),
        _ => None,
    }).unwrap();
    assert_eq!(open["lifecycle_client_context"]["client_ref"], "client-2");
    assert_eq!(open["lifecycle_intent"], "resume");
}

#[test]
fn unauthenticated_client_cannot_resume_a_retained_application() {
    let now = Instant::now();
    let mut controller = Controller::new(Some(config())).unwrap();
    assert!(controller.launch(10, request(1), peer(1), now).is_none());
    let record = controller.records.values_mut().next().unwrap();
    record.lifecycle_required = true;
    record.state = State::Paused;
    record.stage = Stage::Live;
    let original_operation = record.rtsp_operation.clone();
    let unpaired = PeerIdentity::new("127.0.0.1".parse().unwrap(), vec![2; 32]).unwrap();
    assert_eq!(controller.resume(11, request(2), &unpaired, now).unwrap().status, 401);
    let record = controller.records.values().next().unwrap();
    assert_eq!(record.state, State::Paused);
    assert_eq!(record.rtsp_operation, original_operation);
    assert!(controller.pending_resume.is_none());
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

#[test]
fn lost_resume_reply_before_stream_admission_retains_application() {
    let now = Instant::now();
    let mut controller = Controller::new(Some(config())).unwrap();
    assert!(controller.launch(10, request(1), peer(1), now).is_none());
    let record = controller.records.values_mut().next().unwrap();
    record.lifecycle_session_target = serde_json::from_value(json!({"kind":"session","host_epoch":"e","session_id":"s","generation":1})).ok();
    record.resumed = true;
    record.adopted = false;
    record.application_launch = Some(42);
    record.application_requested = true;
    record.history_release_required = false;
    record.stage = Stage::VerifyResumeSession;
    controller.connection_closed(10, now);
    let record = controller.records.values().next().unwrap();
    assert_eq!(record.state, State::Paused);
    assert_eq!(record.application_launch, Some(42));
    assert!(record.application_requested);
}

#[test]
fn uncertain_abandoned_resume_keeps_application_fenced() {
    let now = Instant::now();
    let mut controller = Controller::new(Some(config())).unwrap();
    assert!(controller.launch(10, request(1), peer(1), now).is_none());
    let record = controller.records.values_mut().next().unwrap();
    let id = record.id;
    record.resume_abandoned = true;
    record.application_requested = true;
    record.application_launch = Some(42);
    let mut job = workflow::Job { record:id, phase:workflow::Phase::ReserveHistory,
        call:call::Call::new("amaterasu.stream-session/1", json!({}), "resume-history".into(), 5000), deadline:now + Duration::from_secs(10) };
    let mut replies = Vec::new();
    for _ in 0..4 {
        controller.advance(&mut job, now, &mut |_| Err("broker unavailable".into()), &mut replies);
        if let Some(next) = controller.job.take() { job = next; }
    }
    let record = &controller.records[&id];
    assert_eq!(record.state, State::CleanupFailed);
    assert_eq!(record.cleanup_error, Some("resume_settlement_unconfirmed"));
    assert_eq!(record.application_launch, Some(42));
    assert!(record.application_requested);
}

#[test]
fn connected_resume_history_settlement_uncertainty_preserves_application() {
    for fail_release in [false, true] {
        let now = Instant::now();
        let mut controller = Controller::new(Some(config())).unwrap();
        assert!(controller.launch(10, request(1), peer(1), now).is_none());
        let record = controller.records.values_mut().next().unwrap();
        let id = record.id;
        record.resumed = true;
        record.lifecycle_session_target = serde_json::from_value(
            json!({"kind":"session","host_epoch":"e","session_id":"s","generation":1}),
        )
        .ok();
        record.application_requested = true;
        record.application_launch = Some(42);
        record.stage = Stage::ReserveHistory;
        let mut job = workflow::Job {
            record: id,
            phase: workflow::Phase::ReserveHistory,
            call: call::Call::new(
                "amaterasu.stream-session/1",
                json!({}),
                "resume-history".into(),
                5000,
            ),
            deadline: now + Duration::from_secs(10),
        };
        let mut replies = Vec::new();
        let mut commands = Vec::new();
        for _ in 0..8 {
            controller.advance(
                &mut job,
                now,
                &mut |command| {
                    commands.push(command.clone());
                    match command {
                        Command::Request { .. } => Ok(Response::Accepted { id: 7 }),
                        Command::TakeResult { .. } => Err("lost result".into()),
                        Command::Cancel { .. } if fail_release => Ok(Response::Ok),
                        Command::Cancel { .. } | Command::ReleaseResult { .. } => {
                            Err("lost settlement".into())
                        }
                        _ => panic!("unexpected broker call"),
                    }
                },
                &mut replies,
            );
            if let Some(next) = controller.job.take() {
                job = next;
            } else {
                break;
            }
        }
        let record = &controller.records[&id];
        assert_eq!(record.state, State::CleanupFailed);
        assert_eq!(record.cleanup_error, Some("resume_settlement_unconfirmed"));
        assert_eq!(record.application_launch, Some(42));
        assert!(record.application_requested);
        assert_eq!(record.uncertain.len(), 1);
        assert!(commands.iter().any(|command| matches!(command, Command::TakeResult { request: 7 })));
        // A lost result leaves the provider operation fenced; cancellation or
        // release would not prove that a delivered mutation has retired.
        assert!(!commands.iter().any(|command| matches!(command, Command::Cancel { request: 7 } | Command::ReleaseResult { request: 7 })));
        assert!(!commands.iter().any(|command| matches!(command, Command::Request { service, .. } if service == "amaterasu.launch/1")));
    }
}

#[test]
fn terminal_lifecycle_end_reply_without_pending_fence_allows_cleanup() {
    let now = Instant::now();
    let mut controller = Controller::new(Some(config())).unwrap();
    assert!(controller.launch(10, request(1), peer(1), now).is_none());
    let target = json!({
        "kind":"session", "host_epoch":"epoch", "session_id":"session", "generation":1
    });
    let record = controller.records.values_mut().next().unwrap();
    record.lifecycle_session_target = serde_json::from_value(target.clone()).ok();
    record.lifecycle_prepare_dispatched = true;
    record.desktop = true;
    record.launch_connection = None;
    record.request_cleanup(None, now);
    let mut broker = Broker::default();
    // The coordinator's completed receipt has no pending-only `fenced` flag.
    for _ in 0..3 {
        broker
            .lifecycle_replies
            .push_back(json!({"status":"succeeded", "target":target}));
    }
    for tick in 0..100 {
        controller.poll(now + Duration::from_millis(tick * 100), &mut |command| {
            broker.send(command)
        });
        if controller.records.values().next().unwrap().state == State::Stopped {
            break;
        }
    }
    let record = controller.records.values().next().unwrap();
    assert_eq!(record.state, State::Stopped);
    assert!(record.lifecycle_end_fenced);
    assert!(record.lifecycle_end_complete);
}

#[test]
fn lifecycle_end_never_accepts_unfenced_pending_or_another_generation() {
    let target = json!({
        "kind":"session", "host_epoch":"epoch", "session_id":"session", "generation":1
    });
    let wrong_target = json!({
        "kind":"session", "host_epoch":"epoch", "session_id":"session", "generation":2
    });
    for reply in [
        json!({"status":"pending", "target":target}),
        json!({"status":"succeeded", "target":wrong_target}),
        json!({"status":"recovery_required", "target":target, "fenced":true}),
    ] {
        let now = Instant::now();
        let mut controller = Controller::new(Some(config())).unwrap();
        assert!(controller.launch(10, request(1), peer(1), now).is_none());
        let record = controller.records.values_mut().next().unwrap();
        record.lifecycle_session_target = serde_json::from_value(target.clone()).ok();
        record.lifecycle_prepare_dispatched = true;
        record.desktop = true;
        record.launch_connection = None;
        record.request_cleanup(None, now);
        let mut broker = Broker::default();
        broker.lifecycle_replies.push_back(reply);
        for tick in 0..20 {
            controller.poll(now + Duration::from_millis(tick * 100), &mut |command| {
                broker.send(command)
            });
            if controller.records.values().next().unwrap().state == State::CleanupFailed {
                break;
            }
        }
        assert!(
            !controller
                .records
                .values()
                .next()
                .unwrap()
                .lifecycle_end_fenced
        );
    }
}

#[test]
fn long_stream_monitoring_retires_results_before_real_broker_capacity_is_exhausted() {
    use amaterasu_contracts::{Grants, RuntimeConfig};
    let started = Instant::now();
    let mut controller = Controller::new(Some(config())).unwrap();
    let mut responses = Broker::default();
    assert!(
        controller
            .launch(10, request(1), peer(1), started)
            .is_none()
    );
    assert_eq!(
        poll_until_reply(&mut controller, &mut responses, started)
            .1
            .status,
        200
    );
    controller.reply_delivered(10, started);
    let mut broker = amaterasu_runtime::Broker::new(RuntimeConfig {
        principals: [
            (
                "amaterasu.nvhttp".into(),
                Grants {
                    call: ["amaterasu.launch/1".into(), "amaterasu.rtsp/1".into()].into(),
                    ..Grants::default()
                },
            ),
            (
                "status-provider".into(),
                Grants {
                    provide: ["amaterasu.launch/1".into(), "amaterasu.rtsp/1".into()].into(),
                    ..Grants::default()
                },
            ),
        ]
        .into(),
        ..RuntimeConfig::default()
    })
    .unwrap();
    for service in ["amaterasu.launch/1", "amaterasu.rtsp/1"] {
        assert!(matches!(
            broker.dispatch(
                "status-provider",
                Command::Register {
                    service: service.into()
                }
            ),
            Response::Ok
        ));
    }
    let mut monitored = 0;
    // Simulate a long stream without waiting for the broker's real-time expiry.
    // The real ledger permits only 512 retained requests, so consumed results
    // must be retired instead of relying on the mock's destructive TakeResult.
    for cycle in 0..700 {
        let now = started + Duration::from_millis((cycle + 1) * 250);
        for _ in 0..8 {
            controller.poll(now, &mut |command| {
                Ok(broker.dispatch("amaterasu.nvhttp", command))
            });
            if let Response::Request {
                id,
                service,
                payload,
                ..
            } = broker.dispatch("status-provider", Command::Poll {})
            {
                let Response::Accepted { id: fake } = responses
                    .send(Command::Request {
                        service,
                        payload,
                        timeout_ms: 5000,
                        operation_id: format!("status-{monitored}"),
                        parent: None,
                    })
                    .unwrap()
                else {
                    panic!("status fixture did not admit request")
                };
                let Response::Result { payload } = responses
                    .send(Command::TakeResult { request: fake })
                    .unwrap()
                else {
                    panic!("status fixture did not complete request")
                };
                assert!(matches!(
                    broker.dispatch(
                        "status-provider",
                        Command::Complete {
                            request: id,
                            payload
                        }
                    ),
                    Response::Ok
                ));
                monitored += 1;
            }
        }
        assert_eq!(
            controller.records.values().next().unwrap().state,
            State::Ready,
            "monitoring exhausted broker capacity after {monitored} status requests"
        );
    }
    assert!(monitored > 600);
    assert!(matches!(
        broker.dispatch(
            "amaterasu.nvhttp",
            Command::Request {
                service: "amaterasu.rtsp/1".into(),
                payload: json!({"action":"status","ticket":71}),
                timeout_ms: 5000,
                operation_id: "cleanup-can-still-be-admitted".into(),
                parent: None,
            }
        ),
        Response::Accepted { .. }
    ));
}

/// Answers the restart sweep's lifecycle calls itself and forwards every
/// other command, including launch lifecycle calls, to the launch fixture.
#[derive(Default)]
struct RestartSweepBroker {
    launch: Broker,
    hold_snapshots: bool,
    refuse_abandonment: bool,
    snapshot_reply: Value,
    lifecycle_operations: Vec<Value>,
    sweep_results: BTreeMap<u64, Value>,
    next: u64,
}

impl RestartSweepBroker {
    const FIRST_SWEEP_REQUEST: u64 = 1_000_000;

    fn send(&mut self, command: Command) -> Transport {
        match &command {
            Command::Request { service, payload, .. }
                if service == "amaterasu.session-lifecycle/1" =>
            {
                self.lifecycle_operations.push(payload.clone());
                let result = match payload["operation"].as_str() {
                    Some("snapshot") => self.snapshot_reply.clone(),
                    Some("recover_session") if payload["restart_abandonment"] == true => {
                        if self.refuse_abandonment {
                            json!({"status":"rejected","reason":"release_work_unavailable"})
                        } else {
                            json!({"status":"pending","operation_id":payload["operation_id"]})
                        }
                    }
                    _ => return self.launch.send(command),
                };
                self.next += 1;
                let id = Self::FIRST_SWEEP_REQUEST + self.next;
                self.sweep_results.insert(id, result);
                Ok(Response::Accepted { id })
            }
            Command::TakeResult { request } if self.sweep_results.contains_key(request) => {
                let result = &self.sweep_results[request];
                if self.hold_snapshots && result.get("sessions").is_some() {
                    Ok(Response::Pending)
                } else {
                    Ok(Response::Result { payload: result.clone() })
                }
            }
            Command::ReleaseResult { request } | Command::Cancel { request }
                if self.sweep_results.contains_key(request) =>
            {
                Ok(Response::Ok)
            }
            _ => self.launch.send(command),
        }
    }

    fn operations(&self) -> Vec<&str> {
        self.lifecycle_operations
            .iter()
            .map(|payload| payload["operation"].as_str().unwrap_or_default())
            .collect()
    }
}

fn stranded_snapshot() -> Value {
    json!({"status":"ok","host_epoch":"current","sessions":[
        {"session_id":"stranded","host_epoch":"previous","generation":1,"phase":"recovery_required","streams":[]},
        {"session_id":"ended","host_epoch":"previous","generation":1,"phase":"ended","streams":[]},
        {"session_id":"live","host_epoch":"current","generation":1,"phase":"recovery_required","streams":[]}
    ]})
}

#[test]
fn restart_sweep_abandons_stranded_sessions_without_reporting_retirement() {
    let now = Instant::now();
    let mut controller = Controller::new(Some(config())).unwrap();
    controller.set_session_lifecycle_required(true);
    controller.enable_restart_abandonment_sweep();
    let mut broker = RestartSweepBroker {
        snapshot_reply: stranded_snapshot(),
        ..Default::default()
    };
    for tick in 0..16 {
        controller.poll(now + Duration::from_millis(tick * 10), &mut |command| broker.send(command));
    }
    assert_eq!(broker.operations(), ["snapshot", "recover_session"]);
    assert_eq!(
        broker.lifecycle_operations[0]["restart_stranded"], true,
        "the bounded stranded listing, not the full snapshot, is requested"
    );
    let abandon = broker.lifecycle_operations[1].clone();
    assert_eq!(abandon["restart_abandonment"], true, "only the abandonment mode is sent");
    assert_eq!(
        abandon["target"],
        json!({"kind":"session","host_epoch":"current","session_id":"stranded","generation":1})
    );
    let operation_id = abandon["operation_id"].clone();
    assert!(operation_id.as_str().unwrap().starts_with("nvhttp-restart-abandon-"));
    assert!(!controller.busy());

    // A later sweep in the same epoch replays the same coordinator receipt.
    controller.restart_sweep.request_retry();
    for tick in 0..16 {
        controller.poll(now + Duration::from_millis(tick * 10), &mut |command| broker.send(command));
    }
    assert_eq!(
        broker.operations(),
        ["snapshot", "recover_session", "snapshot", "recover_session"]
    );
    assert_eq!(broker.lifecycle_operations[3]["operation_id"], operation_id);
}

/// A truncated listing is continued only after a round made progress, so
/// sessions the coordinator keeps refusing cannot loop the sweep.
#[test]
fn truncated_stranded_listing_continues_only_after_progress() {
    for (refuse, expected_snapshots) in [(false, 8), (true, 1)] {
        let now = Instant::now();
        let mut controller = Controller::new(Some(config())).unwrap();
        controller.set_session_lifecycle_required(true);
        controller.enable_restart_abandonment_sweep();
        let mut snapshot = stranded_snapshot();
        snapshot["truncated"] = json!(true);
        let mut broker = RestartSweepBroker {
            snapshot_reply: snapshot,
            refuse_abandonment: refuse,
            ..Default::default()
        };
        for tick in 0..400 {
            controller.poll(now + Duration::from_millis(tick * 10), &mut |command| broker.send(command));
        }
        let snapshots = broker.operations().iter().filter(|operation| **operation == "snapshot").count();
        assert_eq!(snapshots, expected_snapshots, "refuse={refuse}");
        assert!(!controller.busy());
    }
}

#[test]
fn launches_proceed_during_the_sweep_and_capacity_refusal_sweeps_again() {
    let now = Instant::now();
    let mut controller = Controller::new(Some(config())).unwrap();
    controller.set_session_lifecycle_required(true);
    controller.set_global_display_policy(7, json!({}));
    controller.enable_restart_abandonment_sweep();
    let mut broker = RestartSweepBroker {
        hold_snapshots: true,
        snapshot_reply: json!({"status":"ok","host_epoch":"current","sessions":[]}),
        ..Default::default()
    };
    broker.launch.lifecycle_replies.push_back(json!({"status":"succeeded",
        "target":{"kind":"session","host_epoch":"current","session_id":"session","generation":1}}));
    assert!(controller.launch(10, request(1), peer(1), now).is_none());
    for tick in 0..100 {
        controller.poll(now + Duration::from_millis(tick * 10), &mut |command| broker.send(command));
        if broker.launch.actions().contains(&("amaterasu.launch/1", "launch_export")) {
            break;
        }
    }
    assert!(broker.launch.actions().contains(&("amaterasu.launch/1", "launch_export")));
    assert_eq!(broker.operations(), ["snapshot", "prepare"], "sweep is still pending");

    let later = now + Duration::from_secs(60);
    let mut controller = Controller::new(Some(config())).unwrap();
    controller.set_session_lifecycle_required(true);
    controller.set_global_display_policy(7, json!({}));
    controller.enable_restart_abandonment_sweep();
    let mut broker = RestartSweepBroker {
        snapshot_reply: json!({"status":"ok","host_epoch":"current","sessions":[]}),
        ..Default::default()
    };
    broker.launch.lifecycle_replies.push_back(
        json!({"status":"rejected","reason":"session_capacity_exhausted"}),
    );
    for tick in 0..4 {
        controller.poll(later + Duration::from_millis(tick), &mut |command| broker.send(command));
    }
    assert_eq!(broker.operations(), ["snapshot"]);
    assert!(controller.launch(11, request(2), peer(1), later).is_none());
    for tick in 0..100 {
        controller.poll(later + Duration::from_millis(tick * 10), &mut |command| broker.send(command));
    }
    assert_eq!(broker.operations(), ["snapshot", "prepare", "snapshot"]);
    assert!(!broker.operations().contains(&"application_retired"));
}
