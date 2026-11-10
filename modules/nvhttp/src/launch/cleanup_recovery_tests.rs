// An earlier launch's cleanup must not decide whether a later launch starts.

fn poll_until(
    controller: &mut Controller,
    broker: &mut Broker,
    start: Instant,
    mut done: impl FnMut(&Controller) -> bool,
) -> (Instant, Vec<(u64, Reply)>) {
    let mut replies = Vec::new();
    for tick in 0..600 {
        let time = start + Duration::from_millis(tick * 100);
        replies.extend(controller.poll(time, &mut |command| broker.send(command)));
        if done(controller) {
            return (time, replies);
        }
    }
    panic!("controller did not reach the expected state")
}

fn reply_text(reply: &Reply) -> String {
    String::from_utf8(reply.body.clone()).unwrap()
}

#[test]
fn a_launch_during_cleanup_waits_and_starts_when_the_application_has_closed() {
    let now = Instant::now();
    let mut controller = Controller::new(Some(config())).unwrap();
    let mut broker = Broker::default();
    launch_ready(&mut controller, &mut broker, now);
    assert!(controller.cancel(20, &peer(1), now).is_none());
    assert_eq!(controller.records[&1].state, State::Stopping);

    // The client's next launch arrives before cleanup has finished.
    assert!(controller.launch(30, request(2), peer(1), now).is_none());
    assert!(controller.deferred_launch.is_some());
    assert_eq!(controller.records.len(), 1);

    let (_, replies) = poll_until(&mut controller, &mut broker, now, |controller| {
        controller.deferred_launch.is_none()
    });
    assert!(
        replies
            .iter()
            .any(|(connection, reply)| *connection == 20 && reply.status == 200)
    );
    assert!(controller.records[&1].state.terminal());
    assert!(controller.records.values().any(|record| {
        record.launch_connection == Some(30) && record.state == State::Starting
    }));
}

#[test]
fn a_waiting_launch_reports_that_the_previous_application_is_still_closing() {
    let now = Instant::now();
    let mut controller = Controller::new(Some(config())).unwrap();
    let mut broker = Broker::default();
    launch_ready(&mut controller, &mut broker, now);
    // The application accepts the stop and keeps closing past the deadline.
    broker.application_stop_pending = true;
    assert!(controller.cancel(20, &peer(1), now).is_none());
    assert!(controller.launch(30, request(2), peer(1), now).is_none());
    assert!(
        controller
            .launch(31, request(3), peer(2), now)
            .is_some_and(|reply| reply.status == 503
                && reply_text(&reply).contains("another launch is waiting to start"))
    );
    let record = controller.records.get_mut(&1).unwrap();
    record.deadline = now + INITIAL_DEADLINE * 4;
    let mut reply = None;
    for tick in 0..=(INITIAL_DEADLINE.as_millis() as u64 / 100 + 10) {
        let time = now + Duration::from_millis(tick * 100);
        let replies = controller.poll(time, &mut |command| broker.send(command));
        if let Some(found) = replies.into_iter().find(|(connection, _)| *connection == 30) {
            reply = Some(found.1);
            break;
        }
    }
    let reply = reply.expect("the waiting launch is answered at its deadline");
    assert_eq!(reply.status, 503);
    assert!(reply_text(&reply).contains("previous application is still closing"));
    assert!(controller.deferred_launch.is_none());
}

#[test]
fn a_waiting_launch_is_dropped_when_the_host_is_asked_to_quit_or_the_client_leaves() {
    let now = Instant::now();
    let mut controller = Controller::new(Some(config())).unwrap();
    let mut broker = Broker::default();
    launch_ready(&mut controller, &mut broker, now);
    broker.application_stop_pending = true;
    assert!(controller.cancel(20, &peer(1), now).is_none());

    assert!(controller.launch(30, request(2), peer(1), now).is_none());
    controller.connection_closed(30, now);
    assert!(controller.deferred_launch.is_none());

    assert!(controller.launch(31, request(3), peer(1), now).is_none());
    let reply = controller.cancel(21, &peer(1), now).unwrap();
    assert_eq!(reply.status, 409);
    assert!(controller.deferred_launch.is_none());
    let replies = controller.poll(now, &mut |command| broker.send(command));
    assert!(
        replies
            .iter()
            .any(|(connection, reply)| *connection == 31 && reply.status == 409)
    );
}

#[test]
fn an_active_application_still_refuses_a_different_launch() {
    let now = Instant::now();
    let mut controller = Controller::new(Some(config())).unwrap();
    let mut broker = Broker::default();
    launch_ready(&mut controller, &mut broker, now);
    let mut other = request(2);
    other.app_id = 2;
    let reply = controller.launch(30, other, peer(1), now).unwrap();
    assert_eq!(reply.status, 503);
    assert!(reply_text(&reply).contains("another application is active"));
    assert!(controller.deferred_launch.is_none());
}

#[test]
fn failed_application_cleanup_is_read_back_then_abandoned_without_replaying_stop() {
    let now = Instant::now();
    let mut controller = Controller::new(Some(config())).unwrap();
    let mut broker = Broker::default();
    launch_ready(&mut controller, &mut broker, now);
    broker.application_terminal_stop_state = Some("cleanup_failed");
    assert!(controller.cancel(20, &peer(1), now).is_none());
    assert_eq!(
        poll_until_reply(&mut controller, &mut broker, now).1.status,
        503
    );
    assert_eq!(controller.records[&1].state, State::CleanupFailed);

    // A launch that arrives now waits for the automatic retries.
    assert!(controller.launch(30, request(2), peer(1), now).is_none());
    assert!(controller.deferred_launch.is_some());

    let (finished, _) = poll_until(&mut controller, &mut broker, now, |controller| {
        controller.records[&1].state.terminal()
    });
    let record = &controller.records[&1];
    assert_eq!(record.state, State::CleanupAbandoned);
    assert_eq!(record.cleanup_error, Some("application_cleanup_unconfirmed"));
    assert_eq!(
        usize::from(record.automatic_cleanup_retries),
        AUTOMATIC_CLEANUP_RETRY_DELAYS.len()
    );
    // The whole retry window fits inside one launch deadline.
    assert!(finished.duration_since(now) < INITIAL_DEADLINE);
    // The stop that failed was observed again, never sent again.
    assert_eq!(application_stop_count(&broker), 1);
    assert_eq!(management_state(record.state), "cleanup_abandoned");

    poll_until(&mut controller, &mut broker, finished, |controller| {
        controller.deferred_launch.is_none()
    });
    assert!(controller.records.values().any(|record| {
        record.launch_connection == Some(30) && record.state == State::Starting
    }));
}

#[test]
fn an_automatic_retry_completes_cleanup_that_only_needed_more_time() {
    let now = Instant::now();
    let mut controller = Controller::new(Some(config())).unwrap();
    let mut broker = Broker::default();
    launch_ready(&mut controller, &mut broker, now);
    broker.application_terminal_stop_state = Some("cleanup_failed");
    assert!(controller.cancel(20, &peer(1), now).is_none());
    assert_eq!(
        poll_until_reply(&mut controller, &mut broker, now).1.status,
        503
    );
    assert_eq!(controller.records[&1].state, State::CleanupFailed);
    // The application finishes closing on its own a moment later.
    broker.application_terminal_stop_state = None;
    broker.application_state = "stopped";
    poll_until(&mut controller, &mut broker, now, |controller| {
        controller.records[&1].state.terminal()
    });
    assert!(matches!(
        controller.records[&1].state,
        State::Stopped | State::Failed
    ));
    assert_eq!(controller.records[&1].cleanup_error, None);
    assert_eq!(application_stop_count(&broker), 1);
}

#[test]
fn quitting_during_a_status_poll_does_not_fail_cleanup() {
    let now = Instant::now();
    let mut controller = Controller::new(Some(config())).unwrap();
    let mut broker = Broker::default();
    launch_ready(&mut controller, &mut broker, now);
    // Leave a monitor read in flight, then lose its reply while quitting.
    let mut held = None;
    for _ in 0..8 {
        controller.poll(now + MONITOR_INTERVAL, &mut |command| match command {
            Command::TakeResult { request } if held.is_none() || held == Some(request) => {
                held = Some(request);
                Ok(Response::Pending)
            }
            command => broker.send(command),
        });
        if held.is_some() {
            break;
        }
    }
    let held = held.expect("a monitor read is in flight");
    assert!(
        controller
            .cancel(20, &peer(1), now + MONITOR_INTERVAL)
            .is_none()
    );
    let mut replies = Vec::new();
    for tick in 0..200 {
        let time = now + MONITOR_INTERVAL + Duration::from_millis(tick * 100);
        replies.extend(controller.poll(time, &mut |command| match command {
            Command::TakeResult { request } if request == held => Ok(Response::Pending),
            command => broker.send(command),
        }));
        if controller.records[&1].state.terminal() {
            break;
        }
    }
    assert_eq!(controller.records[&1].state, State::Stopped);
    assert!(controller.records[&1].uncertain.is_empty());
    assert!(
        replies
            .iter()
            .any(|(connection, reply)| *connection == 20 && reply.status == 200)
    );
    assert!(
        broker
            .commands
            .iter()
            .any(|command| matches!(command, Command::Cancel { request } if *request == held))
    );
}

#[test]
fn finished_launches_do_not_use_up_launch_capacity() {
    let mut now = Instant::now();
    let mut controller = Controller::new(Some(config())).unwrap();
    for cycle in 0..(MAX_RECORDS as u8 * 2) {
        let mut broker = Broker::default();
        let connection = 100 + u64::from(cycle) * 2;
        assert!(
            controller
                .launch(connection, request(cycle + 1), peer(1), now)
                .is_none(),
            "launch {cycle} was refused"
        );
        let (reply_connection, reply) = poll_until_reply(&mut controller, &mut broker, now);
        assert_eq!(reply_connection, connection);
        assert_eq!(reply.status, 200, "launch {cycle}: {}", reply_text(&reply));
        controller.reply_delivered(connection, now);
        assert!(controller.cancel(connection + 1, &peer(1), now).is_none());
        let (_, stopped) = poll_until_reply(&mut controller, &mut broker, now);
        assert_eq!(stopped.status, 200);
        assert!(controller.records.len() <= MAX_RECORDS);
        now += Duration::from_secs(1);
    }
    // An evicted launch key is still refused as a replay.
    let replay = controller.launch(999, request(1), peer(1), now).unwrap();
    assert_eq!(replay.status, 409);
}

#[test]
fn a_failed_transport_cleanup_keeps_the_retained_application_resumable() {
    let now = Instant::now();
    let mut controller = Controller::new(Some(config())).unwrap();
    let mut broker = Broker::default();
    launch_ready(&mut controller, &mut broker, now);
    let record = controller.records.get_mut(&1).unwrap();
    // Only this root's own transport failed to retire.
    record.request_transport_cleanup(Some("stream_failed"), now);
    record.fail_cleanup("rtsp_cleanup_unconfirmed");
    record.rtsp_cleanup_blocked = true;
    assert!(record.retains_application());
    broker.fail_rtsp_cleanup = true;

    poll_until(&mut controller, &mut broker, now, |controller| {
        !matches!(
            controller.records[&1].state,
            State::CleanupFailed | State::Stopping
        )
    });
    let record = &controller.records[&1];
    // The game is still owned and visible, so it can be resumed or quit.
    assert_eq!(record.state, State::Paused);
    assert!(record.application_requested);
    assert!(!record.rtsp_requested);
    assert_eq!(controller.current_game(&peer(1)), 1);
    assert_eq!(application_stop_count(&broker), 0);
}
