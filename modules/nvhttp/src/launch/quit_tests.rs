#[test]
fn unconfirmed_rtsp_cleanup_after_cross_client_quit_is_a_warning() {
    let (mut controller, mut broker, now) = one_client_application();
    broker.broker.fail_rtsp_cleanup = true;
    assert!(controller.cancel(20, &peer(2), now).is_none());
    let (connection, reply) = broker.reply(&mut controller, now);
    assert_eq!(connection, 20);
    assert_eq!(reply.status, 503);
    assert!(reply_text(&reply).contains("stream cleanup is incomplete"));

    let record = &controller.records[&1];
    assert_eq!(record.state, State::CleanupFailed);
    assert_eq!(record.cleanup_error, Some("rtsp_cleanup_unconfirmed"));
    assert!(record.rtsp_requested);
    assert_eq!(record.rtsp_ticket, Some(71));
    assert!(!broker.broker.application_stopped);

    // The client response releases the connection. Background retries must
    // report the same warning while continuing to check the exact ticket.
    for tick in 1..=25 {
        controller.poll(now + Duration::from_millis(tick * 100), &mut |command| {
            broker.send(command)
        });
    }
    let lifecycle = lifecycle_of(&controller, 1);
    assert!(lifecycle.healthy(), "{:?}", lifecycle.root_cause());
    let entries: Vec<_> = lifecycle
        .entries()
        .filter(|entry| entry.issue.code == "rtsp_cleanup_unconfirmed")
        .collect();
    assert!(entries.len() >= 2);
    for entry in entries {
        assert_eq!(entry.issue.disposition, IssueDisposition::AdvisoryWarning);
        assert_eq!(entry.issue.caused_by, Some(LifecycleLayer::Rtsp));
        assert_eq!(entry.issue.phase, "verify_rtsp_stopped");
    }
    assert_eq!(controller.records[&1].rtsp_ticket, Some(71));
    assert!(!broker.broker.application_stopped);
}

#[test]
fn quit_during_transport_monitor_keeps_application_cleanup_authority() {
    for transport_state in [
        "playing", "closing", "closed", "failed", "invalid", "rejected", "settled",
    ] {
        let (mut controller, mut broker, now) = one_client_application();
        broker.transports.get_mut(&71).unwrap()["state"] = json!("closed");
        let mut reply = broker.terminal_transport(71);
        reply["state"] = json!(transport_state);
        let mut monitor = workflow::Job {
            record: 1,
            phase: Phase::MonitorRtsp,
            call: Call::new("unused", json!({}), "unused".into(), 1000),
            deadline: now + Duration::from_secs(5),
        };

        assert!(controller.cancel(20, &peer(1), now).is_none());
        let deadline = controller.records[&1].deadline;
        // A reply already delivered by the broker can outlive cancellation
        // of the monitor that requested it.
        match transport_state {
            "rejected" => controller.advance(
                &mut monitor,
                now,
                &mut |_| Ok(Response::error("unavailable", "monitor unavailable")),
                &mut Vec::new(),
            ),
            "settled" => {
                monitor.call.cancel();
                controller.advance(
                    &mut monitor,
                    now,
                    &mut |_| panic!("an unsent cancelled monitor has no broker work"),
                    &mut Vec::new(),
                );
            }
            _ => controller.consume(&monitor, reply, now, &mut Vec::new()),
        }
        let record = &controller.records[&1];
        assert_eq!(record.state, State::Stopping, "{transport_state}");
        assert_eq!(record.stage, Stage::Cleanup, "{transport_state}");
        assert!(record.owns_application_cleanup(), "{transport_state}");
        assert_eq!(record.deadline, deadline);

        let (connection, reply) = broker.reply(&mut controller, now);
        assert_eq!(connection, 20);
        assert_eq!(reply.status, 200);
        assert!(broker.broker.application_stopped);
        assert_eq!(controller.current_game(&peer(1)), 0);
    }
}

#[test]
fn quit_escalates_transport_cleanup_without_restarting_its_deadline() {
    for shared in [false, true] {
        let (mut controller, mut broker, now) = if shared {
            two_client_application()
        } else {
            one_client_application()
        };
        let record = controller.records.get_mut(&1).unwrap();
        record.request_transport_cleanup(None, now);
        let deadline = record.deadline;
        let quit_at = now + Duration::from_secs(1);
        assert!(controller.cancel(20, &peer(1), quit_at).is_none());
        assert!(controller.records[&1].owns_application_cleanup());
        assert_eq!(controller.records[&1].deadline, deadline);

        let (connection, reply) = broker.reply(&mut controller, quit_at);
        assert_eq!(connection, 20);
        assert_eq!(reply.status, 200);
        assert!(broker.broker.application_stopped);
        assert!(
            controller
                .records
                .values()
                .all(|record| record.state.terminal())
        );
    }
}
