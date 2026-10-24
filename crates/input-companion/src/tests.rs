use crate::{
    DeviceSink, GamepadFeedback, GrantRegistry, InputError, InputEvent, InputPacket, InputResult,
    InputSession, MouseButton, PROTOCOL_VERSION, PeerIdentity, PermitLedger, ProcessIdentity,
};
use std::collections::VecDeque;
use std::time::{Duration, Instant};

#[derive(Default)]
struct FakeDevice {
    sent: Vec<InputEvent>,
    releases: usize,
    fail_next: bool,
    fail_release: bool,
    heartbeats: usize,
    fail_heartbeat: bool,
    feedback: VecDeque<GamepadFeedback>,
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
        self.feedback.clear();
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

    fn poll_gamepad_feedback(&mut self) -> InputResult<Option<GamepadFeedback>> {
        Ok(self.feedback.pop_front())
    }
}

#[cfg(target_os = "linux")]
#[test]
fn feedback_poll_is_bound_to_the_current_gamepad_grant_and_revoke_clears_it() {
    let now = Instant::now();
    let mut ungranted = connect(
        now,
        FakeDevice {
            feedback: VecDeque::from([GamepadFeedback {
                slot: 0,
                strong_motor: 1,
                weak_motor: 1,
                duration_ms: 1,
            }]),
            ..FakeDevice::default()
        },
    );
    assert_eq!(
        ungranted.poll_gamepad_feedback(7, &"a".repeat(32)),
        Err(InputError::PermissionDenied)
    );
    ungranted.revoke().unwrap();

    let identity = peer();
    let feedback = GamepadFeedback {
        slot: 2,
        strong_motor: u16::MAX,
        weak_motor: 17,
        duration_ms: 10_000,
    };
    let mut registry = GrantRegistry::synthetic();
    let grant = registry
        .issue_with_capabilities(
            identity.clone(),
            "a".repeat(32),
            7,
            now + Duration::from_secs(1),
            false,
            false,
            true,
        )
        .unwrap();
    let mut session = InputSession::connect(
        &identity,
        grant,
        FakeDevice {
            feedback: VecDeque::from([feedback]),
            ..FakeDevice::default()
        },
        now,
    )
    .unwrap();

    assert_eq!(
        session.poll_gamepad_feedback(7, &"a".repeat(32)),
        Ok(Some(feedback))
    );
    assert_eq!(session.poll_gamepad_feedback(7, &"a".repeat(32)), Ok(None));
    assert_eq!(
        session.poll_gamepad_feedback(8, &"a".repeat(32)),
        Err(InputError::PermissionDenied)
    );
    assert_eq!(
        session.poll_gamepad_feedback(7, &"b".repeat(32)),
        Err(InputError::PermissionDenied)
    );
    session.revoke().unwrap();
    assert_eq!(
        session.poll_gamepad_feedback(7, &"a".repeat(32)),
        Err(InputError::PermissionDenied)
    );

    let replacement = registry
        .issue_with_capabilities(
            identity.clone(),
            "c".repeat(32),
            8,
            now + Duration::from_secs(1),
            false,
            false,
            true,
        )
        .unwrap();
    let mut replacement_session =
        InputSession::connect(&identity, replacement, FakeDevice::default(), now).unwrap();
    assert_eq!(
        replacement_session.poll_gamepad_feedback(8, &"c".repeat(32)),
        Ok(None)
    );
    replacement_session.revoke().unwrap();
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

fn gamepad_event() -> InputEvent {
    InputEvent::Gamepad {
        number: 0,
        active_mask: 1,
        buttons: 1 << 12,
        left_trigger: 0,
        right_trigger: 0,
        left_x: i16::MIN,
        left_y: i16::MAX,
        right_x: 0,
        right_y: 0,
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
        InputEvent::EvdevKeyRepeat { code: 0 },
        InputEvent::EvdevKeyRepeat { code: 240 },
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
    assert_eq!(
        session.submit(&packet(1, InputEvent::Absolute { x: u16::MAX, y: 0 }), now,),
        Err(InputError::PermissionDenied)
    );
    assert!(session.sink.sent.is_empty());
    assert!(serde_json::from_str::<InputEvent>(r#"{"kind":"absolute","x":65536,"y":0}"#).is_err());
    assert!(serde_json::from_str::<InputEvent>(r#"{"kind":"absolute","x":0,"y":-1}"#).is_err());
    assert!(serde_json::from_str::<InputPacket>(r#"{"version":1,"generation":7,"sequence":1,"lease_token":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","event":{"kind":"raw_report","report_id":1}}"#).is_err());
}

#[test]
fn absolute_pointer_uses_mouse_grant_without_adding_held_cleanup() {
    let now = Instant::now();
    let mut session = connect(now, FakeDevice::default());
    let absolute = InputEvent::Absolute { x: u16::MAX, y: 0 };

    assert_eq!(absolute.transition(), None);
    session.submit(&packet(1, absolute.clone()), now).unwrap();
    assert_eq!(
        session.sink.sent.as_slice(),
        std::slice::from_ref(&absolute)
    );
    session.revoke().unwrap();

    assert_eq!(session.sink.sent, [absolute]);
    assert_eq!(session.sink.releases, 1);
}

#[test]
fn evdev_repeat_requires_an_owned_key_and_does_not_change_cleanup_state() {
    let now = Instant::now();
    let mut session = connect(now, FakeDevice::default());
    let repeat = InputEvent::EvdevKeyRepeat { code: 30 };

    assert_eq!(
        session.submit(&packet(1, repeat.clone()), now),
        Err(InputError::InvalidSequence)
    );
    assert!(session.sink.sent.is_empty());

    let down = InputEvent::EvdevKey {
        code: 30,
        pressed: true,
    };
    let up = InputEvent::EvdevKey {
        code: 30,
        pressed: false,
    };
    session.submit(&packet(1, down.clone()), now).unwrap();
    session.submit(&packet(2, repeat.clone()), now).unwrap();
    session.submit(&packet(3, up.clone()), now).unwrap();
    assert_eq!(
        session.submit(&packet(4, repeat.clone()), now),
        Err(InputError::InvalidSequence)
    );
    assert_eq!(session.sink.sent, [down, repeat, up]);
    session.revoke().unwrap();
    assert_eq!(session.sink.releases, 1);
}

#[test]
fn evdev_repeat_keeps_one_held_owner_until_disconnect_cleanup() {
    let now = Instant::now();
    let mut session = connect(now, FakeDevice::default());
    let down = InputEvent::EvdevKey {
        code: 30,
        pressed: true,
    };
    let repeat = InputEvent::EvdevKeyRepeat { code: 30 };
    let neutralizing_up = InputEvent::EvdevKey {
        code: 30,
        pressed: false,
    };
    session.submit(&packet(1, down.clone()), now).unwrap();
    session.submit(&packet(2, repeat.clone()), now).unwrap();

    session.revoke().unwrap();
    session.revoke().unwrap();
    assert_eq!(session.sink.sent, [down, repeat, neutralizing_up]);
    assert_eq!(session.sink.releases, 1);
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

#[test]
fn gamepad_requires_an_explicit_host_grant_and_revoke_releases_its_session_device() {
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
    let mut session = InputSession::connect(&identity, grant, FakeDevice::default(), now).unwrap();

    assert_eq!(
        session.submit(&packet(1, gamepad_event()), now),
        Err(InputError::PermissionDenied)
    );
    assert!(session.sink.sent.is_empty());

    let mut granted_registry = GrantRegistry::synthetic();
    let grant = granted_registry
        .issue_with_capabilities(
            identity.clone(),
            "a".repeat(32),
            7,
            now + Duration::from_secs(1),
            false,
            false,
            true,
        )
        .unwrap();
    let mut granted = InputSession::connect(&identity, grant, FakeDevice::default(), now).unwrap();
    granted.submit(&packet(1, gamepad_event()), now).unwrap();
    granted.revoke().unwrap();

    assert_eq!(granted.sink.sent, vec![gamepad_event()]);
    assert_eq!(granted.sink.releases, 1);
}

#[test]
fn gamepad_policy_change_revokes_an_existing_host_permit() {
    let now = Instant::now();
    let process = ProcessIdentity::new("uid:1000".into(), 3, 11, 12356).unwrap();
    let mut ledger = PermitLedger::new(GrantRegistry::synthetic());
    ledger
        .refresh_from_host_with_gamepad(process.clone(), "a".repeat(64), false, false, true, now)
        .unwrap();
    let lease = ledger.claim(&process, now).unwrap();
    assert!(lease.grant.gamepad_enabled());

    assert_eq!(
        ledger.refresh_from_host_with_gamepad(
            process.clone(),
            "a".repeat(64),
            false,
            false,
            false,
            now,
        ),
        Err(InputError::InvalidGrant)
    );
    assert!(!ledger.is_current(&process, now));
}

#[test]
fn failed_gamepad_report_closes_the_lease_and_runs_device_cleanup() {
    let now = Instant::now();
    let identity = peer();
    let grant = GrantRegistry::synthetic()
        .issue_with_capabilities(
            identity.clone(),
            "a".repeat(32),
            7,
            now + Duration::from_secs(1),
            true,
            true,
            true,
        )
        .unwrap();
    let device = FakeDevice {
        fail_next: true,
        ..FakeDevice::default()
    };
    let mut session = InputSession::connect(&identity, grant, device, now).unwrap();

    assert_eq!(
        session.submit(&packet(1, gamepad_event()), now),
        Err(InputError::Device)
    );
    assert!(session.sink.sent.contains(&gamepad_event()));
    assert_eq!(session.sink.releases, 1);
    assert!(!session.cleanup_required());
    assert_eq!(
        session.submit(&packet(2, gamepad_event()), now),
        Err(InputError::Closed)
    );
}

#[test]
fn gamepad_reports_reject_out_of_range_slots_and_reserved_button_bits() {
    gamepad_event().validate().unwrap();
    let mut invalid_slot = gamepad_event();
    if let InputEvent::Gamepad { number, .. } = &mut invalid_slot {
        *number = 16;
    }
    assert_eq!(invalid_slot.validate(), Err(InputError::InvalidEvent));
    let mut invalid_button = gamepad_event();
    if let InputEvent::Gamepad { buttons, .. } = &mut invalid_button {
        *buttons = 1 << 11;
    }
    assert_eq!(invalid_button.validate(), Err(InputError::InvalidEvent));
}

#[test]
fn gamepad_protocol_round_trips_the_normalized_snapshot_fields() {
    let event = gamepad_event();
    let encoded = serde_json::to_value(&event).unwrap();

    assert_eq!(encoded["kind"], "gamepad");
    assert_eq!(
        serde_json::from_value::<InputEvent>(encoded).unwrap(),
        event
    );
}
