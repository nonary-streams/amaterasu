use super::*;
use crate::model::{Axis, Button, DevicePolicy, Key};
use std::cell::RefCell;
use std::rc::Rc;

#[derive(Default)]
struct FakeBackend {
    trace: Rc<RefCell<Vec<Event>>>,
    starts: usize,
    stops: usize,
    fail_next: bool,
    fail_release: bool,
    fail_stop: bool,
    fail_controller_input: bool,
    reject_absolute: bool,
    controller_input_supported: bool,
}
impl Backend for FakeBackend {
    fn name(&self) -> &'static str {
        "test_only_fake"
    }
    fn supported(&self) -> bool {
        true
    }
    fn absolute_pointer(&self) -> bool {
        !self.reject_absolute
    }
    fn controller_input_supported(&self) -> bool {
        self.controller_input_supported
    }
    fn key_encoding(&self) -> &'static str {
        "test_only"
    }
    fn validate(&self, event: &Event) -> InputResult<()> {
        event.validate()?;
        if self.reject_absolute && matches!(event, Event::Absolute { .. }) {
            return Err(InputError::new("unsupported_event", "absolute unavailable"));
        }
        Ok(())
    }
    fn start(&mut self) -> InputResult<()> {
        self.starts += 1;
        Ok(())
    }
    fn inject(&mut self, event: &Event) -> InputResult<()> {
        self.trace.borrow_mut().push(event.clone());
        if matches!(event, Event::Controller { .. })
            && std::mem::take(&mut self.fail_controller_input)
        {
            return Err(InputError::new(
                "controller_unavailable",
                "test virtual gamepad failure",
            ));
        }
        if std::mem::take(&mut self.fail_next)
            || (self.fail_release && event.transition().is_some_and(|(_, down)| !down))
        {
            return Err(InputError::new(
                "injection_failed",
                "injected native failure",
            ));
        }
        Ok(())
    }
    fn stop(&mut self) -> InputResult<()> {
        self.stops += 1;
        if self.fail_stop {
            Err(InputError::new(
                "injection_failed",
                "virtual device still active",
            ))
        } else {
            Ok(())
        }
    }
}
fn key(code: u16, state: KeyState) -> Event {
    Event::Key {
        key: Key::WindowsScan {
            code,
            extended: false,
        },
        state,
    }
}
fn moonlight_key(code: u8, state: KeyState, repeat_modifiers: u8, temporary: bool) -> Event {
    Event::Key {
        key: Key::MoonlightVk {
            code,
            normalized: true,
            repeat_modifiers,
            temporary_modifier: temporary,
        },
        state,
    }
}
fn acquire(controller: &mut Controller<FakeBackend>, now: Instant) -> String {
    let response = controller.handle(
        "owner",
        Operation::Acquire {
            operation_id: None,
            ttl_ms: Some(1000),
        },
        now,
        &mut || Ok(()),
    );
    assert_eq!(response["ok"], true);
    response["lease"].as_str().unwrap().into()
}

#[test]
fn controller_capability_is_present_in_status_and_active_lease_responses() {
    let mut controller = Controller::new(
        FakeBackend {
            controller_input_supported: true,
            ..Default::default()
        },
        true,
    );
    let now = Instant::now();
    let acquired = controller.handle(
        "owner",
        Operation::Acquire {
            operation_id: Some("gamepad-capability".into()),
            ttl_ms: Some(1000),
        },
        now,
        &mut || Ok(()),
    );
    assert_eq!(acquired["controller_input"], true);

    let lookup = controller.handle(
        "owner",
        Operation::Lookup {
            operation_id: "gamepad-capability".into(),
        },
        now,
        &mut || Ok(()),
    );
    assert_eq!(lookup["controller_input"], true);

    let status = controller.handle("owner", Operation::Status, now, &mut || Ok(()));
    assert_eq!(status["controller_input"], true);
}

#[test]
fn failed_backend_stop_keeps_lease_for_cleanup_retry() {
    let mut controller = Controller::new(
        FakeBackend {
            fail_stop: true,
            ..Default::default()
        },
        true,
    );
    let now = Instant::now();
    let token = acquire(&mut controller, now);
    let rejected_release = controller.handle(
        "owner",
        Operation::Release {
            lease: token.clone(),
        },
        now,
        &mut || Ok(()),
    );
    assert_eq!(rejected_release["error"]["code"], "release_failed");
    assert_eq!(rejected_release["cleanup_required"], true);

    let status = controller.handle("owner", Operation::Status, now, &mut || Ok(()));
    assert_eq!(status["lease"]["token"], token);
    assert_eq!(status["lease"]["cleanup_required"], true);

    controller.backend.fail_stop = false;
    controller.expire(now + Duration::from_millis(101));
    assert!(controller.lease.is_none());
    assert_eq!(controller.backend.stops, 2);
}
fn batch(controller: &mut Controller<FakeBackend>, lease: &str, events: Vec<Event>) -> Value {
    controller.handle(
        "owner",
        Operation::Batch {
            lease: lease.into(),
            events,
        },
        Instant::now(),
        &mut || Ok(()),
    )
}

fn controller_event() -> Event {
    Event::Controller {
        number: 0,
        active_mask: 1,
        buttons: 1 << 4,
        left_trigger: 0,
        right_trigger: 0,
        left_x: 0,
        left_y: 0,
        right_x: 0,
        right_y: 0,
    }
}

#[test]
fn unavailable_gamepad_keeps_the_input_lease_for_other_events_and_cleanup() {
    let mut controller = Controller::new(
        FakeBackend {
            controller_input_supported: true,
            fail_controller_input: true,
            ..Default::default()
        },
        true,
    );
    let token = acquire(&mut controller, Instant::now());

    let failure = batch(&mut controller, &token, vec![controller_event()]);
    assert_eq!(failure["error"]["code"], "controller_unavailable");
    assert_eq!(failure["accepted"], 0);
    assert_eq!(failure["partial_event_possible"], true);
    assert!(controller.lease.is_some());
    assert_eq!(controller.backend.stops, 0);

    let accepted = batch(&mut controller, &token, vec![key(30, KeyState::Down)]);
    assert_eq!(accepted["ok"], true);
    assert!(controller.lease.is_some());
}

#[test]
fn lease_owner_and_generation_gate_all_input_and_renewal() {
    let mut controller = Controller::new(FakeBackend::default(), true);
    let now = Instant::now();
    let token = acquire(&mut controller, now);
    let denied = controller.handle(
        "other",
        Operation::Batch {
            lease: token.clone(),
            events: vec![key(30, KeyState::Down)],
        },
        now,
        &mut || Ok(()),
    );
    assert_eq!(denied["error"]["code"], "permission_denied");
    assert!(controller.backend.trace.borrow().is_empty());
    let status = controller.handle("other", Operation::Status, now, &mut || Ok(()));
    assert!(status["lease"]["token"].is_null());
    let stale = controller.handle(
        "owner",
        Operation::Renew {
            lease: "stale".into(),
            ttl_ms: Some(2000),
        },
        now,
        &mut || Ok(()),
    );
    assert_eq!(stale["error"]["code"], "permission_denied");
    let renewed = controller.handle(
        "owner",
        Operation::Renew {
            lease: token,
            ttl_ms: Some(2000),
        },
        now,
        &mut || Ok(()),
    );
    assert_eq!(renewed["ttl_ms"], 2000);
    controller.expire(now + Duration::from_millis(1500));
    assert!(controller.lease.is_some());
}
#[test]
fn release_and_expiry_release_chords_in_reverse_order() {
    for expire in [false, true] {
        let mut controller = Controller::new(FakeBackend::default(), true);
        let now = Instant::now();
        let token = acquire(&mut controller, now);
        assert_eq!(
            batch(
                &mut controller,
                &token,
                vec![
                    key(29, KeyState::Down),
                    key(30, KeyState::Down),
                    Event::Button {
                        button: Button::Left,
                        pressed: true
                    }
                ]
            )["ok"],
            true
        );
        if expire {
            controller.expire(now + Duration::from_millis(1001));
        } else {
            assert_eq!(
                controller.handle(
                    "owner",
                    Operation::Release { lease: token },
                    now,
                    &mut || Ok(())
                )["released"],
                true
            );
        }
        assert_eq!(
            &controller.backend.trace.borrow()[3..],
            &[
                Event::Button {
                    button: Button::Left,
                    pressed: false
                },
                key(30, KeyState::Up),
                key(29, KeyState::Up)
            ]
        );
        assert!(controller.lease.is_none());
        assert!(controller.pressed.is_empty());
        assert_eq!(controller.backend.stops, 1);
    }
}
#[test]
fn whole_batch_validation_precedes_every_native_effect() {
    let mut controller = Controller::new(
        FakeBackend {
            reject_absolute: true,
            ..Default::default()
        },
        true,
    );
    let token = acquire(&mut controller, Instant::now());
    for events in [
        vec![key(30, KeyState::Down), Event::Absolute { x: 1, y: 2 }],
        vec![key(30, KeyState::Down), key(30, KeyState::Down)],
        vec![key(30, KeyState::Repeat)],
        vec![key(30, KeyState::Up)],
        vec![Event::Relative {
            dx: i32::MAX,
            dy: 0,
        }],
        vec![Event::Relative { dx: 1, dy: 1 }; 65],
    ] {
        assert_eq!(batch(&mut controller, &token, events)["ok"], false);
        assert!(controller.backend.trace.borrow().is_empty());
        assert!(controller.pressed.is_empty());
    }
}
#[test]
fn cancellation_mid_batch_releases_accepted_inputs_and_ends_lease() {
    let mut controller = Controller::new(FakeBackend::default(), true);
    let token = acquire(&mut controller, Instant::now());
    let mut guards = 0;
    let response = controller.handle(
        "owner",
        Operation::Batch {
            lease: token,
            events: vec![key(30, KeyState::Down), Event::Relative { dx: 1, dy: 1 }],
        },
        Instant::now(),
        &mut || {
            guards += 1;
            if guards == 1 {
                Ok(())
            } else {
                Err(InputError::new("cancelled", "request ended"))
            }
        },
    );
    assert_eq!(response["error"]["code"], "cancelled");
    assert_eq!(response["accepted"], 1);
    assert_eq!(
        *controller.backend.trace.borrow(),
        [key(30, KeyState::Down), key(30, KeyState::Up)]
    );
    assert!(controller.lease.is_none());
}
#[test]
fn uncertain_down_failure_is_tracked_before_cleanup() {
    let mut controller = Controller::new(FakeBackend::default(), true);
    let token = acquire(&mut controller, Instant::now());
    controller.backend.fail_next = true;
    let response = batch(&mut controller, &token, vec![key(30, KeyState::Down)]);
    assert_eq!(response["ok"], false);
    assert_eq!(response["accepted"], 0);
    assert_eq!(
        *controller.backend.trace.borrow(),
        [key(30, KeyState::Down), key(30, KeyState::Up)]
    );
    assert!(controller.lease.is_none());
}
#[test]
fn failed_releases_remain_tracked_block_new_leases_and_retry() {
    let mut controller = Controller::new(FakeBackend::default(), true);
    let now = Instant::now();
    let token = acquire(&mut controller, now);
    batch(&mut controller, &token, vec![key(30, KeyState::Down)]);
    controller.backend.fail_release = true;
    let result = controller.handle(
        "owner",
        Operation::Release {
            lease: token.clone(),
        },
        now,
        &mut || Ok(()),
    );
    assert_eq!(result["error"]["code"], "release_failed");
    assert_eq!(result["cleanup_required"], true);
    assert_eq!(controller.pressed.len(), 1);
    assert_eq!(controller.backend.stops, 0);
    let busy = controller.handle(
        "other",
        Operation::Acquire {
            operation_id: None,
            ttl_ms: None,
        },
        now,
        &mut || Ok(()),
    );
    assert_eq!(busy["error"]["code"], "input_busy");
    assert_eq!(
        batch(
            &mut controller,
            &token,
            vec![Event::Relative { dx: 1, dy: 1 }]
        )["error"]["code"],
        "cleanup_required"
    );
    controller.backend.fail_release = false;
    controller.expire(now + Duration::from_millis(101));
    assert!(controller.pressed.is_empty());
    assert!(controller.lease.is_none());
    assert_eq!(controller.backend.stops, 1);
}
#[test]
fn completed_request_cancellation_and_shutdown_release_owned_state() {
    let trace = Rc::new(RefCell::new(Vec::new()));
    {
        let mut controller = Controller::new(
            FakeBackend {
                trace: trace.clone(),
                ..Default::default()
            },
            true,
        );
        let token = acquire(&mut controller, Instant::now());
        batch(&mut controller, &token, vec![key(30, KeyState::Down)]);
        controller.cancel_owner("other");
        assert_eq!(controller.pressed.len(), 1);
        controller.cancel_owner("owner");
        assert!(controller.pressed.is_empty());
        let token = acquire(&mut controller, Instant::now());
        batch(
            &mut controller,
            &token,
            vec![Event::Button {
                button: Button::Right,
                pressed: true,
            }],
        );
    }
    assert_eq!(
        *trace.borrow(),
        [
            key(30, KeyState::Down),
            key(30, KeyState::Up),
            Event::Button {
                button: Button::Right,
                pressed: true
            },
            Event::Button {
                button: Button::Right,
                pressed: false
            }
        ]
    );
}
#[test]
fn disabled_policy_cancelled_acquire_and_malformed_coordinates_do_not_activate() {
    let mut controller = Controller::new(FakeBackend::default(), false);
    let response = controller.handle(
        "owner",
        Operation::Acquire {
            operation_id: None,
            ttl_ms: None,
        },
        Instant::now(),
        &mut || Ok(()),
    );
    assert_eq!(response["error"]["code"], "injection_disabled");
    assert_eq!(controller.backend.starts, 0);
    controller.enabled = true;
    let response = controller.handle(
        "owner",
        Operation::Acquire {
            operation_id: None,
            ttl_ms: None,
        },
        Instant::now(),
        &mut || Err(InputError::new("cancelled", "ended")),
    );
    assert_eq!(response["error"]["code"], "cancelled");
    assert_eq!(controller.backend.starts, 0);
    for value in [
        json!({"kind":"absolute","x":-1,"y":0}),
        json!({"kind":"absolute","x":65536,"y":0}),
        json!({"kind":"relative","dx":0,"dy":0,"owner":"other"}),
    ] {
        assert!(serde_json::from_value::<Event>(value).is_err());
    }
}

fn lose_completion(controller: &mut Controller<FakeBackend>, mutating: bool, reply: &Value) {
    if crate::completion_requires_cleanup(mutating, reply) {
        controller.cancel_owner("owner");
    }
}

#[test]
fn lost_error_replies_preserve_the_existing_lease_and_pressed_keys() {
    let mut controller = Controller::new(FakeBackend::default(), true);
    let now = Instant::now();
    let token = acquire(&mut controller, now);
    batch(&mut controller, &token, vec![key(30, KeyState::Down)]);
    for operation in [
        Operation::Acquire {
            operation_id: None,
            ttl_ms: None,
        },
        Operation::Renew {
            lease: token.clone(),
            ttl_ms: Some(0),
        },
        Operation::Renew {
            lease: "invalid".into(),
            ttl_ms: None,
        },
        Operation::Batch {
            lease: "invalid".into(),
            events: vec![key(31, KeyState::Down)],
        },
    ] {
        let reply = controller.handle("owner", operation, now, &mut || Ok(()));
        assert_eq!(reply["ok"], false);
        lose_completion(&mut controller, true, &reply);
        assert_eq!(controller.lease.as_ref().unwrap().token, token);
        assert_eq!(controller.pressed.len(), 1);
        assert_eq!(
            *controller.backend.trace.borrow(),
            [key(30, KeyState::Down)]
        );
    }
    let status = controller.handle("owner", Operation::Status, now, &mut || Ok(()));
    lose_completion(&mut controller, false, &status);
    assert!(controller.lease.is_some());
}

#[test]
fn lost_success_replies_release_new_acquisition_and_batch_state() {
    let mut controller = Controller::new(FakeBackend::default(), true);
    let reply = controller.handle(
        "owner",
        Operation::Acquire {
            operation_id: None,
            ttl_ms: None,
        },
        Instant::now(),
        &mut || Ok(()),
    );
    assert_eq!(reply["ok"], true);
    lose_completion(&mut controller, true, &reply);
    assert!(controller.lease.is_none());
    assert_eq!(controller.backend.stops, 1);

    let token = acquire(&mut controller, Instant::now());
    let reply = batch(&mut controller, &token, vec![key(30, KeyState::Down)]);
    assert_eq!(reply["ok"], true);
    lose_completion(&mut controller, true, &reply);
    assert!(controller.lease.is_none());
    assert_eq!(
        *controller.backend.trace.borrow(),
        [key(30, KeyState::Down), key(30, KeyState::Up)]
    );
}

#[test]
fn lost_failed_native_reply_retries_unresolved_cleanup() {
    let mut controller = Controller::new(FakeBackend::default(), true);
    let token = acquire(&mut controller, Instant::now());
    controller.backend.fail_next = true;
    controller.backend.fail_release = true;
    let reply = batch(&mut controller, &token, vec![key(30, KeyState::Down)]);
    assert_eq!(reply["ok"], false);
    assert_eq!(reply["cleanup_required"], true);
    controller.backend.fail_release = false;
    lose_completion(&mut controller, true, &reply);
    assert!(controller.lease.is_none());
    assert!(controller.pressed.is_empty());
}

#[test]
fn acquire_operation_retries_lookup_and_terminal_tombstones_are_owner_scoped() {
    let mut controller = Controller::new(FakeBackend::default(), true);
    let now = Instant::now();
    let acquire = || Operation::Acquire {
        operation_id: Some("session.input.7".into()),
        ttl_ms: Some(2000),
    };
    let first = controller.handle("owner", acquire(), now, &mut || Ok(()));
    let token = first["lease"].as_str().unwrap().to_owned();
    assert_eq!(first["state"], "active");
    assert_eq!(first["absolute_pointer"], true);
    assert_eq!(controller.backend.starts, 1);

    let retry = controller.handle(
        "owner",
        acquire(),
        now + Duration::from_millis(500),
        &mut || Ok(()),
    );
    assert_eq!(retry["lease"], token);
    assert_eq!(retry["expires_in_ms"], 1500);
    assert_eq!(controller.backend.starts, 1);
    let lookup = controller.handle(
        "owner",
        Operation::Lookup {
            operation_id: "session.input.7".into(),
        },
        now + Duration::from_millis(600),
        &mut || Ok(()),
    );
    assert_eq!(lookup["lease"], token);
    assert_eq!(lookup["absolute_pointer"], true);
    let foreign = controller.handle(
        "other",
        Operation::Lookup {
            operation_id: "session.input.7".into(),
        },
        now,
        &mut || Ok(()),
    );
    assert_eq!(foreign["error"]["code"], "not_found");
    let conflict = controller.handle(
        "owner",
        Operation::Acquire {
            operation_id: Some("session.input.7".into()),
            ttl_ms: Some(3000),
        },
        now,
        &mut || Ok(()),
    );
    assert_eq!(conflict["error"]["code"], "operation_conflict");

    let released = controller.handle(
        "owner",
        Operation::Release { lease: token },
        now,
        &mut || Ok(()),
    );
    assert_eq!(released["released"], true);
    let tombstone = controller.handle("owner", acquire(), now, &mut || Ok(()));
    assert_eq!(tombstone["state"], "released");
    assert_eq!(tombstone["reason"], "released");
    assert!(tombstone["lease"].is_null());
    assert!(tombstone.get("absolute_pointer").is_none());
    assert_eq!(controller.backend.starts, 1);
}

#[test]
fn moonlight_modifier_aliases_share_ownership_and_native_encodings_cannot_mix() {
    let mut controller = Controller::new(FakeBackend::default(), true);
    let token = acquire(&mut controller, Instant::now());
    let generic_shift = Event::Key {
        key: Key::MoonlightVk {
            code: 0x10,
            normalized: true,
            repeat_modifiers: 0,
            temporary_modifier: false,
        },
        state: KeyState::Down,
    };
    let left_shift_up = Event::Key {
        key: Key::MoonlightVk {
            code: 0xa0,
            normalized: true,
            repeat_modifiers: 0,
            temporary_modifier: false,
        },
        state: KeyState::Up,
    };
    assert_eq!(
        batch(
            &mut controller,
            &token,
            vec![generic_shift.clone(), left_shift_up.clone()]
        )["ok"],
        true
    );
    assert!(controller.pressed.is_empty());

    assert_eq!(
        batch(
            &mut controller,
            &token,
            vec![generic_shift, key(42, KeyState::Up)]
        )["error"]["code"],
        "invalid_sequence"
    );
    assert!(controller.pressed.is_empty());
}

#[test]
fn raw_key_cleanup_keeps_the_keydown_mode_and_rejects_cross_mode_duplicate() {
    let backend = FakeBackend::default();
    let trace = backend.trace.clone();
    let mut controller = Controller::new(backend, true);
    let token = acquire(&mut controller, Instant::now());
    let raw_down = Event::Key {
        key: Key::MoonlightVk {
            code: 0x41,
            normalized: false,
            repeat_modifiers: 0,
            temporary_modifier: false,
        },
        state: KeyState::Down,
    };
    assert_eq!(batch(&mut controller, &token, vec![raw_down])["ok"], true);

    let normalized_duplicate = Event::Key {
        key: Key::MoonlightVk {
            code: 0x41,
            normalized: true,
            repeat_modifiers: 0,
            temporary_modifier: false,
        },
        state: KeyState::Down,
    };
    assert_eq!(
        batch(&mut controller, &token, vec![normalized_duplicate])["error"]["code"],
        "invalid_sequence"
    );

    assert_eq!(
        controller.handle(
            "owner",
            Operation::Release { lease: token },
            Instant::now(),
            &mut || Ok(()),
        )["released"],
        true
    );
    assert_eq!(
        trace.borrow().last(),
        Some(&Event::Key {
            key: Key::MoonlightVk {
                code: 0x41,
                normalized: false,
                repeat_modifiers: 0,
                temporary_modifier: false,
            },
            state: KeyState::Up,
        })
    );
}

#[test]
fn disabled_categories_are_accepted_without_native_injection() {
    let mut controller = Controller::new(FakeBackend::default(), true);
    let token = acquire(&mut controller, Instant::now());
    controller
        .set_device_policy(
            DevicePolicy {
                keyboard: false,
                mouse: true,
            },
            Instant::now(),
        )
        .unwrap();

    let response = batch(
        &mut controller,
        &token,
        vec![
            key(30, KeyState::Down),
            key(30, KeyState::Up),
            Event::Relative { dx: 1, dy: 2 },
        ],
    );
    assert_eq!(response["ok"], true);
    assert_eq!(response["accepted"], 3);
    assert_eq!(response["suppressed"], 2);
    assert_eq!(
        *controller.backend.trace.borrow(),
        [Event::Relative { dx: 1, dy: 2 }]
    );
}

#[test]
fn high_resolution_scroll_units_pass_through_by_default() {
    let mut controller = Controller::new(FakeBackend::default(), true);
    let token = acquire(&mut controller, Instant::now());
    let events = vec![
        Event::ScrollUnits {
            axis: Axis::Vertical,
            amount: 17,
        },
        Event::ScrollUnits {
            axis: Axis::Horizontal,
            amount: -31,
        },
    ];

    assert_eq!(batch(&mut controller, &token, events)["ok"], true);
    assert_eq!(
        *controller.backend.trace.borrow(),
        [
            Event::ScrollUnits {
                axis: Axis::Vertical,
                amount: 17,
            },
            Event::ScrollUnits {
                axis: Axis::Horizontal,
                amount: -31,
            },
        ]
    );
}

#[test]
fn event_json_keeps_legacy_steps_and_adds_bounded_raw_units() {
    let legacy: Event = serde_json::from_value(serde_json::json!({
        "kind":"scroll",
        "axis":"vertical",
        "steps":-2,
    }))
    .unwrap();
    assert_eq!(
        legacy,
        Event::Scroll {
            axis: Axis::Vertical,
            steps: -2,
        }
    );

    let raw: Event = serde_json::from_value(serde_json::json!({
        "kind":"scroll_units",
        "axis":"horizontal",
        "amount":-45,
    }))
    .unwrap();
    assert_eq!(
        raw,
        Event::ScrollUnits {
            axis: Axis::Horizontal,
            amount: -45,
        }
    );
    assert!(raw.validate().is_ok());

    let zero: Event = serde_json::from_value(serde_json::json!({
        "kind":"scroll_units",
        "axis":"vertical",
        "amount":0,
    }))
    .unwrap();
    assert_eq!(zero.validate().unwrap_err().code, "invalid_event");
}

#[test]
fn disabled_high_resolution_scroll_accumulates_per_axis_and_resets_per_lease() {
    let mut controller = Controller::new(FakeBackend::default(), true);
    controller.set_high_resolution_scrolling(false);
    let token = acquire(&mut controller, Instant::now());

    assert_eq!(
        batch(
            &mut controller,
            &token,
            vec![
                Event::ScrollUnits {
                    axis: Axis::Vertical,
                    amount: 60,
                },
                Event::ScrollUnits {
                    axis: Axis::Horizontal,
                    amount: -60,
                },
                Event::ScrollUnits {
                    axis: Axis::Vertical,
                    amount: 60,
                },
                Event::ScrollUnits {
                    axis: Axis::Horizontal,
                    amount: -60,
                },
            ]
        )["ok"],
        true
    );
    assert_eq!(
        *controller.backend.trace.borrow(),
        [
            Event::Scroll {
                axis: Axis::Vertical,
                steps: 1,
            },
            Event::Scroll {
                axis: Axis::Horizontal,
                steps: -1,
            },
        ]
    );

    assert_eq!(
        batch(
            &mut controller,
            &token,
            vec![Event::ScrollUnits {
                axis: Axis::Vertical,
                amount: 60,
            }]
        )["ok"],
        true
    );
    assert_eq!(controller.vertical_scroll_remainder, 60);
    assert_eq!(
        controller.handle(
            "owner",
            Operation::Release { lease: token },
            Instant::now(),
            &mut || Ok(()),
        )["ok"],
        true
    );
    assert_eq!(controller.vertical_scroll_remainder, 0);

    let next_lease = acquire(&mut controller, Instant::now());
    assert_eq!(
        batch(
            &mut controller,
            &next_lease,
            vec![Event::ScrollUnits {
                axis: Axis::Vertical,
                amount: 60,
            }]
        )["ok"],
        true
    );
    assert_eq!(controller.backend.trace.borrow().len(), 2);
    assert_eq!(controller.vertical_scroll_remainder, 60);
}

#[test]
fn changing_scroll_mode_discards_fractional_wheel_units() {
    let mut controller = Controller::new(FakeBackend::default(), true);
    controller.set_high_resolution_scrolling(false);
    let token = acquire(&mut controller, Instant::now());
    assert_eq!(
        batch(
            &mut controller,
            &token,
            vec![Event::ScrollUnits {
                axis: Axis::Vertical,
                amount: 60,
            }]
        )["ok"],
        true
    );
    assert_eq!(controller.vertical_scroll_remainder, 60);

    controller.set_high_resolution_scrolling(true);
    assert_eq!(controller.vertical_scroll_remainder, 0);
    assert_eq!(
        batch(
            &mut controller,
            &token,
            vec![Event::ScrollUnits {
                axis: Axis::Vertical,
                amount: 60,
            }]
        )["ok"],
        true
    );
    assert_eq!(
        controller.backend.trace.borrow().last(),
        Some(&Event::ScrollUnits {
            axis: Axis::Vertical,
            amount: 60,
        })
    );
}

#[test]
fn a_large_raw_scroll_is_split_into_bounded_legacy_step_events() {
    let mut controller = Controller::new(FakeBackend::default(), true);
    controller.set_high_resolution_scrolling(false);
    let token = acquire(&mut controller, Instant::now());

    assert_eq!(
        batch(
            &mut controller,
            &token,
            vec![Event::ScrollUnits {
                axis: Axis::Vertical,
                amount: i16::MAX,
            }]
        )["ok"],
        true
    );
    assert_eq!(
        *controller.backend.trace.borrow(),
        [
            Event::Scroll {
                axis: Axis::Vertical,
                steps: 100,
            },
            Event::Scroll {
                axis: Axis::Vertical,
                steps: 100,
            },
            Event::Scroll {
                axis: Axis::Vertical,
                steps: 73,
            },
        ]
    );
    assert_eq!(controller.vertical_scroll_remainder, 7);
}

#[test]
fn failed_legacy_scroll_injection_does_not_commit_fractional_remainder() {
    let mut controller = Controller::new(FakeBackend::default(), true);
    controller.set_high_resolution_scrolling(false);
    let token = acquire(&mut controller, Instant::now());
    assert_eq!(
        batch(&mut controller, &token, vec![key(30, KeyState::Down)])["ok"],
        true
    );
    controller.backend.fail_release = true;
    controller.backend.fail_next = true;

    let failed = batch(
        &mut controller,
        &token,
        vec![Event::ScrollUnits {
            axis: Axis::Vertical,
            amount: 121,
        }],
    );
    assert_eq!(failed["ok"], false);
    assert_eq!(failed["accepted"], 0);
    assert_eq!(failed["partial_event_possible"], true);
    assert!(controller.cleanup_required());
    assert_eq!(controller.vertical_scroll_remainder, 0);

    controller.backend.fail_release = false;
    controller.expire(Instant::now() + Duration::from_millis(101));
    assert!(controller.lease.is_none());
    assert_eq!(controller.vertical_scroll_remainder, 0);
}

#[test]
fn disabling_a_category_releases_held_inputs_and_gates_before_cleanup_retry() {
    let mut controller = Controller::new(FakeBackend::default(), true);
    let now = Instant::now();
    let token = acquire(&mut controller, now);
    batch(
        &mut controller,
        &token,
        vec![
            key(30, KeyState::Down),
            Event::Button {
                button: Button::Left,
                pressed: true,
            },
        ],
    );

    controller
        .set_device_policy(
            DevicePolicy {
                keyboard: false,
                mouse: true,
            },
            now,
        )
        .unwrap();
    assert_eq!(controller.pressed, [Pressed::Button(Button::Left)]);
    assert!(!controller.device_policy().keyboard);
    assert_eq!(
        batch(&mut controller, &token, vec![key(30, KeyState::Down)])["suppressed"],
        1
    );

    controller.backend.fail_release = true;
    let failure = controller.set_device_policy(
        DevicePolicy {
            keyboard: true,
            mouse: false,
        },
        now,
    );
    assert_eq!(failure.unwrap_err().code, "release_failed");
    assert!(!controller.device_policy().mouse);
    assert!(controller.cleanup_required());
    assert_eq!(controller.pressed, [Pressed::Button(Button::Left)]);

    controller.backend.fail_release = false;
    controller
        .set_device_policy(
            DevicePolicy {
                keyboard: true,
                mouse: false,
            },
            now,
        )
        .unwrap();
    controller.expire(now + Duration::from_millis(101));
    assert!(controller.pressed.is_empty());
    assert!(controller.lease.is_none());
}

#[test]
fn repeat_scheduler_obeys_delay_cadence_and_cancels_on_key_up() {
    let backend = FakeBackend::default();
    let trace = backend.trace.clone();
    let mut controller = Controller::new(backend, true);
    controller.set_key_repeat_policy(10, 100.0);
    let token = acquire(&mut controller, Instant::now());
    assert_eq!(
        batch(&mut controller, &token, vec![key(30, KeyState::Down)])["ok"],
        true
    );
    let after_down = Instant::now();

    controller.expire(after_down + Duration::from_millis(9));
    assert_eq!(trace.borrow().len(), 1);
    controller.expire(after_down + Duration::from_millis(37));
    assert_eq!(trace.borrow().len(), 2);
    assert_eq!(trace.borrow()[1], key(30, KeyState::Repeat));
    let next_due = controller.repeating_key.as_ref().unwrap().next_due;
    assert!(next_due > after_down + Duration::from_millis(37));
    assert!(next_due <= after_down + Duration::from_millis(41));

    controller.expire(after_down + Duration::from_millis(37));
    assert_eq!(
        trace.borrow().len(),
        2,
        "a missed deadline does not catch up"
    );
    controller.expire(next_due);
    assert_eq!(
        trace.borrow().len(),
        3,
        "at most one repeat is emitted per tick"
    );

    assert_eq!(
        batch(&mut controller, &token, vec![key(30, KeyState::Up)])["ok"],
        true
    );
    assert!(controller.repeating_key.is_none());
    let count = trace.borrow().len();
    controller.expire(Instant::now() + Duration::from_millis(100));
    assert_eq!(trace.borrow().len(), count);
}

#[test]
fn standalone_modifier_keys_follow_reference_repeat_behavior() {
    let backend = FakeBackend::default();
    let trace = backend.trace.clone();
    let mut controller = Controller::new(backend, true);
    controller.set_key_repeat_policy(1, 100.0);
    let token = acquire(&mut controller, Instant::now());
    controller.handle(
        "owner",
        Operation::Renew {
            lease: token.clone(),
            ttl_ms: Some(30_000),
        },
        Instant::now(),
        &mut || Ok(()),
    );

    // Vibeshine's is_modifier check suppresses synthesized shortcut modifiers;
    // its repeat timer is scheduled for every accepted key-down, including modifiers.
    for code in [0x10, 0xa2, 0xa5, 0x5b] {
        assert_eq!(
            batch(
                &mut controller,
                &token,
                vec![moonlight_key(code, KeyState::Down, 0, false)],
            )["ok"],
            true
        );
        let next_due = controller.repeating_key.as_ref().unwrap().next_due;
        controller.expire(next_due);
        assert_eq!(
            trace.borrow().last(),
            Some(&moonlight_key(code, KeyState::Repeat, 0, false)),
            "modifier VK {code:#04x} must be repeated"
        );
        assert_eq!(
            batch(
                &mut controller,
                &token,
                vec![moonlight_key(code, KeyState::Up, 0, false)],
            )["ok"],
            true
        );
        assert!(controller.repeating_key.is_none());
    }
}

#[test]
fn zero_delay_disables_repeat_and_zero_frequency_uses_reference_default() {
    let mut controller = Controller::new(FakeBackend::default(), true);
    assert_eq!(
        repeat_period(0.0),
        repeat_period(DEFAULT_KEY_REPEAT_FREQUENCY_HZ)
    );
    assert_eq!(repeat_period(1e-20), Duration::MAX);

    controller.set_key_repeat_policy(0, 0.0);
    let token = acquire(&mut controller, Instant::now());
    assert_eq!(
        batch(&mut controller, &token, vec![key(30, KeyState::Down)])["ok"],
        true
    );
    assert!(controller.repeating_key.is_none());
    let count = controller.backend.trace.borrow().len();
    controller.expire(Instant::now() + Duration::from_millis(500));
    assert_eq!(controller.backend.trace.borrow().len(), count);
}

#[test]
fn tiny_positive_frequency_keeps_the_initial_repeat_delay() {
    let backend = FakeBackend::default();
    let trace = backend.trace.clone();
    let mut controller = Controller::new(backend, true);
    controller.set_key_repeat_policy(1, 1e-20);
    let token = acquire(&mut controller, Instant::now());
    assert_eq!(
        batch(&mut controller, &token, vec![key(30, KeyState::Down)])["ok"],
        true
    );

    let first_repeat_due = controller.repeating_key.as_ref().unwrap().next_due;
    controller.expire(first_repeat_due);
    assert_eq!(trace.borrow().len(), 2);
    assert_eq!(trace.borrow()[1], key(30, KeyState::Repeat));
    assert!(
        controller.repeating_key.is_none(),
        "an interval that cannot fit in Instant must not create an immediate repeat loop"
    );
}

#[test]
fn settings_change_and_lease_end_cancel_repeat_state() {
    let mut controller = Controller::new(FakeBackend::default(), true);
    let token = acquire(&mut controller, Instant::now());
    assert_eq!(
        batch(&mut controller, &token, vec![key(30, KeyState::Down)])["ok"],
        true
    );
    assert!(controller.repeating_key.is_some());
    controller.set_key_repeat_policy(250, 30.5);
    assert!(controller.repeating_key.is_none());
    assert_eq!(controller.key_repeat_policy(), (250, 30.5));

    assert_eq!(
        controller.handle(
            "owner",
            Operation::Release { lease: token },
            Instant::now(),
            &mut || Ok(()),
        )["released"],
        true
    );
    assert!(controller.repeating_key.is_none());
}

#[test]
fn repeating_key_pulses_only_synthetic_modifiers_and_preserves_real_held_modifiers() {
    let backend = FakeBackend::default();
    let trace = backend.trace.clone();
    let mut controller = Controller::new(backend, true);
    let token = acquire(&mut controller, Instant::now());
    controller.handle(
        "owner",
        Operation::Renew {
            lease: token.clone(),
            ttl_ms: Some(5_000),
        },
        Instant::now(),
        &mut || Ok(()),
    );
    let chord = vec![
        moonlight_key(0x10, KeyState::Down, 0, true),
        moonlight_key(0x41, KeyState::Down, 1, false),
        moonlight_key(0x10, KeyState::Up, 0, true),
    ];
    assert_eq!(batch(&mut controller, &token, chord)["ok"], true);
    let after_down = Instant::now();
    controller.expire(after_down + Duration::from_millis(501));
    assert_eq!(
        &trace.borrow()[3..],
        &[
            moonlight_key(0x10, KeyState::Down, 0, true),
            moonlight_key(0x41, KeyState::Repeat, 0, false),
            moonlight_key(0x10, KeyState::Up, 0, true),
        ]
    );
    assert!(
        !controller
            .pressed
            .iter()
            .any(|pressed| matches!(pressed, Pressed::Key(key) if is_temporary_modifier(key)))
    );

    assert_eq!(
        batch(
            &mut controller,
            &token,
            vec![moonlight_key(0xa0, KeyState::Down, 0, false)],
        )["ok"],
        true
    );
    let real_shift_down = trace.borrow().len() - 1;
    let second_due = controller.repeating_key.as_ref().unwrap().next_due;
    controller.expire(second_due);
    assert_eq!(
        trace.borrow()[real_shift_down + 1],
        moonlight_key(0xa0, KeyState::Repeat, 0, false)
    );
    assert!(controller.pressed.contains(&Pressed::Key(Key::MoonlightVk {
        code: 0xa0,
        normalized: true,
        repeat_modifiers: 0,
        temporary_modifier: false,
    })));
    assert!(!trace.borrow()[real_shift_down + 1..]
        .iter()
        .any(|event| matches!(event, Event::Key { key, state: KeyState::Down | KeyState::Up } if is_temporary_modifier(key))));
}

#[test]
fn repeat_metadata_is_validated_across_split_batches_and_cleanup_releases_temporary_keys() {
    let backend = FakeBackend::default();
    let trace = backend.trace.clone();
    let mut controller = Controller::new(backend, true);
    let token = acquire(&mut controller, Instant::now());
    assert_eq!(
        batch(
            &mut controller,
            &token,
            vec![moonlight_key(0x10, KeyState::Down, 0, true)],
        )["ok"],
        true
    );
    assert_eq!(
        batch(
            &mut controller,
            &token,
            vec![moonlight_key(0x41, KeyState::Down, 1, false)],
        )["ok"],
        true
    );
    assert_eq!(
        batch(
            &mut controller,
            &token,
            vec![moonlight_key(0x10, KeyState::Up, 0, true)],
        )["ok"],
        true
    );
    assert!(
        !controller
            .pressed
            .iter()
            .any(|pressed| matches!(pressed, Pressed::Key(key) if is_temporary_modifier(key)))
    );

    assert_eq!(
        batch(
            &mut controller,
            &token,
            vec![moonlight_key(0x10, KeyState::Down, 0, true)],
        )["ok"],
        true
    );
    controller.backend.fail_next = true;
    let failure = batch(
        &mut controller,
        &token,
        vec![
            moonlight_key(0x42, KeyState::Down, 1, false),
            moonlight_key(0x10, KeyState::Up, 0, true),
        ],
    );
    assert_eq!(failure["ok"], false);
    assert!(controller.lease.is_none());
    assert!(controller.pressed.is_empty());
    assert!(controller.repeating_key.is_none());
    let events = trace.borrow();
    assert_eq!(
        &events[events.len() - 3..],
        &[
            moonlight_key(0x42, KeyState::Up, 0, false),
            moonlight_key(0x10, KeyState::Up, 0, true),
            moonlight_key(0x41, KeyState::Up, 0, false),
        ]
    );
}

#[test]
fn repeat_metadata_rejects_bad_masks_or_orphaned_mismatched_markers() {
    let mut controller = Controller::new(FakeBackend::default(), true);
    let token = acquire(&mut controller, Instant::now());
    for event in [
        moonlight_key(0x41, KeyState::Down, 0b1000, false),
        moonlight_key(0x41, KeyState::Up, 1, false),
        moonlight_key(0x10, KeyState::Repeat, 0, true),
        moonlight_key(0x1b, KeyState::Down, 0, true),
    ] {
        assert_eq!(batch(&mut controller, &token, vec![event])["ok"], false);
        assert!(controller.backend.trace.borrow().is_empty());
    }

    assert_eq!(
        batch(
            &mut controller,
            &token,
            vec![moonlight_key(0x41, KeyState::Down, 1, false)],
        )["error"]["code"],
        "invalid_sequence"
    );
    assert_eq!(
        batch(
            &mut controller,
            &token,
            vec![moonlight_key(0x10, KeyState::Up, 0, true)],
        )["error"]["code"],
        "invalid_sequence"
    );
    assert_eq!(
        batch(
            &mut controller,
            &token,
            vec![
                moonlight_key(0x10, KeyState::Down, 0, true),
                moonlight_key(0x10, KeyState::Up, 0, false),
            ],
        )["error"]["code"],
        "invalid_sequence"
    );
    assert!(controller.backend.trace.borrow().is_empty());
}

#[test]
fn native_repeat_failure_stops_scheduler_and_retries_owned_cleanup() {
    let mut controller = Controller::new(FakeBackend::default(), true);
    let token = acquire(&mut controller, Instant::now());
    assert_eq!(
        batch(&mut controller, &token, vec![key(30, KeyState::Down)])["ok"],
        true
    );
    controller.backend.fail_next = true;
    controller.backend.fail_release = true;
    controller.expire(Instant::now() + Duration::from_millis(501));
    assert!(controller.repeating_key.is_none());
    assert!(controller.cleanup_required());
    assert!(!controller.pressed.is_empty());

    controller.backend.fail_release = false;
    controller.expire(Instant::now() + Duration::from_millis(700));
    assert!(controller.lease.is_none());
    assert!(controller.pressed.is_empty());
}
