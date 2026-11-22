#[test]
fn rapid_same_client_reconnect_retires_old_transport_then_resumes_the_root() {
    let (mut controller, mut broker, now) = one_client_application();
    assert!(controller.resume(90, request(3), &peer(1), now).is_none());
    assert!(controller.deferred_launch.is_some());
    assert_eq!(broker.transports.len(), 1);
    let (connection, reply) = broker.reply(&mut controller, now);
    assert_eq!(connection, 90);
    assert_eq!(
        reply.status,
        200,
        "{}",
        String::from_utf8_lossy(&reply.body)
    );
    assert_eq!(broker.transports[&71]["state"], "closed");
    assert_eq!(broker.transports[&72]["lifecycle_intent"], "resume");
    assert!(broker.transports[&72]["shared_source_operation_id"].is_null());
    assert_eq!(controller.records.len(), 1);
    assert!(!broker.broker.application_stopped);
}

#[test]
fn reconnect_waits_for_a_stopping_source_before_a_different_client_resumes() {
    let (mut controller, mut broker, now) = one_client_application();
    controller
        .records
        .values_mut()
        .next()
        .unwrap()
        .rtsp_stream_ending = true;
    broker.transports.get_mut(&71).unwrap()["state"] = json!("closing");
    assert!(controller.resume(90, request(3), &peer(2), now).is_none());
    for tick in 1..10 {
        controller.poll(now + Duration::from_millis(tick * 10), &mut |command| {
            broker.send(command)
        });
    }
    assert_eq!(
        broker.transports.len(),
        1,
        "a retiring source must never be borrowed"
    );
    broker.transports.get_mut(&71).unwrap()["state"] = json!("closed");
    let (_, reply) = broker.reply(&mut controller, now + Duration::from_secs(1));
    assert_eq!(
        reply.status,
        200,
        "{}",
        String::from_utf8_lossy(&reply.body)
    );
    assert_eq!(broker.transports[&72]["lifecycle_intent"], "resume");
}

#[test]
fn reconnect_wait_is_bounded_and_connection_close_cancels_admission() {
    let (mut controller, _broker, now) = one_client_application();
    assert!(controller.resume(90, request(3), &peer(1), now).is_none());
    let mut replies = Vec::new();
    controller.admit_deferred_launch(now + INITIAL_DEADLINE, &mut |_, _| true, &mut replies);
    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0].1.status, 503);
    assert!(controller.deferred_launch.is_none());

    let (mut controller, _broker, now) = one_client_application();
    assert!(controller.resume(90, request(3), &peer(1), now).is_none());
    controller.connection_closed(90, now);
    assert!(controller.deferred_launch.is_none());
    let status = controller.management_status(None);
    assert_eq!(
        status["recent_refusals"][0]["root_cause"]["issue"]["code"],
        "connection_closed"
    );
}

#[test]
fn unhealthy_reconnect_does_not_modify_the_predecessor_or_consume_the_wait_slot() {
    let (mut controller, _broker, now) = one_client_application();
    let mut invalid = request(3);
    invalid.width = 0;
    assert_eq!(
        controller
            .resume(90, invalid, &peer(1), now)
            .unwrap()
            .status,
        400
    );
    assert!(controller.deferred_launch.is_none());
    assert!(controller.records.values().next().unwrap().state == State::Ready);
}

#[test]
fn reconnect_keeps_live_transport_when_selected_encoder_cannot_serve_hdr() {
    let (mut controller, _broker, now) = one_client_application();
    let mut request = request(3);
    request.hdr = true;
    let reply = controller
        .resume(90, request, &peer(1), now)
        .expect("HDR unavailable");
    assert_eq!(reply.status, 503);
    assert!(controller.deferred_launch.is_none());
    assert!(controller.records.values().next().unwrap().state == State::Ready);
}

#[test]
fn delayed_queued_rtsp_status_retries_without_closing_the_transport_and_has_a_bound() {
    let (mut controller, _broker, now) = one_client_application();
    let id = *controller.records.keys().next().unwrap();
    let mut replies = Vec::new();
    let mut job = workflow::Job {
        record: id,
        phase: workflow::Phase::MonitorRtsp,
        call: call::Call::new(
            "replacement.rtsp/1",
            json!({"action":"status","ticket":71}),
            "late-read".into(),
            5_000,
        ),
        deadline: now + Duration::from_secs(5),
    };
    let mut commands = Vec::new();
    let mut send = |command| {
        let response = match &command {
            Command::Request { .. } => Response::Accepted { id: 100 },
            Command::TakeResult { .. } => Response::Pending,
            _ => Response::Ok,
        };
        commands.push(command);
        Ok(response)
    };
    controller.advance(&mut job, now, &mut send, &mut replies);
    for tick in 0..4 {
        controller.advance(
            &mut job,
            now + Duration::from_millis(5_100 + tick),
            &mut send,
            &mut replies,
        );
    }
    assert!(controller.records[&id].state == State::Ready);
    assert!(controller.records[&id].late_monitor_since.is_some());
    assert!(!commands.iter().any(|command| matches!(command, Command::Request { payload, .. } if payload["action"] == "close")));
    assert!(controller.records[&id].uncertain.is_empty());
    let expiry = now + Duration::from_secs(36);
    let mut next = workflow::Job {
        record: id,
        phase: workflow::Phase::MonitorRtsp,
        call: call::Call::new(
            "replacement.rtsp/1",
            json!({"action":"status","ticket":71}),
            "next-read".into(),
            5_000,
        ),
        deadline: expiry,
    };
    controller.advance(
        &mut next,
        expiry,
        &mut |_| panic!("expired unsubmitted read cannot dispatch"),
        &mut replies,
    );
    // Settled cancellation feeds the same stale-read policy.
    assert!(controller.records[&id].state == State::Stopping);
    assert_eq!(
        controller.records[&id].failure,
        Some("stream_observation_lost")
    );
}

#[test]
fn appidless_resume_keeps_the_retained_application_visible_during_transport_retirement() {
    let (mut controller, mut broker, now) = one_client_application();
    controller
        .records
        .values_mut()
        .next()
        .unwrap()
        .request_transport_cleanup(None, now);
    assert_eq!(controller.current_game(&peer(1)), 1);
    let mut resumed = request(3);
    resumed.app_id = 0;
    assert!(controller.resume(90, resumed, &peer(1), now).is_none());
    assert!(controller.deferred_launch.is_some());
    let (_, reply) = broker.reply(&mut controller, now);
    assert_eq!(
        reply.status,
        200,
        "{}",
        String::from_utf8_lossy(&reply.body)
    );
    assert_eq!(broker.transports[&72]["lifecycle_intent"], "resume");
}

#[test]
fn replacing_one_client_preserves_the_other_clients_transport_and_application() {
    let (mut controller, mut broker, now) = two_client_application();
    assert!(controller.resume(90, request(3), &peer(2), now).is_none());
    let (_, reply) = broker.reply(&mut controller, now);
    assert_eq!(
        reply.status,
        200,
        "{}",
        String::from_utf8_lossy(&reply.body)
    );
    assert_ne!(broker.transports[&71]["state"], "closed");
    assert_eq!(broker.transports[&72]["state"], "closed");
    assert_eq!(broker.transports[&73]["lifecycle_intent"], "join");
    assert!(!broker.broker.application_stopped);
}

#[test]
fn held_reconnect_rechecks_authorization_before_opening_a_new_transport() {
    let (mut controller, mut broker, now) = one_client_application();
    assert!(controller.resume(90, request(3), &peer(1), now).is_none());
    let mut reply = None;
    for tick in 1..100 {
        for (connection, response) in controller.poll_with_trust(
            now + Duration::from_millis(tick * 10),
            &mut |command| broker.send(command),
            |_| false,
        ) {
            if connection == 90 {
                reply = Some(response);
            }
        }
        if reply.is_some() {
            break;
        }
    }
    assert_eq!(reply.expect("held request must be refused").status, 401);
    assert_eq!(broker.transports.len(), 1);
}
