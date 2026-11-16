use super::tests::{Endpoint, client, record};
use super::*;

#[test]
fn slow_queued_startup_observation_does_not_stop_the_playing_stream() {
    let mut controller = Controller::new(Settings::default()).unwrap();
    let mut endpoint = Endpoint {
        verifier_pending: true,
        deny_settlement: true,
        ..Default::default()
    };
    let runtime = client(&mut endpoint);
    let mut stream = record(State::Playing);
    stream.next_status = 0;
    controller.schedule_status(&stream, None, None, 0);
    controller.records.insert(stream.id, stream);
    controller.advance_job(&runtime, 0);
    controller.advance_job(&runtime, 5_001);
    assert!(controller.job.is_none());
    assert_eq!(controller.late_observations.len(), 1);
    for now in 5_002..6_000 {
        controller.advance_late_observations(&runtime);
        controller.schedule_background(now);
        assert_eq!(controller.late_observations.len(), 1);
        assert!(controller.job.is_none());
        assert!(controller.records[&5].state == State::Playing);
        assert!(controller.records[&5].failure.is_none());
    }
    assert!(!endpoint.commands.iter().any(|command| matches!(command,
        Command::Request { payload, .. } if payload["action"] == "stop")));
    endpoint.deny_settlement = false;
    controller.advance_late_observations(&runtime);
    assert!(controller.late_observations.is_empty());
    endpoint.verifier_pending = false;
    endpoint.verifier_reply =
        Some(json!({"ok":true,"session":23,"state":"active","cleanup_error":null}));
    controller.schedule_background(6_001);
    for now in 6_002..6_010 {
        controller.advance_job(&runtime, now);
    }
    assert!(controller.records[&5].state == State::Playing);
    assert!(controller.records[&5].observation_stale_at >= 36_001);
}

#[test]
fn unavailable_observation_is_bounded_and_cannot_keep_a_stream_alive_forever() {
    let mut controller = Controller::new(Settings::default()).unwrap();
    let mut endpoint = Endpoint {
        verifier_pending: true,
        deny_settlement: true,
        ..Default::default()
    };
    let runtime = client(&mut endpoint);
    let mut stream = record(State::Playing);
    stream.next_status = 0;
    controller.schedule_status(&stream, None, None, 0);
    controller.records.insert(stream.id, stream);
    controller.advance_job(&runtime, 0);
    controller.advance_job(&runtime, 5_001);
    controller.schedule_background(OBSERVATION_STALE_MS - 1);
    assert!(controller.records[&5].state == State::Playing);
    controller.schedule_background(OBSERVATION_STALE_MS);
    assert!(controller.records[&5].state == State::Closing);
    assert_eq!(
        controller.records[&5].failure,
        Some("stream_observation_lost")
    );
    assert_eq!(controller.late_observations.len(), 1);
}
