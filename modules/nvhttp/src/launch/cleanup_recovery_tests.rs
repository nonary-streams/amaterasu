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

/// One fixture host per launch record behind a single broker. The fixture
/// models one application and one ticket, so launches that overlap in time
/// each need their own. Calls are routed by the record named in their
/// operation identifier.
struct Generations {
    hosts: BTreeMap<u64, Broker>,
    stop_pending: bool,
}

const GENERATION_SPAN: u64 = 1 << 20;

impl Generations {
    fn new() -> Self {
        Self {
            hosts: BTreeMap::new(),
            stop_pending: false,
        }
    }

    fn host(&mut self, record: u64) -> &mut Broker {
        let stop_pending = self.stop_pending;
        self.hosts.entry(record).or_insert_with(|| Broker {
            application_stop_pending: stop_pending,
            ..Broker::default()
        })
    }

    fn send(&mut self, command: Command) -> Transport {
        let record = match &command {
            Command::Request { operation_id, .. } | Command::LookupRequest { operation_id, .. } => {
                operation_id
                    .split('-')
                    .nth(2)
                    .and_then(|id| u64::from_str_radix(id, 16).ok())
                    .unwrap_or(0)
            }
            Command::TakeResult { request }
            | Command::Cancel { request }
            | Command::ReleaseResult { request } => request / GENERATION_SPAN,
            _ => 0,
        };
        let local = |request: u64| request % GENERATION_SPAN;
        let command = match command {
            Command::TakeResult { request } => Command::TakeResult {
                request: local(request),
            },
            Command::Cancel { request } => Command::Cancel {
                request: local(request),
            },
            Command::ReleaseResult { request } => Command::ReleaseResult {
                request: local(request),
            },
            command => command,
        };
        match self.host(record).send(command) {
            Ok(Response::Accepted { id }) => Ok(Response::Accepted {
                id: record * GENERATION_SPAN + id,
            }),
            response => response,
        }
    }
}

fn poll_generations_until(
    controller: &mut Controller,
    hosts: &mut Generations,
    start: Instant,
    mut done: impl FnMut(&Controller, &[(u64, Reply)]) -> bool,
) -> (Instant, Vec<(u64, Reply)>) {
    let mut replies = Vec::new();
    for tick in 0..600 {
        let time = start + Duration::from_millis(tick * 100);
        replies.extend(controller.poll(time, &mut |command| hosts.send(command)));
        if done(controller, &replies) {
            return (time, replies);
        }
    }
    panic!("controller did not reach the expected state")
}

/// Launch on `connection` and deliver its reply, waiting out any hold.
fn launch_delivered(
    controller: &mut Controller,
    hosts: &mut Generations,
    connection: u64,
    key: u8,
    start: Instant,
) -> Instant {
    assert!(
        controller
            .launch(connection, request(key), peer(1), start)
            .is_none(),
        "launch on connection {connection} was refused"
    );
    let (delivered, replies) =
        poll_generations_until(controller, hosts, start, |_, replies| {
            replies.iter().any(|(answered, _)| *answered == connection)
        });
    let (_, reply) = replies
        .iter()
        .find(|(answered, _)| *answered == connection)
        .unwrap();
    assert_eq!(reply.status, 200, "{}", reply_text(reply));
    controller.reply_delivered(connection, delivered);
    delivered
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
fn a_launch_starts_after_the_grace_while_the_previous_application_never_finishes_closing() {
    let now = Instant::now();
    let mut controller = Controller::new(Some(config())).unwrap();
    let mut hosts = Generations::new();
    launch_delivered(&mut controller, &mut hosts, 10, 1, now);
    // The application accepts the stop and never finishes closing.
    hosts.host(1).application_stop_pending = true;
    assert!(controller.cancel(20, &peer(1), now).is_none());
    assert!(controller.launch(30, request(2), peer(1), now).is_none());
    assert!(
        controller
            .launch(31, request(3), peer(2), now)
            .is_some_and(|reply| reply.status == 503
                && reply_text(&reply).contains("another launch is waiting to start"))
    );
    controller.records.get_mut(&1).unwrap().deadline = now + INITIAL_DEADLINE * 4;

    let (admitted, replies) =
        poll_generations_until(&mut controller, &mut hosts, now, |controller, _| {
            controller.deferred_launch.is_none()
        });
    // The wait is the grace and no longer, and it ends in a launch.
    assert!(admitted.duration_since(now) >= cleanup_recovery::SUCCESSOR_GRACE);
    assert!(admitted.duration_since(now) < cleanup_recovery::SUCCESSOR_GRACE * 2);
    assert!(replies.iter().all(|(connection, _)| *connection != 30));
    assert_eq!(controller.records[&1].state, State::Stopping);

    // It then runs to a delivered stream beside the unfinished cleanup.
    let (_, replies) =
        poll_generations_until(&mut controller, &mut hosts, admitted, |_, replies| {
            replies.iter().any(|(connection, _)| *connection == 30)
        });
    let (_, reply) = replies
        .iter()
        .find(|(connection, _)| *connection == 30)
        .unwrap();
    assert_eq!(reply.status, 200, "{}", reply_text(reply));
    assert_eq!(controller.records[&1].state, State::Stopping);
    assert_eq!(application_stop_count(hosts.host(2)), 0);
}

#[test]
fn quit_and_management_act_on_the_running_application_not_the_one_left_closing() {
    let now = Instant::now();
    let mut controller = Controller::new(Some(config())).unwrap();
    let mut hosts = Generations::new();
    launch_delivered(&mut controller, &mut hosts, 10, 1, now);
    hosts.host(1).application_stop_pending = true;
    assert!(controller.cancel(20, &peer(1), now).is_none());
    controller.records.get_mut(&1).unwrap().deadline = now + INITIAL_DEADLINE * 4;
    let delivered = launch_delivered(&mut controller, &mut hosts, 30, 2, now);
    assert_eq!(controller.records[&1].state, State::Stopping);
    assert_eq!(controller.records[&2].state, State::Ready);

    // The console shows the application that is running.
    let status = controller.management_status(None);
    assert_eq!(status["launch"]["state"], "ready");

    // Quit ends it. The earlier cleanup does not answer for it.
    assert!(controller.cancel(40, &peer(1), delivered).is_none());
    assert_eq!(controller.records[&2].state, State::Stopping);
    let (_, replies) =
        poll_generations_until(&mut controller, &mut hosts, delivered, |_, replies| {
            replies.iter().any(|(connection, _)| *connection == 40)
        });
    let (_, reply) = replies
        .iter()
        .find(|(connection, _)| *connection == 40)
        .unwrap();
    assert_eq!(reply.status, 200, "{}", reply_text(reply));
    assert!(controller.records[&2].state.terminal());
    assert_eq!(controller.records[&1].state, State::Stopping);
}

#[test]
fn a_launch_does_not_wait_for_an_application_that_ended_long_ago() {
    let now = Instant::now();
    let mut controller = Controller::new(Some(config())).unwrap();
    let mut broker = Broker::default();
    launch_ready(&mut controller, &mut broker, now);
    broker.application_stop_pending = true;
    assert!(controller.cancel(20, &peer(1), now).is_none());
    controller.records.get_mut(&1).unwrap().deadline = now + INITIAL_DEADLINE * 4;
    let later = now + cleanup_recovery::SUCCESSOR_GRACE + Duration::from_secs(1);
    controller.poll(now, &mut |command| broker.send(command));
    controller.poll(later, &mut |command| broker.send(command));
    assert_eq!(controller.records[&1].state, State::Stopping);

    assert!(controller.launch(30, request(2), peer(1), later).is_none());
    assert!(controller.deferred_launch.is_none());
    assert!(controller.records.values().any(|record| {
        record.launch_connection == Some(30) && record.state == State::Starting
    }));
}

#[test]
fn a_retained_application_settling_its_transport_still_holds_the_slot() {
    let now = Instant::now();
    let mut controller = Controller::new(Some(config())).unwrap();
    let mut broker = Broker::default();
    launch_ready(&mut controller, &mut broker, now);
    let record = controller.records.get_mut(&1).unwrap();
    record.request_transport_cleanup(Some("stream_failed"), now);
    assert!(record.retains_application());
    assert_eq!(
        controller.application_admission(),
        cleanup_recovery::ApplicationAdmission::Settling
    );
    // A different application waits for the answer instead of starting
    // beside one that is being kept for resume.
    let mut other = request(2);
    other.app_id = 2;
    assert!(controller.launch(30, other, peer(1), now).is_none());
    assert!(controller.deferred_launch.is_some());
    let (_, replies) = poll_until(&mut controller, &mut broker, now, |controller| {
        controller.deferred_launch.is_none()
    });
    assert!(matches!(
        controller.records[&1].state,
        State::Ready | State::Paused
    ));
    let (_, reply) = replies
        .iter()
        .find(|(connection, _)| *connection == 30)
        .expect("the held launch is answered");
    assert_eq!(reply.status, 503);
    assert!(reply_text(reply).contains("another application is active"));
}

#[test]
fn unfinished_cleanups_never_use_up_launch_capacity() {
    let mut now = Instant::now();
    let mut controller = Controller::new(Some(config())).unwrap();
    let mut hosts = Generations::new();
    // Every application accepts its stop and never finishes closing.
    hosts.stop_pending = true;
    for cycle in 0..(MAX_RECORDS as u8 + 4) {
        let connection = 100 + u64::from(cycle) * 2;
        let delivered = launch_delivered(&mut controller, &mut hosts, connection, cycle + 1, now);
        assert!(
            controller
                .cancel(connection + 1, &peer(1), delivered)
                .is_none()
        );
        for record in controller.records.values_mut() {
            record.deadline = delivered + INITIAL_DEADLINE * 4_000;
        }
        assert!(controller.records.len() <= MAX_RECORDS);
        now = delivered + Duration::from_secs(1);
    }
    // More applications were left closing than the table holds, and each
    // later launch was still admitted and delivered.
    assert!(
        controller
            .records
            .values()
            .filter(|record| record.state == State::Stopping)
            .count()
            >= MAX_RECORDS - 1
    );
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
