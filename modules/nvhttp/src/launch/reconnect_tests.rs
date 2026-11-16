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
