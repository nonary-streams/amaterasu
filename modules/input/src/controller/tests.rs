use super::*;
use crate::model::{Button, DevicePolicy, Key};
use std::cell::RefCell;
use std::rc::Rc;

#[derive(Default)]
struct FakeBackend {
    trace: Rc<RefCell<Vec<Event>>>,
    starts: usize,
    stops: usize,
    fail_next: bool,
    fail_release: bool,
    reject_absolute: bool,
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
    fn stop(&mut self) {
        self.stops += 1;
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
        key: Key::MoonlightVk { code: 0x10 },
        state: KeyState::Down,
    };
    let left_shift_up = Event::Key {
        key: Key::MoonlightVk { code: 0xa0 },
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
    assert_eq!(controller.device_policy().keyboard, false);
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
    assert_eq!(controller.device_policy().mouse, false);
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
