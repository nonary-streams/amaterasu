/// Separately owned RTSP tickets and coordinator receipts exercised through the
/// unchanged NVHTTP controller. No listeners or native input devices are opened.
#[derive(Default)]
struct SharedSessionBroker {
    broker: Broker,
    transports: BTreeMap<u64, Value>,
    operations: BTreeMap<String, u64>,
}

impl SharedSessionBroker {
    fn session_target() -> Value {
        json!({"kind":"session","host_epoch":"epoch","session_id":"application","generation":1})
    }

    fn stream_target(ticket: u64) -> Value {
        json!({"kind":"stream","host_epoch":"epoch","session_id":"application","stream_id":format!("client-{ticket}"),"generation":1})
    }

    fn terminal_transport(&self, ticket: u64) -> Value {
        let mut reply = self.transports[&ticket].clone();
        if reply["state"] == "closed" {
            let shared = self
                .transports
                .iter()
                .any(|(other, transport)| *other != ticket && transport["state"] != "closed");
            reply["lifecycle_retired"] = json!(!shared);
            reply["lifecycle_consumer_retired"] = json!(true);
            reply["lifecycle_resources_released"] = json!(!shared);
            reply["lifecycle_shared_retained"] = json!(shared);
            reply["lifecycle_shared_source"] = if shared {
                Self::stream_target(71)
            } else {
                Value::Null
            };
            reply["lifecycle_cleanup_state"] =
                json!(if shared { "shared_retained" } else { "retired" });
        }
        reply
    }

    fn send(&mut self, command: Command) -> Transport {
        let response = self.broker.send(command.clone())?;
        let (
            Command::Request {
                service, payload, ..
            },
            Response::Accepted { id },
        ) = (&command, &response)
        else {
            return Ok(response);
        };
        if service == "amaterasu.session-lifecycle/1" {
            let mut result = match payload["operation"].as_str().unwrap_or_default() {
                "prepare" => {
                    json!({"status":"succeeded","target":Self::session_target(),"capture_binding_mode":"managed"})
                }
                "snapshot" => {
                    let live = self
                        .transports
                        .values()
                        .any(|transport| transport["state"] != "closed");
                    json!({"status":"ok","host_epoch":"epoch","sessions":[{"session_id":"application","generation":1,"phase":if live {"ready"} else {"paused"}}]})
                }
                "bind_application" => {
                    json!({"status":"succeeded","target":payload["target"],"launch_id":payload["launch_id"],"application_operation_id":payload["application_operation_id"]})
                }
                "end_session" | "application_retired" | "lookup" => {
                    json!({"status":"succeeded","target":Self::session_target()})
                }
                operation => panic!("unexpected lifecycle operation {operation}"),
            };
            result["operation_id"] = payload["operation_id"].clone();
            self.broker.results.insert(*id, result);
        } else if service == "amaterasu.rtsp/1" {
            let action = payload["action"].as_str().unwrap_or_default();
            if action == "open" {
                let ticket = 71 + self.transports.len() as u64;
                let mut result = self.broker.results[id].clone();
                result["ticket"] = json!(ticket);
                result["session"] = json!(ticket + 100);
                result["rtsp_url"] = json!(format!("rtspenc://127.0.0.1:48010/launch-{ticket}"));
                result["lifecycle_target"] = Self::stream_target(ticket);
                result["lifecycle_cleanup_state"] = json!("retirement_pending");
                result["lifecycle_retired"] = json!(false);
                self.operations
                    .insert(payload["operation_id"].as_str().unwrap().to_owned(), ticket);
                self.transports.insert(ticket, result.clone());
                self.broker.results.insert(*id, result);
            } else {
                let ticket = payload["ticket"]
                    .as_u64()
                    .or_else(|| {
                        self.operations
                            .get(payload["operation_id"].as_str().unwrap_or_default())
                            .copied()
                    })
                    .expect("owned ticket");
                if action == "close" {
                    self.transports.get_mut(&ticket).unwrap()["state"] = json!("closed");
                }
                self.broker
                    .results
                    .insert(*id, self.terminal_transport(ticket));
            }
        }
        Ok(response)
    }

    fn reply(&mut self, controller: &mut Controller, now: Instant) -> (u64, Reply) {
        for tick in 0..100 {
            if let Some(reply) = controller
                .poll(now + Duration::from_millis(tick * 10), &mut |command| {
                    self.send(command)
                })
                .into_iter()
                .next()
            {
                return reply;
            }
        }
        panic!("shared application did not produce a bounded reply");
    }
}

fn one_client_application() -> (Controller, SharedSessionBroker, Instant) {
    one_client_application_with_lifetime(false)
}

fn one_client_application_with_lifetime(
    stop_on_stream_end: bool,
) -> (Controller, SharedSessionBroker, Instant) {
    let now = Instant::now();
    let mut controller = Controller::new(Some(config())).unwrap();
    controller.set_global_display_policy(7, json!({}));
    controller.set_session_lifecycle_required(true);
    let mut broker = SharedSessionBroker::default();
    broker.broker.application_stop_on_stream_end = stop_on_stream_end;
    assert!(controller.launch(10, request(1), peer(1), now).is_none());
    let (_, first) = broker.reply(&mut controller, now);
    assert_eq!(
        first.status,
        200,
        "{}",
        String::from_utf8_lossy(&first.body)
    );
    controller.reply_delivered(10, now);
    (controller, broker, now)
}

fn two_client_application() -> (Controller, SharedSessionBroker, Instant) {
    let (mut controller, mut broker, now) = one_client_application();
    assert!(controller.resume(11, request(2), &peer(2), now).is_none());
    let (_, second) = broker.reply(&mut controller, now);
    assert_eq!(
        second.status,
        200,
        "{}",
        String::from_utf8_lossy(&second.body)
    );
    controller.reply_delivered(11, now);
    (controller, broker, now)
}

#[test]
fn two_clients_have_distinct_transports_and_one_running_application() {
    let (mut controller, broker, now) = two_client_application();
    assert_eq!(controller.records.len(), 2);
    let clients: Vec<_> = controller.records.values().collect();
    assert!(clients.iter().all(|record| record.state == State::Ready));
    assert_ne!(clients[0].rtsp_ticket, clients[1].rtsp_ticket);
    assert_ne!(clients[0].rtsp_operation, clients[1].rtsp_operation);
    assert_ne!(
        clients[0].request.remote_input_key,
        clients[1].request.remote_input_key
    );
    assert_ne!(clients[0].connect_data, clients[1].connect_data);
    assert_eq!(clients[1].application_session, Some(clients[0].id));
    assert_eq!(clients[0].application_launch, clients[1].application_launch);
    assert!(clients[0].application_requested);
    assert!(!clients[1].application_requested);
    assert_eq!(
        broker
            .broker
            .actions()
            .iter()
            .filter(|action| **action == ("amaterasu.launch/1", "launch_export"))
            .count(),
        1
    );
    assert_eq!(
        broker
            .broker
            .actions()
            .iter()
            .filter(|action| **action == ("amaterasu.launch/1", "stop"))
            .count(),
        0
    );
    assert_eq!(broker.transports[&71]["lifecycle_intent"], "initial");
    assert_eq!(broker.transports[&72]["lifecycle_intent"], "join");
    let retry = controller.resume(12, request(2), &peer(2), now).unwrap();
    assert_eq!(retry.status, 200);
    assert!(String::from_utf8_lossy(&retry.body).contains("launch-72"));
    assert_eq!(controller.records.len(), 2);
}

#[test]
fn disconnecting_either_client_preserves_the_other_and_the_application() {
    for departing in [71, 72] {
        let (mut controller, mut broker, now) = two_client_application();
        broker.transports.get_mut(&departing).unwrap()["state"] = json!("closed");
        for tick in 1..=50 {
            controller.poll(now + Duration::from_millis(tick * 250), &mut |command| {
                broker.send(command)
            });
        }
        let remaining = controller
            .records
            .values()
            .find(|record| record.rtsp_ticket == Some(if departing == 71 { 72 } else { 71 }))
            .unwrap();
        assert_eq!(remaining.state, State::Ready);
        assert_eq!(
            controller.management_status(None)["launch"]["state"],
            "ready"
        );
        assert!(!broker.broker.application_stopped);
        assert!(!broker.broker.commands.iter().any(|command| matches!(command, Command::Request {service,payload,..} if service=="amaterasu.session-lifecycle/1" && payload["operation"]=="end_session")));
        assert_eq!(controller.current_game(&peer(3)), 1);
    }
}

#[test]
fn management_stop_requires_the_observed_application_membership() {
    let (mut controller, mut broker, now) = one_client_application();
    let observed = controller.management_status(None);
    let reference = observed["launch"]["reference"].as_str().unwrap();
    let stale = observed["launch"]["revision"].as_str().unwrap();
    assert!(controller.resume(11, request(2), &peer(2), now).is_none());
    assert_eq!(broker.reply(&mut controller, now).1.status, 200);
    controller.reply_delivered(11, now);
    assert_eq!(
        controller.management_stop(reference, stale, now)["code"],
        "launch_changed"
    );
    assert!(
        controller
            .records
            .values()
            .all(|record| record.state == State::Ready)
    );
    let current = controller.management_status(None);
    assert_eq!(
        controller.management_stop(
            reference,
            current["launch"]["revision"].as_str().unwrap(),
            now
        )["accepted"],
        true
    );
    for tick in 1..=100 {
        controller.poll(now + Duration::from_millis(tick * 50), &mut |command| {
            broker.send(command)
        });
        if controller
            .records
            .values()
            .all(|record| record.state.terminal())
        {
            break;
        }
    }
    assert!(
        controller
            .records
            .values()
            .all(|record| record.state.terminal())
    );
    assert!(broker.broker.application_stopped);
}

#[test]
fn explicit_quit_from_a_joined_client_ends_the_whole_application_after_transports() {
    let (mut controller, mut broker, now) = two_client_application();
    assert!(controller.cancel(12, &peer(2), now).is_none());
    let (connection, reply) = broker.reply(&mut controller, now);
    assert_eq!(connection, 12);
    assert_eq!(reply.status, 200);
    assert!(
        controller
            .records
            .values()
            .all(|record| record.state.terminal())
    );
    assert!(broker.broker.application_stopped);
    assert_eq!(controller.current_game(&peer(1)), 0);
    let requests: Vec<_> = broker
        .broker
        .commands
        .iter()
        .filter_map(|command| match command {
            Command::Request {
                service, payload, ..
            } => Some((service.as_str(), payload)),
            _ => None,
        })
        .collect();
    let stop_application = requests
        .iter()
        .position(|(service, payload)| {
            *service == "amaterasu.launch/1" && payload["action"] == "stop"
        })
        .unwrap();
    assert_eq!(
        requests[..stop_application]
            .iter()
            .filter(
                |(service, payload)| *service == "amaterasu.rtsp/1" && payload["action"] == "close"
            )
            .count(),
        2
    );
}

#[test]
fn revoking_a_client_retires_only_its_transport() {
    for marker in [1, 2] {
        let (mut controller, mut broker, now) = two_client_application();
        controller.revoke_untrusted(now, |certificate| certificate != vec![marker; 32]);
        for tick in 1..=50 {
            controller.poll(now + Duration::from_millis(tick * 250), &mut |command| {
                broker.send(command)
            });
        }
        let remaining = controller
            .records
            .values()
            .find(|record| record.peer.certificate == vec![3 - marker; 32])
            .unwrap();
        assert_eq!(remaining.state, State::Ready);
        assert!(!broker.broker.application_stopped);
    }
}

#[test]
fn client_monitoring_and_history_failures_do_not_end_the_shared_application() {
    for failure in ["status_rejected", "status_timeout", "history_rejected"] {
        let (mut controller, mut broker, now) = two_client_application();
        let mut injected = false;
        let mut held = None;
        if failure == "history_rejected" {
            broker.transports.get_mut(&71).unwrap()["state"] = json!("closed");
            broker.broker.history_release_unavailable = true;
        }
        for tick in 1..=100 {
            controller.poll(now + Duration::from_millis(tick * 250), &mut |command| {
                if !injected && failure != "history_rejected"
                    && matches!(&command, Command::Request { service, payload, .. }
                        if service=="amaterasu.rtsp/1" && payload["action"]=="status" && payload["ticket"]==71) {
                    injected = true;
                    if failure == "status_rejected" { return Ok(Response::error("unavailable", "fixture")); }
                    let response = broker.send(command)?;
                    if let Response::Accepted { id } = response { held = Some(id); }
                    return Ok(response);
                }
                if matches!(&command, Command::TakeResult {request} if Some(*request)==held) {
                    return Ok(Response::Pending);
                }
                if matches!(&command, Command::Cancel {request} if Some(*request)==held) { held = None; }
                broker.send(command)
            });
        }
        let sibling = controller
            .records
            .values()
            .find(|record| record.peer.certificate == vec![2; 32])
            .unwrap();
        assert_eq!(sibling.state, State::Ready, "{failure}");
        assert!(!broker.broker.application_stopped, "{failure}");
        assert!(
            !broker.broker.commands.iter().any(
                |command| matches!(command, Command::Request {service,payload,..}
            if service=="amaterasu.session-lifecycle/1" && payload["operation"]=="end_session")
            ),
            "{failure}"
        );
    }
}

#[test]
fn joined_client_reconnects_retire_metadata_without_forgetting_used_keys() {
    let (mut controller, mut broker, now) = two_client_application();
    let mut elapsed = 0;
    for marker in 3..=70 {
        let ticket = controller
            .records
            .values()
            .find(|record| record.application_session.is_some() && record.state == State::Ready)
            .unwrap()
            .rtsp_ticket
            .unwrap();
        broker.transports.get_mut(&ticket).unwrap()["state"] = json!("closed");
        for _ in 0..64 {
            elapsed += 50;
            controller.poll(now + Duration::from_millis(elapsed), &mut |command| {
                broker.send(command)
            });
            if controller
                .records
                .values()
                .all(|record| record.application_session.is_none() || record.state.terminal())
            {
                break;
            }
        }
        assert!(
            controller
                .records
                .values()
                .all(|record| record.application_session.is_none() || record.state.terminal()),
            "marker={marker} states={:?}",
            controller
                .records
                .values()
                .map(|record| (record.id, record.state, record.stage, record.cleanup_error))
                .collect::<Vec<_>>()
        );
        assert!(
            controller
                .resume(
                    100 + marker as u64,
                    request(marker),
                    &peer(2),
                    now + Duration::from_millis(elapsed)
                )
                .is_none()
        );
        let (connection, reply) =
            broker.reply(&mut controller, now + Duration::from_millis(elapsed));
        assert_eq!(
            reply.status,
            200,
            "marker={marker} {}",
            String::from_utf8_lossy(&reply.body)
        );
        controller.reply_delivered(connection, now + Duration::from_millis(elapsed));
        assert!(controller.records.len() <= MAX_RECORDS);
    }
    assert!(!controller.retired_stream_keys.is_empty());
    let replay = controller
        .resume(
            200,
            request(2),
            &peer(2),
            now + Duration::from_millis(elapsed),
        )
        .unwrap();
    assert_eq!(replay.status, 409);
    assert!(!broker.broker.application_stopped);
}

#[test]
fn root_resume_keeps_its_previous_transport_key_fenced() {
    let (mut controller, mut broker, now) = two_client_application();
    for transport in broker.transports.values_mut() {
        transport["state"] = json!("closed");
    }
    for tick in 1..=50 {
        controller.poll(now + Duration::from_millis(tick * 50), &mut |command| {
            broker.send(command)
        });
    }
    assert_eq!(controller.records[&1].state, State::Paused);
    let resumed_at = now + Duration::from_secs(3);
    assert!(
        controller
            .resume(12, request(3), &peer(1), resumed_at)
            .is_none()
    );
    assert_eq!(broker.reply(&mut controller, resumed_at).1.status, 200);
    controller.reply_delivered(12, resumed_at);
    let replay = controller
        .resume(13, request(1), &peer(2), resumed_at)
        .unwrap();
    assert_eq!(replay.status, 409);
    assert_eq!(
        controller.records[&1].request.remote_input_key,
        request(3).remote_input_key
    );
    assert!(!broker.broker.application_stopped);
}

#[test]
fn a_joined_client_can_confirm_application_exit_after_the_source_monitor_is_fenced() {
    let (mut controller, mut broker, now) = two_client_application();
    broker.transports.get_mut(&71).unwrap()["state"] = json!("closed");
    broker.broker.history_release_unavailable = true;
    for tick in 1..=50 {
        controller.poll(now + Duration::from_millis(tick * 250), &mut |command| {
            broker.send(command)
        });
    }
    assert_eq!(controller.records[&1].state, State::CleanupFailed);
    assert_eq!(controller.records[&2].state, State::Ready);
    broker.broker.history_release_unavailable = false;
    broker.broker.application_state = "stopped";
    for tick in 51..=150 {
        controller.poll(now + Duration::from_millis(tick * 250), &mut |command| {
            broker.send(command)
        });
        if controller
            .records
            .values()
            .all(|record| record.state.terminal())
        {
            break;
        }
    }
    assert!(
        controller
            .records
            .values()
            .all(|record| record.state.terminal())
    );
    assert_eq!(controller.current_game(&peer(2)), 0);
    assert!(broker.broker.commands.iter().any(
        |command| matches!(command, Command::Request {service,payload,..}
        if service=="amaterasu.session-lifecycle/1" && payload["operation"]=="end_session")
    ));
}

#[test]
fn stream_end_lifetime_policy_retires_the_owned_application() {
    for joined_client in [false, true] {
        let (mut controller, mut broker, now) = one_client_application_with_lifetime(true);
        if joined_client {
            assert!(controller.resume(11, request(2), &peer(2), now).is_none());
            assert_eq!(broker.reply(&mut controller, now).1.status, 200);
            controller.reply_delivered(11, now);
        }
        broker.transports.get_mut(&71).unwrap()["state"] = json!("closed");
        for tick in 1..=100 {
            controller.poll(now + Duration::from_millis(tick * 100), &mut |command| {
                broker.send(command)
            });
            if controller
                .records
                .values()
                .all(|record| record.state.terminal())
            {
                break;
            }
        }
        assert!(broker.broker.application_stopped);
        assert!(
            controller
                .records
                .values()
                .all(|record| record.state.terminal())
        );
        assert!(
            broker
                .transports
                .values()
                .all(|transport| transport["state"] == "closed")
        );
        assert_eq!(controller.current_game(&peer(1)), 0);
        let operations: Vec<_> = broker
            .broker
            .commands
            .iter()
            .filter_map(|command| match command {
                Command::Request {
                    service, payload, ..
                } if service == "amaterasu.session-lifecycle/1" => payload["operation"].as_str(),
                _ => None,
            })
            .collect();
        assert!(operations.contains(&"end_session"));
        assert!(operations.contains(&"application_retired"));
        if joined_client {
            let close_joined = broker.broker.commands.iter().position(|command| matches!(command,
                Command::Request {service, payload, ..}
                if service == "amaterasu.rtsp/1" && payload["action"] == "close" && payload["ticket"] == 72)).unwrap();
            let stop_application = broker
                .broker
                .commands
                .iter()
                .position(|command| {
                    matches!(command,
                Command::Request {service, payload, ..}
                if service == "amaterasu.launch/1" && payload["action"] == "stop")
                })
                .unwrap();
            assert!(close_joined < stop_application);
        }
    }
}

#[test]
fn closing_transport_keeps_its_application_lifetime_policy_until_retirement() {
    for stop_on_stream_end in [false, true] {
        let (mut controller, mut broker, now) =
            one_client_application_with_lifetime(stop_on_stream_end);
        broker.transports.get_mut(&71).unwrap()["state"] = json!("closing");
        let mut observed_closing = false;
        for tick in 1..=20 {
            controller.poll(now + Duration::from_millis(tick * 100), &mut |command| {
                if matches!(&command, Command::Request {service, payload, ..}
                    if service == "amaterasu.rtsp/1" && payload["action"] == "status" && payload["ticket"] == 71)
                    && broker.transports[&71]["state"] == "closing"
                {
                    observed_closing = true;
                }
                broker.send(command)
            });
        }
        assert!(observed_closing);
        assert_eq!(controller.records[&1].state, State::Ready);
        assert!(controller.records[&1].failure.is_none());
        assert!(!broker.broker.application_stopped);
        broker.transports.get_mut(&71).unwrap()["state"] = json!("closed");
        for tick in 21..=150 {
            controller.poll(now + Duration::from_millis(tick * 100), &mut |command| {
                broker.send(command)
            });
            if controller.records[&1].state.terminal()
                || controller.records[&1].state == State::Paused
            {
                break;
            }
        }
        assert_eq!(broker.broker.application_stopped, stop_on_stream_end);
        let record = &controller.records[&1];
        if stop_on_stream_end {
            assert_eq!(record.state, State::Stopped, "{:?}", record.failure);
        } else {
            assert_eq!(record.state, State::Paused, "{:?}", record.cleanup_error);
            assert!(record.failure.is_none());
        }
    }
}

#[test]
fn stream_end_policy_waits_for_a_confirmed_retirement_before_stopping_application() {
    for stop_on_stream_end in [false, true] {
        let (mut controller, mut broker, now) =
            one_client_application_with_lifetime(stop_on_stream_end);
        broker.transports.get_mut(&71).unwrap()["state"] = json!("closed");
        let mut injected = false;
        let mut observed_close = false;
        for tick in 1..=150 {
            controller.poll(now + Duration::from_millis(tick * 100), &mut |command| {
                let status = matches!(&command, Command::Request {service, payload, ..}
                    if service == "amaterasu.rtsp/1" && payload["action"] == "status" && payload["ticket"] == 71);
                let close = matches!(&command, Command::Request {service, payload, ..}
                    if service == "amaterasu.rtsp/1" && payload["action"] == "close" && payload["ticket"] == 71);
                if close {
                    assert!(!broker.broker.application_stopped);
                    observed_close = true;
                }
                let response = broker.send(command)?;
                if status && !injected && let Response::Accepted { id } = response {
                    injected = true;
                    let payload = broker.broker.results.get_mut(&id).unwrap();
                    payload["lifecycle_retired"] = json!(false);
                    payload["lifecycle_consumer_retired"] = json!(false);
                    payload["lifecycle_cleanup_state"] = json!("retirement_pending");
                }
                Ok(response)
            });
            if injected
                && observed_close
                && (controller.records[&1].state.terminal()
                    || controller.records[&1].state == State::Paused)
            {
                break;
            }
        }
        assert!(injected && observed_close);
        assert_eq!(broker.broker.application_stopped, stop_on_stream_end);
        if stop_on_stream_end {
            assert!(controller.records[&1].state.terminal());
        } else {
            assert_eq!(controller.records[&1].state, State::Paused);
        }
    }
}

#[test]
fn authenticated_disconnect_is_recorded_only_from_the_retirement_receipt() {
    let (mut controller, mut broker, now) = one_client_application();
    broker.transports.get_mut(&71).unwrap()["state"] = json!("closed");
    let mut unretired = broker.terminal_transport(71);
    unretired["lifecycle_retired"] = json!(false);
    unretired["lifecycle_consumer_retired"] = json!(false);
    unretired["lifecycle_cleanup_state"] = json!("retirement_pending");
    unretired["authenticated_disconnect_reason"] = json!("peer_disconnected");
    let job = |phase| workflow::Job {
        record: 1,
        phase,
        call: call::Call::new("unused", json!({}), "unused".into(), 1000),
        deadline: now + Duration::from_secs(5),
    };
    controller.consume(
        &job(workflow::Phase::MonitorRtsp),
        unretired,
        now,
        &mut Vec::new(),
    );
    assert!(!controller.records[&1].authenticated_client_disconnected);
    let mut retired = broker.terminal_transport(71);
    retired["authenticated_disconnect_reason"] = json!("peer_disconnected");
    controller.consume(
        &job(workflow::Phase::StopRtsp),
        retired,
        now,
        &mut Vec::new(),
    );
    assert!(controller.records[&1].authenticated_client_disconnected);
}

#[test]
fn recovered_transport_gets_a_fresh_closing_deadline() {
    let (mut controller, broker, now) = one_client_application();
    let job = workflow::Job {
        record: 1,
        phase: workflow::Phase::MonitorRtsp,
        call: call::Call::new("unused", json!({}), "unused".into(), 1000),
        deadline: now + Duration::from_secs(5),
    };
    let mut response = broker.transports[&71].clone();
    response["state"] = json!("closing");
    controller.consume(&job, response.clone(), now, &mut Vec::new());
    let first_deadline = controller.records[&1].rtsp_closing_deadline.unwrap();
    response["state"] = json!("playing");
    controller.consume(
        &job,
        response.clone(),
        now + Duration::from_secs(1),
        &mut Vec::new(),
    );
    assert_eq!(controller.records[&1].rtsp_closing_deadline, None);
    response["state"] = json!("closing");
    controller.consume(
        &job,
        response,
        first_deadline + Duration::from_secs(1),
        &mut Vec::new(),
    );
    assert_eq!(controller.records[&1].state, State::Ready);
    assert!(controller.records[&1].rtsp_closing_deadline.unwrap() > first_deadline);
}

#[test]
fn joined_transport_never_owns_application_cleanup_with_stream_end_policy() {
    let (mut controller, mut broker, now) = one_client_application_with_lifetime(true);
    assert!(controller.resume(11, request(2), &peer(2), now).is_none());
    assert_eq!(broker.reply(&mut controller, now).1.status, 200);
    controller.reply_delivered(11, now);
    broker.transports.get_mut(&72).unwrap()["state"] = json!("closed");
    for tick in 1..=100 {
        controller.poll(now + Duration::from_millis(tick * 100), &mut |command| {
            broker.send(command)
        });
    }
    let application = controller
        .records
        .values()
        .find(|record| record.application_session.is_none())
        .unwrap();
    let joined = controller
        .records
        .values()
        .find(|record| record.application_session.is_some())
        .unwrap();
    assert_eq!(application.state, State::Ready);
    assert!(joined.state.terminal());
    assert!(!broker.broker.application_stopped);
    assert_eq!(controller.current_game(&peer(1)), 1);
}

#[test]
fn unrelated_paired_client_can_end_all_shared_streams_and_launch_again() {
    let (mut controller, mut broker, now) = two_client_application();
    assert!(controller.cancel(80, &peer(9), now).is_none());
    let (connection, reply) = broker.reply(&mut controller, now);
    assert_eq!(connection, 80);
    assert_eq!(reply.status, 200);
    assert!(controller.records.values().all(|record| record.state.terminal()));
    assert!(broker.transports.values().all(|transport| transport["state"] == "closed"));
    assert!(controller.launch(81, request(9), peer(9), now).is_none());
    let (connection, reply) = broker.reply(&mut controller, now);
    assert_eq!(connection, 81);
    assert_eq!(reply.status, 200);
}

#[test]
fn host_shutdown_drains_a_paused_application_before_the_host_deadline() {
    let (mut controller, mut broker, now) = one_client_application();
    // The client vanished: its stream failed and the application is paused.
    let stream = broker.transports.get_mut(&71).unwrap();
    stream["state"] = json!("failed");
    stream["failure"] = json!("peer_disconnected");
    for tick in 1..=200 {
        controller.poll(now + Duration::from_millis(tick * 50), &mut |command| broker.send(command));
    }
    let paused = now + Duration::from_secs(10);
    assert!(controller.records.values().any(|record| record.state == State::Paused), "{:?}", controller.records.values().map(|record| (record.state, record.stage)).collect::<Vec<_>>());
    let mut drained_at = None;
    for tick in 0..=400 {
        let at = paused + Duration::from_millis(tick * 50);
        controller.poll(at, &mut |command| broker.send(command));
        if controller.quiesce(at) {
            drained_at = Some(tick * 50);
            break;
        }
    }
    let states: Vec<_> = controller.records.values().map(|record| (record.state, record.stage)).collect();
    let drained_at = drained_at.unwrap_or_else(|| panic!("shutdown never drained: {states:?}"));
    assert!(drained_at < 15_000, "drain took {drained_at} ms");
    assert!(broker.broker.application_stopped);
}

#[test]
fn private_catalog_cannot_be_shared_by_clients_with_either_preference() {
    for (retained_private, joining_private) in [(true, false), (true, true), (false, true)] {
        let (mut controller, mut broker, now) = one_client_application();
        controller.records.values_mut().next().unwrap().peer.client_properties["requested_resolution_only"] = json!(retained_private);
        let mut joining = peer(2);
        joining.client_properties["requested_resolution_only"] = json!(joining_private);
        let command_count = broker.broker.commands.len();
        let reply = controller.resume(11, request(2), &joining, now).unwrap();
        assert_eq!(reply.status, 409);
        assert_eq!(controller.records.len(), 1);
        assert_eq!(broker.broker.commands.len(), command_count);
        assert!(!broker.broker.application_stopped);
    }
}

#[test]
fn paused_private_catalog_rejects_other_owners_and_catalog_changes_before_effects() {
    for case in ["other_owner", "opt_out", "mode_change", "opt_in"] {
        let (mut controller, mut broker, now) = one_client_application();
        let root = controller.records.values_mut().next().unwrap();
        root.state = State::Paused;
        root.peer.client_properties["requested_resolution_only"] = json!(case != "opt_in");
        let mut joining = peer(if case == "other_owner" { 2 } else { 1 });
        joining.client_properties["requested_resolution_only"] = json!(case != "opt_out");
        let mut stream = request(3);
        if case == "mode_change" { stream.width = 1920; }
        let command_count = broker.broker.commands.len();
        let reply = controller.resume(11, stream, &joining, now).unwrap();
        assert_eq!(reply.status, 409, "{case}");
        assert_eq!(controller.records.values().next().unwrap().state, State::Paused);
        assert_eq!(broker.broker.commands.len(), command_count);
        assert!(!broker.broker.application_stopped);
    }
}
