use crate::{
    DeviceSink, GrantRegistry, InputError, InputEvent, InputPacket, InputResult, InputSession,
    MouseButton, PROTOCOL_VERSION, PeerIdentity,
};
use std::time::{Duration, Instant};

#[derive(Default)]
struct FakeDevice {
    sent: Vec<InputEvent>,
    releases: usize,
    fail_next: bool,
    fail_release: bool,
    heartbeats: usize,
    fail_heartbeat: bool,
}

impl DeviceSink for FakeDevice {
    fn submit(&mut self, event: &InputEvent) -> InputResult<()> {
        self.sent.push(event.clone());
        if std::mem::take(&mut self.fail_next) {
            return Err(InputError::Device);
        }
        Ok(())
    }

    fn release_all(&mut self) -> InputResult<()> {
        self.releases += 1;
        if self.fail_release {
            return Err(InputError::Device);
        }
        Ok(())
    }

    fn keep_alive(&mut self) -> InputResult<()> {
        self.heartbeats += 1;
        if self.fail_heartbeat {
            return Err(InputError::Device);
        }
        Ok(())
    }
}

fn peer() -> PeerIdentity {
    PeerIdentity::from_verified_connection("S-1-5-21-test".into(), 3, "a".repeat(64)).unwrap()
}

fn connect(now: Instant, device: FakeDevice) -> InputSession<FakeDevice> {
    let identity = peer();
    let grant = GrantRegistry::synthetic()
        .issue(
            identity.clone(),
            "a".repeat(32),
            7,
            now + Duration::from_secs(1),
            true,
            true,
        )
        .unwrap();
    InputSession::connect(&identity, grant, device, now).unwrap()
}

fn packet(sequence: u64, event: InputEvent) -> InputPacket {
    InputPacket {
        version: PROTOCOL_VERSION,
        generation: 7,
        sequence,
        lease_token: "a".repeat(32),
        event,
    }
}

fn key(pressed: bool) -> InputEvent {
    InputEvent::Key {
        scan_code: 30,
        extended: false,
        pressed,
    }
}

#[test]
fn transport_peer_must_match_grant_without_a_packet_caller_field() {
    let now = Instant::now();
    let mut registry = GrantRegistry::synthetic();
    let grant = registry
        .issue(
            peer(),
            "a".repeat(32),
            7,
            now + Duration::from_secs(1),
            true,
            true,
        )
        .unwrap();
    let other_session =
        PeerIdentity::from_verified_connection("S-1-5-21-test".into(), 4, "a".repeat(64)).unwrap();
    assert_eq!(
        InputSession::connect(&other_session, grant, FakeDevice::default(), now)
            .err()
            .unwrap(),
        InputError::PermissionDenied
    );
}

#[test]
fn grant_generation_is_single_use_and_monotonic_per_peer() {
    let now = Instant::now();
    let mut registry = GrantRegistry::synthetic();
    let identity = peer();
    registry
        .issue(
            identity.clone(),
            "a".repeat(32),
            7,
            now + Duration::from_secs(1),
            true,
            true,
        )
        .unwrap();
    assert_eq!(
        registry
            .issue(
                identity.clone(),
                "b".repeat(32),
                7,
                now + Duration::from_secs(1),
                true,
                true
            )
            .err()
            .unwrap(),
        InputError::InvalidSequence
    );
    assert_eq!(
        registry
            .issue(
                identity.clone(),
                "b".repeat(32),
                6,
                now + Duration::from_secs(1),
                true,
                true
            )
            .err()
            .unwrap(),
        InputError::InvalidSequence
    );
    registry
        .issue(
            identity,
            "b".repeat(32),
            8,
            now + Duration::from_secs(1),
            true,
            true,
        )
        .unwrap();
}

#[test]
fn protected_generation_floor_survives_restart_and_rejects_corruption() {
    let fixture = std::env::temp_dir().join(format!(
        "amaterasu-input-floor-{}",
        amaterasu_storage::random_identifier().unwrap()
    ));
    let now = Instant::now();
    let identity = peer();
    let mut registry = GrantRegistry::open_protected(&fixture).unwrap();
    let (generation, _) = registry
        .issue_next(
            identity.clone(),
            "a".repeat(32),
            now + Duration::from_secs(1),
            true,
            true,
        )
        .unwrap();
    assert_eq!(generation, 1);
    drop(registry);
    let mut reopened = GrantRegistry::open_protected(&fixture).unwrap();
    assert_eq!(
        reopened
            .issue(
                identity.clone(),
                "b".repeat(32),
                1,
                now + Duration::from_secs(1),
                true,
                true
            )
            .err(),
        Some(InputError::InvalidSequence)
    );
    assert_eq!(
        reopened
            .issue_next(
                identity,
                "b".repeat(32),
                now + Duration::from_secs(1),
                true,
                true,
            )
            .unwrap()
            .0,
        2
    );
    std::fs::write(fixture.join("input-generation-floors.json"), b"{}").unwrap();
    assert!(matches!(
        GrantRegistry::open_protected(&fixture),
        Err(InputError::Storage)
    ));
    let canonical = fixture.canonicalize().unwrap();
    assert!(canonical.starts_with(std::env::temp_dir().canonicalize().unwrap()));
    std::fs::remove_dir_all(canonical).unwrap();
}

#[test]
fn a_new_grant_waits_for_the_old_device_lease_to_neutralize() {
    let now = Instant::now();
    let mut registry = GrantRegistry::synthetic();
    let identity = peer();
    let first = registry
        .issue(
            identity.clone(),
            "a".repeat(32),
            7,
            now + Duration::from_secs(1),
            true,
            true,
        )
        .unwrap();
    let mut old = InputSession::connect(&identity, first, FakeDevice::default(), now).unwrap();
    old.submit(&packet(1, key(true)), now).unwrap();
    assert_eq!(
        registry
            .issue(
                identity.clone(),
                "b".repeat(32),
                8,
                now + Duration::from_secs(1),
                true,
                true
            )
            .err()
            .unwrap(),
        InputError::ResourceExhausted
    );
    let other_peer =
        PeerIdentity::from_verified_connection("S-1-5-21-other".into(), 4, "b".repeat(64)).unwrap();
    assert_eq!(
        registry
            .issue(
                other_peer,
                "c".repeat(32),
                1,
                now + Duration::from_secs(1),
                true,
                true
            )
            .err()
            .unwrap(),
        InputError::ResourceExhausted
    );
    old.revoke().unwrap();
    assert_eq!(old.sink.releases, 1);
    registry
        .issue(
            identity,
            "b".repeat(32),
            8,
            now + Duration::from_secs(1),
            true,
            true,
        )
        .unwrap();
    assert_eq!(
        old.submit(&packet(2, key(true)), now),
        Err(InputError::Closed)
    );
}

#[test]
fn packet_lease_generation_and_sequence_are_checked_before_device_access() {
    let now = Instant::now();
    let mut session = connect(now, FakeDevice::default());
    let mut invalid = packet(1, key(true));
    invalid.lease_token = "b".repeat(32);
    assert_eq!(
        session.submit(&invalid, now),
        Err(InputError::PermissionDenied)
    );
    invalid.lease_token = "a".repeat(32);
    invalid.generation = 8;
    assert_eq!(
        session.submit(&invalid, now),
        Err(InputError::PermissionDenied)
    );
    assert_eq!(
        session.submit(&packet(2, key(true)), now),
        Err(InputError::InvalidSequence)
    );
    session.submit(&packet(1, key(true)), now).unwrap();
    assert_eq!(
        session.submit(&packet(1, key(true)), now),
        Err(InputError::InvalidSequence)
    );
    assert_eq!(session.sink.sent.len(), 1);
}

#[test]
fn invalid_events_and_device_grants_do_not_reach_the_sink() {
    let now = Instant::now();
    let identity = peer();
    let grant = GrantRegistry::synthetic()
        .issue(
            identity.clone(),
            "a".repeat(32),
            7,
            now + Duration::from_secs(1),
            true,
            false,
        )
        .unwrap();
    let mut session = InputSession::connect(&identity, grant, FakeDevice::default(), now).unwrap();
    for event in [
        InputEvent::Key {
            scan_code: 0,
            extended: false,
            pressed: true,
        },
        InputEvent::EvdevKey {
            code: 0,
            pressed: true,
        },
        InputEvent::EvdevKey {
            code: 240,
            pressed: true,
        },
        InputEvent::Relative { dx: 4, dy: 0 },
        InputEvent::Scroll {
            vertical: 12_001,
            horizontal: 0,
        },
    ] {
        assert!(session.submit(&packet(1, event), now).is_err());
    }
    assert!(session.sink.sent.is_empty());
    assert_eq!(
        session.submit(
            &packet(
                1,
                InputEvent::MouseButton {
                    button: MouseButton::Left,
                    pressed: true,
                },
            ),
            now,
        ),
        Err(InputError::PermissionDenied)
    );
    assert!(session.sink.sent.is_empty());
    assert!(serde_json::from_str::<InputPacket>(r#"{"version":1,"generation":7,"sequence":1,"lease_token":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","event":{"kind":"raw_report","report_id":1}}"#).is_err());
}

#[test]
fn uncertain_delivery_closes_and_neutralizes_before_another_event() {
    let now = Instant::now();
    let device = FakeDevice {
        fail_next: true,
        ..Default::default()
    };
    let mut session = connect(now, device);
    assert_eq!(
        session.submit(&packet(1, key(true)), now),
        Err(InputError::Device)
    );
    let delivered_before_retry = session.sink.sent.len();
    assert_eq!(
        session.submit(&packet(2, key(true)), now),
        Err(InputError::Closed)
    );
    assert_eq!(session.sink.sent.len(), delivered_before_retry);
    assert_eq!(session.sink.releases, 1);
}

#[test]
fn companion_tick_heartbeats_live_lease_and_closes_on_watchdog_failure() {
    let now = Instant::now();
    let mut session = connect(now, FakeDevice::default());
    assert_eq!(
        session.poll_expiry(now + Duration::from_millis(100)),
        Ok(false)
    );
    assert_eq!(session.sink.heartbeats, 1);
    session.sink.fail_heartbeat = true;
    assert_eq!(
        session.poll_expiry(now + Duration::from_millis(200)),
        Err(InputError::Device)
    );
    assert_eq!(session.sink.releases, 1);
    assert_eq!(
        session.submit(&packet(1, key(true)), now),
        Err(InputError::Closed)
    );
}

#[test]
fn already_removed_vhf_child_can_release_lease_for_new_generation() {
    let now = Instant::now();
    let identity = peer();
    let mut registry = GrantRegistry::synthetic();
    let grant = registry
        .issue(
            identity.clone(),
            "a".repeat(32),
            7,
            now + Duration::from_secs(1),
            true,
            true,
        )
        .unwrap();
    let mut session = InputSession::connect(
        &identity,
        grant,
        FakeDevice {
            // The driver removed the child on its own watchdog. A subsequent
            // heartbeat fails, while idempotent DESTROY succeeds.
            fail_heartbeat: true,
            ..Default::default()
        },
        now,
    )
    .unwrap();
    assert_eq!(
        session.poll_expiry(now + Duration::from_millis(100)),
        Err(InputError::Device)
    );
    assert_eq!(session.sink.releases, 1);
    let next = registry
        .issue(
            identity.clone(),
            "b".repeat(32),
            8,
            now + Duration::from_secs(1),
            true,
            true,
        )
        .unwrap();
    let replacement = InputSession::connect(&identity, next, FakeDevice::default(), now).unwrap();
    assert!(!replacement.cleanup_required());
}

#[test]
fn disconnect_releases_held_keys_and_mouse_buttons_in_reverse_order() {
    let now = Instant::now();
    let mut session = connect(now, FakeDevice::default());
    session.submit(&packet(1, key(true)), now).unwrap();
    session
        .submit(
            &packet(
                2,
                InputEvent::MouseButton {
                    button: MouseButton::Left,
                    pressed: true,
                },
            ),
            now,
        )
        .unwrap();
    session.revoke().unwrap();
    assert_eq!(
        session.sink.sent[2],
        InputEvent::MouseButton {
            button: MouseButton::Left,
            pressed: false
        }
    );
    assert_eq!(session.sink.sent[3], key(false));
    assert_eq!(session.sink.releases, 1);
    assert_eq!(
        session.submit(&packet(3, key(true)), now),
        Err(InputError::Closed)
    );
}

#[test]
fn idle_expiry_releases_without_another_worker_packet() {
    let now = Instant::now();
    let mut session = connect(now, FakeDevice::default());
    session.submit(&packet(1, key(true)), now).unwrap();
    assert!(!session.poll_expiry(now).unwrap());
    assert!(session.poll_expiry(now + Duration::from_secs(2)).unwrap());
    assert_eq!(session.sink.releases, 1);
    assert_eq!(
        session.submit(&packet(2, key(true)), now),
        Err(InputError::Closed)
    );
}

#[test]
fn current_host_permit_can_extend_a_live_session_without_replaying_a_generation() {
    let now = Instant::now();
    let mut session = connect(now, FakeDevice::default());
    session.submit(&packet(1, key(true)), now).unwrap();
    session
        .refresh_host_authorized_expiry(now + Duration::from_millis(900))
        .unwrap();
    assert!(!session.poll_expiry(now + Duration::from_secs(2)).unwrap());
    session
        .submit(&packet(2, key(false)), now + Duration::from_secs(2))
        .unwrap();
    session.revoke().unwrap();
    assert_eq!(
        session.refresh_host_authorized_expiry(now + Duration::from_secs(3)),
        Err(InputError::Closed)
    );
}

#[test]
fn failed_release_retains_cleanup_for_retry() {
    let now = Instant::now();
    let mut session = connect(now, FakeDevice::default());
    session.submit(&packet(1, key(true)), now).unwrap();
    session.sink.fail_next = true;
    session.sink.fail_release = true;
    assert_eq!(session.revoke(), Err(InputError::Device));
    assert!(session.cleanup_required());
    session.sink.fail_release = false;
    assert!(session.poll_expiry(now).unwrap());
    assert!(!session.cleanup_required());
}

#[test]
fn a_failed_device_release_blocks_all_replacement_grants() {
    let now = Instant::now();
    let mut registry = GrantRegistry::synthetic();
    let identity = peer();
    let grant = registry
        .issue(
            identity.clone(),
            "a".repeat(32),
            7,
            now + Duration::from_secs(1),
            true,
            true,
        )
        .unwrap();
    let mut session = InputSession::connect(&identity, grant, FakeDevice::default(), now).unwrap();
    session.submit(&packet(1, key(true)), now).unwrap();
    session.sink.fail_release = true;
    assert_eq!(session.revoke(), Err(InputError::Device));
    assert_eq!(
        registry
            .issue(
                identity.clone(),
                "b".repeat(32),
                8,
                now + Duration::from_secs(1),
                true,
                true
            )
            .err()
            .unwrap(),
        InputError::ResourceExhausted
    );
    session.sink.fail_release = false;
    assert!(session.poll_expiry(now).unwrap());
    registry
        .issue(
            identity,
            "b".repeat(32),
            8,
            now + Duration::from_secs(1),
            true,
            true,
        )
        .unwrap();
}

#[test]
fn rate_limit_never_blocks_release_of_a_held_key() {
    let now = Instant::now();
    let mut session = connect(now, FakeDevice::default());
    session.submit(&packet(1, key(true)), now).unwrap();
    for sequence in 2..=2048 {
        session
            .submit(
                &packet(sequence, InputEvent::Relative { dx: 1, dy: 0 }),
                now,
            )
            .unwrap();
    }
    assert_eq!(
        session.submit(&packet(2049, InputEvent::Relative { dx: 1, dy: 0 }), now),
        Err(InputError::ResourceExhausted)
    );
    session.submit(&packet(2049, key(false)), now).unwrap();
}
