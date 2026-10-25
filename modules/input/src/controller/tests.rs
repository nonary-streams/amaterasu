use super::*;
use crate::model::{
    Axis, Button, ControllerTouchPhase, DevicePolicy, Event as InputEvent, Key, PEN_BUTTON_MASK,
    PEN_POINTER_ID, PenTool, PenTouchDevice, PenTouchPhase,
};
use amaterasu_gamestream::control::HostFeedback;
use std::cell::RefCell;
use std::collections::BTreeSet;
use std::rc::Rc;

#[derive(Default)]
struct FakeBackend {
    gamepad_feedback: Option<amaterasu_input_companion::GamepadFeedback>,
    touch_as_mouse_max_contacts: Option<usize>,
    trace: Rc<RefCell<Vec<Event>>>,
    starts: usize,
    stops: usize,
    fail_next: bool,
    fail_release: bool,
    fail_stop: bool,
    fail_start_once: bool,
    native_resource_owned: bool,
    native_resource_trace: Option<Rc<std::cell::Cell<bool>>>,
    fail_controller_input: bool,
    reject_absolute: bool,
    supports_pen_touch: bool,
    max_pen_contacts: Option<usize>,
    max_touch_contacts: Option<usize>,
    failed_pen_touch_events: Vec<(u32, PenTouchPhase)>,
    controller_input_supported: bool,
    effective_gamepad_profile: Option<&'static str>,
    gamepad_unavailable_reason: Option<&'static str>,
    gamepad_profile: GamepadProfile,
    ds4_back_as_touchpad_click: bool,
    motion_as_ds4: bool,
    touchpad_as_ds4: bool,
    ds5_inputtino_randomize_mac: bool,
    back_button_timeout_ms: i32,
    always_send_scancodes: bool,
    key_rightalt_to_key_win: bool,
    keybindings: KeyBindings,
    keybinding_updates: Vec<KeyBindings>,
    right_alt_mapping_trace: Vec<(KeyState, bool)>,
    fail_controller_disable: bool,
    controller_disable_calls: usize,
    active_controller_slots: BTreeSet<u16>,
    full_advances: usize,
    pen_touch_advances: usize,
}
impl Backend for FakeBackend {
    fn poll_gamepad_feedback(
        &mut self,
    ) -> InputResult<Option<amaterasu_input_companion::GamepadFeedback>> {
        Ok(self.gamepad_feedback.take())
    }
    fn touch_as_mouse_max_contacts(&self) -> Option<usize> {
        self.touch_as_mouse_max_contacts
    }
    fn supports_pen_touch_event(&self, device: PenTouchDevice) -> bool {
        self.supports_pen_touch
            || (device == PenTouchDevice::Touch && self.touch_as_mouse_max_contacts.is_some())
    }

    fn name(&self) -> &'static str {
        "test_only_fake"
    }
    fn supported(&self) -> bool {
        true
    }
    fn absolute_pointer(&self) -> bool {
        !self.reject_absolute
    }
    fn pen_touch_supported(&self) -> bool {
        self.supports_pen_touch
    }
    fn pen_touch_max_contacts(&self, device: PenTouchDevice) -> Option<usize> {
        match device {
            PenTouchDevice::Pen => self.max_pen_contacts,
            PenTouchDevice::Touch => self.max_touch_contacts,
        }
    }
    fn controller_input_supported(&self) -> bool {
        self.controller_input_supported
    }
    fn effective_gamepad_profile(&self) -> Option<&'static str> {
        self.effective_gamepad_profile
    }
    fn gamepad_unavailable_reason(&self) -> Option<&str> {
        self.gamepad_unavailable_reason
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
        if self.native_resource_owned {
            return Err(InputError::new(
                "backend_busy",
                "native resource still owned",
            ));
        }
        self.native_resource_owned = true;
        if let Some(trace) = &self.native_resource_trace {
            trace.set(true);
        }
        if std::mem::take(&mut self.fail_start_once) {
            return Err(InputError::new(
                "injection_failed",
                "partial native startup",
            ));
        }
        Ok(())
    }
    fn inject(&mut self, event: &Event) -> InputResult<()> {
        self.trace.borrow_mut().push(event.clone());
        if let Event::PenTouch {
            pointer_id, phase, ..
        } = event
            && let Some(index) = self
                .failed_pen_touch_events
                .iter()
                .position(|failure| *failure == (*pointer_id, *phase))
        {
            self.failed_pen_touch_events.remove(index);
            return Err(InputError::new(
                "injection_failed",
                "test pen/touch device loss",
            ));
        }
        if let Event::Key {
            key: Key::MoonlightVk { code: 0xa5, .. },
            state,
        } = event
        {
            self.right_alt_mapping_trace
                .push((*state, self.key_rightalt_to_key_win));
        }
        if let Event::Controller { active_mask, .. } = event {
            self.active_controller_slots
                .retain(|slot| active_mask & (1 << slot) != 0);
            for slot in 0..16 {
                if active_mask & (1 << slot) != 0 {
                    self.active_controller_slots.insert(slot);
                }
            }
        }
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
    fn set_back_button_timeout(&mut self, timeout_ms: i32) {
        self.back_button_timeout_ms = timeout_ms;
    }

    fn set_motion_as_ds4(&mut self, enabled: bool) {
        self.motion_as_ds4 = enabled;
    }

    fn set_touchpad_as_ds4(&mut self, enabled: bool) {
        self.touchpad_as_ds4 = enabled;
    }
    fn set_ds5_inputtino_randomize_mac(&mut self, enabled: bool) {
        self.ds5_inputtino_randomize_mac = enabled;
    }
    fn set_always_send_scancodes(&mut self, enabled: bool) {
        self.always_send_scancodes = enabled;
    }
    fn set_key_rightalt_to_key_win(&mut self, enabled: bool) {
        self.key_rightalt_to_key_win = enabled;
    }
    fn set_keybindings(&mut self, keybindings: &KeyBindings) {
        self.keybindings = keybindings.clone();
        self.keybinding_updates.push(keybindings.clone());
    }
    fn set_gamepad_profile(&mut self, profile: GamepadProfile) {
        self.gamepad_profile = profile;
    }
    fn set_ds4_back_as_touchpad_click(&mut self, enabled: bool) {
        self.ds4_back_as_touchpad_click = enabled;
    }
    fn advance(&mut self, _now: Instant) -> InputResult<()> {
        self.full_advances += 1;
        Ok(())
    }
    fn advance_pen_touch(&mut self, _now: Instant) -> InputResult<()> {
        self.pen_touch_advances += 1;
        Ok(())
    }
    fn disable_controller_input(&mut self) -> InputResult<()> {
        self.controller_disable_calls += 1;
        if std::mem::take(&mut self.fail_controller_disable) {
            return Err(InputError::new(
                "release_failed",
                "test virtual controller removal failure",
            ));
        }
        self.active_controller_slots.clear();
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
            self.native_resource_owned = false;
            if let Some(trace) = &self.native_resource_trace {
                trace.set(false);
            }
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

fn pen_touch(
    pointer_id: u32,
    device: PenTouchDevice,
    phase: PenTouchPhase,
    x: u16,
    y: u16,
) -> Event {
    Event::PenTouch {
        pointer_id,
        device,
        phase,
        x,
        y,
        pressure: None,
        contact_area_major: None,
        contact_area_minor: None,
        rotation_degrees: None,
        pen_tool: None,
        pen_buttons: None,
        tilt: None,
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
    let response = controller.handle("owner", acquire_operation(), now, &mut || Ok(()));
    assert_eq!(response["ok"], true);
    response["lease"].as_str().unwrap().into()
}

fn acquire_operation() -> Operation {
    Operation::Acquire {
        operation_id: None,
        ttl_ms: Some(1000),
        back_button_timeout: None,
        controller: None,
        gamepad: None,
        ds4_back_as_touchpad_click: None,
        motion_as_ds4: None,
        touchpad_as_ds4: None,
        ds5_inputtino_randomize_mac: None,
        native_pen_touch: None,
        keyboard: None,
        mouse: None,
        high_resolution_scrolling: None,
        always_send_scancodes: None,
        key_rightalt_to_key_win: None,
        keybindings: None,
        key_repeat_delay: None,
        key_repeat_frequency: None,
    }
}

#[test]
fn feedback_pull_is_stream_session_only_lease_bound_and_acknowledged_after_peek() {
    let mut controller = Controller::new(FakeBackend::default(), true);
    let now = Instant::now();
    let acquired = controller.handle(
        "amaterasu.stream-session",
        acquire_operation(),
        now,
        &mut || Ok(()),
    );
    assert_eq!(acquired["ok"], true);
    let lease = acquired["lease"].as_str().unwrap().to_owned();
    let generation = controller.feedback.generation();
    let producer = controller.feedback.producer(generation, 2, 0);
    assert!(producer.push(HostFeedback::RgbLed {
        controller: 2,
        red: 12,
        green: 34,
        blue: 56,
    }));

    for limit in [0, 17] {
        let response = controller.handle(
            "amaterasu.stream-session",
            Operation::FeedbackPeek {
                lease: lease.clone(),
                limit,
            },
            now,
            &mut || Ok(()),
        );
        assert_eq!(response["error"]["code"], "invalid_request");
    }

    let peek = Operation::FeedbackPeek {
        lease: lease.clone(),
        limit: 16,
    };
    let denied = controller.handle("untrusted.caller", peek, now, &mut || Ok(()));
    assert_eq!(denied["error"]["code"], "permission_denied");

    let peeked = controller.handle(
        "amaterasu.stream-session",
        Operation::FeedbackPeek {
            lease: lease.clone(),
            limit: 16,
        },
        now,
        &mut || Ok(()),
    );
    assert_eq!(peeked["ok"], true);
    assert_eq!(peeked["events"][0]["sequence"], 1);
    assert_eq!(
        peeked["events"][0]["feedback"],
        json!({"type":"rgb_led","controller":2,"red":12,"green":34,"blue":56})
    );

    let ahead = controller.handle(
        "amaterasu.stream-session",
        Operation::FeedbackAck {
            lease: lease.clone(),
            through_sequence: 2,
        },
        now,
        &mut || Ok(()),
    );
    assert_eq!(ahead["error"]["code"], "invalid_request");
    let acknowledged = controller.handle(
        "amaterasu.stream-session",
        Operation::FeedbackAck {
            lease: lease.clone(),
            through_sequence: 1,
        },
        now,
        &mut || Ok(()),
    );
    assert_eq!(acknowledged["ok"], true);
    let duplicate_ack = controller.handle(
        "amaterasu.stream-session",
        Operation::FeedbackAck {
            lease: lease.clone(),
            through_sequence: 1,
        },
        now,
        &mut || Ok(()),
    );
    assert_eq!(duplicate_ack["ok"], true);
    assert!(controller.feedback.peek(16).is_empty());

    assert_eq!(
        controller.handle(
            "amaterasu.stream-session",
            Operation::Release { lease },
            now,
            &mut || Ok(()),
        )["released"],
        true
    );
    assert!(!producer.push(HostFeedback::RgbLed {
        controller: 2,
        red: 1,
        green: 2,
        blue: 3,
    }));
    assert!(controller.feedback.peek(16).is_empty());
}

#[test]
fn failed_startup_releases_partial_device_before_next_acquire() {
    let mut controller = Controller::new(
        FakeBackend {
            fail_start_once: true,
            ..Default::default()
        },
        true,
    );
    let now = Instant::now();
    let first = controller.handle("owner", acquire_operation(), now, &mut || Ok(()));
    assert_eq!(first["error"]["code"], "injection_failed");
    assert_eq!(controller.backend.stops, 1);
    assert!(!controller.backend.native_resource_owned);
    assert!(!controller.cleanup_required());

    let lease = acquire(&mut controller, now);
    assert!(controller.backend.native_resource_owned);
    assert_eq!(
        batch(&mut controller, &lease, vec![key(30, KeyState::Down)])["ok"],
        true
    );
    let released = controller.handle("owner", Operation::Release { lease }, now, &mut || Ok(()));
    assert_eq!(released["released"], true);
    assert!(!controller.backend.native_resource_owned);
}

#[test]
fn failed_startup_cleanup_blocks_acquire_until_retry_succeeds() {
    let mut controller = Controller::new(
        FakeBackend {
            fail_start_once: true,
            fail_stop: true,
            ..Default::default()
        },
        true,
    );
    let now = Instant::now();
    let first = controller.handle("owner", acquire_operation(), now, &mut || Ok(()));
    assert_eq!(first["error"]["code"], "injection_failed");
    assert!(controller.backend.native_resource_owned);
    assert_eq!(
        controller.handle("owner", Operation::Status, now, &mut || Ok(()))["startup_cleanup_required"],
        true
    );
    assert_eq!(
        controller.handle("owner", acquire_operation(), now, &mut || Ok(()))["error"]["code"],
        "cleanup_required"
    );
    controller.expire(now + Duration::from_millis(99));
    assert_eq!(controller.backend.stops, 1);

    controller.backend.fail_stop = false;
    controller.expire(now + Duration::from_millis(100));
    assert_eq!(controller.backend.stops, 2);
    assert!(!controller.cleanup_required());
    let lease = acquire(&mut controller, now + Duration::from_millis(101));
    assert_eq!(
        batch(&mut controller, &lease, vec![key(30, KeyState::Down)])["ok"],
        true
    );
    assert_eq!(
        controller.handle(
            "owner",
            Operation::Release { lease },
            now + Duration::from_millis(101),
            &mut || Ok(())
        )["released"],
        true
    );
    assert!(!controller.backend.native_resource_owned);
}

#[test]
fn shutdown_retries_cleanup_of_a_partial_start_without_a_lease() {
    let resource_owned = Rc::new(std::cell::Cell::new(false));
    let mut controller = Controller::new(
        FakeBackend {
            fail_start_once: true,
            fail_stop: true,
            native_resource_trace: Some(resource_owned.clone()),
            ..Default::default()
        },
        true,
    );
    let now = Instant::now();
    let failed = controller.handle("owner", acquire_operation(), now, &mut || Ok(()));
    assert_eq!(failed["error"]["code"], "injection_failed");
    assert_eq!(failed["cleanup_required"], true);
    assert!(resource_owned.get());
    controller.backend.fail_stop = false;

    drop(controller);
    assert!(!resource_owned.get());
}

#[test]
fn scoped_timeout_is_snapshotted_for_a_lease_and_global_updates_apply_after_release() {
    let mut controller = Controller::new(
        FakeBackend {
            back_button_timeout_ms: -1,
            ..Default::default()
        },
        true,
    );
    controller.set_back_button_timeout_ms(80);
    let now = Instant::now();
    let acquired = controller.handle(
        "client-one",
        Operation::Acquire {
            operation_id: Some("client-one-stream".into()),
            ttl_ms: None,
            back_button_timeout: Some(-1),
            controller: None,
            gamepad: None,
            ds4_back_as_touchpad_click: None,
            motion_as_ds4: None,
            touchpad_as_ds4: None,
            ds5_inputtino_randomize_mac: None,
            native_pen_touch: None,
            keyboard: None,
            mouse: None,
            high_resolution_scrolling: None,
            always_send_scancodes: None,
            key_rightalt_to_key_win: None,
            keybindings: None,
            key_repeat_delay: None,
            key_repeat_frequency: None,
        },
        now,
        &mut || Ok(()),
    );
    assert_eq!(acquired["back_button_timeout"], -1);
    assert_eq!(controller.backend.back_button_timeout_ms, -1);

    controller.set_back_button_timeout_ms(250);
    assert_eq!(controller.backend.back_button_timeout_ms, -1);
    controller.handle(
        "client-one",
        Operation::Release {
            lease: acquired["lease"].as_str().unwrap().into(),
        },
        now,
        &mut || Ok(()),
    );
    assert_eq!(controller.backend.back_button_timeout_ms, 250);

    let next = controller.handle(
        "client-two",
        Operation::Acquire {
            operation_id: Some("client-two-stream".into()),
            ttl_ms: None,
            back_button_timeout: None,
            controller: None,
            gamepad: None,
            ds4_back_as_touchpad_click: None,
            motion_as_ds4: None,
            touchpad_as_ds4: None,
            ds5_inputtino_randomize_mac: None,
            native_pen_touch: None,
            keyboard: None,
            mouse: None,
            high_resolution_scrolling: None,
            always_send_scancodes: None,
            key_rightalt_to_key_win: None,
            keybindings: None,
            key_repeat_delay: None,
            key_repeat_frequency: None,
        },
        now,
        &mut || Ok(()),
    );
    assert_eq!(next["back_button_timeout"], 250);
    assert_eq!(controller.backend.back_button_timeout_ms, 250);
}

#[test]
fn scoped_controller_auto_selection_survives_global_updates_and_restores_after_release() {
    let mut controller = Controller::new(FakeBackend::default(), true);
    let now = Instant::now();
    let acquired = controller.handle(
        "owner",
        Operation::Acquire {
            operation_id: Some("ds4-back-mapping".into()),
            ttl_ms: None,
            back_button_timeout: None,
            controller: None,
            gamepad: Some(GamepadProfile::Auto),
            ds4_back_as_touchpad_click: Some(false),
            motion_as_ds4: Some(false),
            touchpad_as_ds4: Some(false),
            ds5_inputtino_randomize_mac: Some(false),
            native_pen_touch: None,
            keyboard: None,
            mouse: None,
            high_resolution_scrolling: None,
            always_send_scancodes: None,
            key_rightalt_to_key_win: None,
            keybindings: None,
            key_repeat_delay: None,
            key_repeat_frequency: None,
        },
        now,
        &mut || Ok(()),
    );
    assert_eq!(acquired["ok"], true);
    assert_eq!(acquired["ds4_back_as_touchpad_click"], false);
    assert_eq!(acquired["motion_as_ds4"], false);
    assert_eq!(acquired["touchpad_as_ds4"], false);
    assert_eq!(acquired["ds5_inputtino_randomize_mac"], false);
    let lease = controller.lease.as_ref().unwrap();
    assert_eq!(lease.requested_ds4_back_as_touchpad_click, Some(false));
    assert!(!lease.ds4_back_as_touchpad_click);
    assert!(!lease.motion_as_ds4);
    assert!(!lease.touchpad_as_ds4);
    assert!(!lease.ds5_inputtino_randomize_mac);
    assert!(!controller.backend.ds4_back_as_touchpad_click);
    assert!(!controller.backend.motion_as_ds4);
    assert!(!controller.backend.touchpad_as_ds4);
    assert!(!controller.backend.ds5_inputtino_randomize_mac);

    controller.set_ds4_back_as_touchpad_click(false);
    controller.set_ds4_back_as_touchpad_click(true);
    controller.set_motion_as_ds4(false);
    controller.set_motion_as_ds4(true);
    controller.set_touchpad_as_ds4(false);
    controller.set_touchpad_as_ds4(true);
    controller.set_ds5_inputtino_randomize_mac(false);
    controller.set_ds5_inputtino_randomize_mac(true);
    assert!(!controller.backend.ds4_back_as_touchpad_click);
    assert!(!controller.backend.motion_as_ds4);
    assert!(!controller.backend.touchpad_as_ds4);
    assert!(!controller.backend.ds5_inputtino_randomize_mac);

    controller.handle(
        "owner",
        Operation::Release {
            lease: acquired["lease"].as_str().unwrap().into(),
        },
        now,
        &mut || Ok(()),
    );
    assert!(controller.backend.ds4_back_as_touchpad_click);
    assert!(controller.backend.motion_as_ds4);
    assert!(controller.backend.touchpad_as_ds4);
    assert!(controller.backend.ds5_inputtino_randomize_mac);
}

#[test]
fn controller_touch_release_cancels_contacts_owned_by_the_lease() {
    let mut controller = Controller::new(FakeBackend::default(), true);
    let now = Instant::now();
    let lease = acquire(&mut controller, now);
    let down = InputEvent::ControllerTouch {
        number: 0,
        touchpad_index: 1,
        pointer_id: 42,
        phase: ControllerTouchPhase::Down,
        x: 0.25,
        y: 0.75,
        pressure: 0.5,
    };
    assert_eq!(
        batch(&mut controller, &lease, vec![down.clone()])["ok"],
        true
    );

    let release = controller.handle("owner", Operation::Release { lease }, now, &mut || Ok(()));
    assert_eq!(release["released"], true);
    assert!(controller.controller_touch_contacts.is_empty());
    assert_eq!(
        controller.backend.trace.borrow().last(),
        Some(&InputEvent::ControllerTouch {
            number: 0,
            touchpad_index: 1,
            pointer_id: 42,
            phase: ControllerTouchPhase::Cancel,
            x: 0.25,
            y: 0.75,
            pressure: 0.5,
        })
    );
}

#[test]
fn unscoped_ds4_back_mapping_is_snapshotted_from_the_v9_default() {
    let mut controller = Controller::new(FakeBackend::default(), true);
    let now = Instant::now();
    let acquire = |ds4_back_as_touchpad_click| Operation::Acquire {
        operation_id: Some("ds4-back-default-retry".into()),
        ttl_ms: None,
        back_button_timeout: None,
        controller: None,
        gamepad: None,
        ds4_back_as_touchpad_click,
        motion_as_ds4: None,
        touchpad_as_ds4: None,
        ds5_inputtino_randomize_mac: None,
        keyboard: None,
        mouse: None,
        native_pen_touch: None,
        high_resolution_scrolling: None,
        always_send_scancodes: None,
        key_rightalt_to_key_win: None,
        keybindings: None,
        key_repeat_delay: None,
        key_repeat_frequency: None,
    };

    let acquired = controller.handle("owner", acquire(None), now, &mut || Ok(()));
    assert_eq!(acquired["ok"], true);
    assert_eq!(acquired["ds4_back_as_touchpad_click"], true);
    let lease = controller.lease.as_ref().unwrap();
    assert_eq!(lease.requested_ds4_back_as_touchpad_click, None);
    assert!(lease.ds4_back_as_touchpad_click);

    controller.set_ds4_back_as_touchpad_click(false);
    assert!(!controller.ds4_back_as_touchpad_click());
    assert!(controller.effective_ds4_back_as_touchpad_click());
    assert!(controller.backend.ds4_back_as_touchpad_click);
    assert_eq!(
        controller.status("owner", now)["lease"]["ds4_back_as_touchpad_click"],
        true
    );

    let retry = controller.handle("owner", acquire(None), now, &mut || Ok(()));
    assert_eq!(retry["ok"], true);
    assert_eq!(retry["ds4_back_as_touchpad_click"], true);
    let changed_retry = controller.handle("owner", acquire(Some(false)), now, &mut || Ok(()));
    assert_eq!(changed_retry["error"]["code"], "operation_conflict");

    controller.handle(
        "owner",
        Operation::Release {
            lease: acquired["lease"].as_str().unwrap().into(),
        },
        now,
        &mut || Ok(()),
    );
    assert!(!controller.backend.ds4_back_as_touchpad_click);
}

#[test]
fn native_pen_touch_resolution_is_snapshotted_and_retry_identity_is_optional_value() {
    let mut controller = Controller::new(FakeBackend::default(), true);
    let now = Instant::now();
    let acquire = |operation_id: &str, native_pen_touch| Operation::Acquire {
        operation_id: Some(operation_id.into()),
        ttl_ms: None,
        back_button_timeout: None,
        controller: None,
        gamepad: None,
        ds4_back_as_touchpad_click: None,
        motion_as_ds4: None,
        touchpad_as_ds4: None,
        ds5_inputtino_randomize_mac: None,
        native_pen_touch,
        keyboard: None,
        mouse: None,
        high_resolution_scrolling: None,
        always_send_scancodes: None,
        key_rightalt_to_key_win: None,
        keybindings: None,
        key_repeat_delay: None,
        key_repeat_frequency: None,
    };
    let release = |controller: &mut Controller<FakeBackend>, response: &serde_json::Value| {
        controller.handle(
            "owner",
            Operation::Release {
                lease: response["lease"].as_str().unwrap().into(),
            },
            now,
            &mut || Ok(()),
        )
    };

    let inherited = controller.handle(
        "owner",
        acquire("pen-touch-default", None),
        now,
        &mut || Ok(()),
    );
    assert_eq!(inherited["native_pen_touch"], true);
    let token = inherited["lease"].clone();
    let retry = controller.handle(
        "owner",
        acquire("pen-touch-default", None),
        now,
        &mut || Ok(()),
    );
    assert_eq!(retry["lease"], token);
    assert_eq!(retry["native_pen_touch"], true);

    controller.set_native_pen_touch(false);
    let status = controller.status("owner", now);
    let lookup = controller.handle(
        "owner",
        Operation::Lookup {
            operation_id: "pen-touch-default".into(),
        },
        now,
        &mut || Ok(()),
    );
    assert_eq!(status["native_pen_touch"], false);
    assert_eq!(status["lease"]["native_pen_touch"], true);
    assert_eq!(lookup["native_pen_touch"], true);
    assert_eq!(
        controller.handle(
            "owner",
            acquire("pen-touch-default", Some(true)),
            now,
            &mut || Ok(())
        )["error"]["code"],
        "operation_conflict"
    );
    release(&mut controller, &inherited);
    let terminal_lookup = controller.handle(
        "owner",
        Operation::Lookup {
            operation_id: "pen-touch-default".into(),
        },
        now,
        &mut || Ok(()),
    );
    assert_eq!(terminal_lookup["native_pen_touch"], true);
    assert_eq!(
        controller.handle(
            "owner",
            acquire("pen-touch-default", Some(true)),
            now,
            &mut || Ok(())
        )["error"]["code"],
        "operation_conflict"
    );

    let next_inherited = controller.handle(
        "owner",
        acquire("pen-touch-next-default", None),
        now,
        &mut || Ok(()),
    );
    assert_eq!(next_inherited["native_pen_touch"], false);
    release(&mut controller, &next_inherited);

    controller.set_native_pen_touch(true);
    let explicitly_disabled = controller.handle(
        "owner",
        acquire("pen-touch-disabled", Some(false)),
        now,
        &mut || Ok(()),
    );
    assert_eq!(explicitly_disabled["native_pen_touch"], false);
    release(&mut controller, &explicitly_disabled);

    controller.set_native_pen_touch(false);
    let explicitly_enabled = controller.handle(
        "owner",
        acquire("pen-touch-enabled", Some(true)),
        now,
        &mut || Ok(()),
    );
    assert_eq!(explicitly_enabled["native_pen_touch"], true);
}

#[test]
fn acquire_retry_with_a_different_ds4_back_mapping_conflicts() {
    let mut controller = Controller::new(FakeBackend::default(), true);
    let now = Instant::now();
    let acquire = |enabled| Operation::Acquire {
        operation_id: Some("ds4-back-retry".into()),
        ttl_ms: None,
        back_button_timeout: None,
        controller: None,
        gamepad: None,
        ds4_back_as_touchpad_click: Some(enabled),
        motion_as_ds4: None,
        touchpad_as_ds4: None,
        ds5_inputtino_randomize_mac: None,
        native_pen_touch: None,
        keyboard: None,
        mouse: None,
        high_resolution_scrolling: None,
        always_send_scancodes: None,
        key_rightalt_to_key_win: None,
        keybindings: None,
        key_repeat_delay: None,
        key_repeat_frequency: None,
    };
    assert_eq!(
        controller.handle("owner", acquire(false), now, &mut || Ok(()))["ok"],
        true
    );
    assert_eq!(
        controller.handle("owner", acquire(true), now, &mut || Ok(()))["error"]["code"],
        "operation_conflict"
    );
}

#[test]
fn acquire_retry_distinguishes_zero_key_repeat_delay_from_unspecified() {
    let mut controller = Controller::new(FakeBackend::default(), true);
    controller.set_key_repeat_policy(500, 25.0);
    let now = Instant::now();
    let acquire = |key_repeat_delay| Operation::Acquire {
        operation_id: Some("key-repeat-delay-retry".into()),
        ttl_ms: None,
        back_button_timeout: None,
        controller: None,
        gamepad: None,
        ds4_back_as_touchpad_click: None,
        motion_as_ds4: None,
        touchpad_as_ds4: None,
        ds5_inputtino_randomize_mac: None,
        native_pen_touch: None,
        keyboard: None,
        mouse: None,
        high_resolution_scrolling: None,
        always_send_scancodes: None,
        key_rightalt_to_key_win: None,
        keybindings: None,
        key_repeat_delay,
        key_repeat_frequency: None,
    };

    let acquired = controller.handle("owner", acquire(Some(0)), now, &mut || Ok(()));
    assert_eq!(acquired["ok"], true);
    assert_eq!(acquired["key_repeat_delay"], 0);
    let token = acquired["lease"].as_str().unwrap();
    assert_eq!(controller.effective_key_repeat_policy(), (0, 25.0));

    assert_eq!(
        batch(&mut controller, token, vec![key(30, KeyState::Down)])["ok"],
        true
    );
    assert!(controller.repeating_key.is_none());
    let event_count = controller.backend.trace.borrow().len();
    controller.expire(Instant::now() + Duration::from_millis(500));
    assert_eq!(controller.backend.trace.borrow().len(), event_count);

    let retry = controller.handle("owner", acquire(Some(0)), now, &mut || Ok(()));
    assert_eq!(retry["ok"], true);
    assert_eq!(retry["lease"], token);

    let changed_retry = controller.handle("owner", acquire(None), now, &mut || Ok(()));
    assert_eq!(changed_retry["error"]["code"], "operation_conflict");
    let lease = controller.lease.as_ref().unwrap();
    assert_eq!(lease.token, token);
    assert_eq!(lease.requested_key_repeat_delay, Some(0));
    assert_eq!(controller.effective_key_repeat_policy(), (0, 25.0));
    assert_eq!(controller.pressed.len(), 1);
}

#[test]
fn acquired_key_repeat_frequency_controls_cadence_and_retry_identity() {
    let backend = FakeBackend::default();
    let trace = backend.trace.clone();
    let mut controller = Controller::new(backend, true);
    controller.set_key_repeat_policy(500, 100.0);
    let start = Instant::now();
    let acquire = |frequency| Operation::Acquire {
        operation_id: Some("key-repeat-frequency-cadence".into()),
        ttl_ms: Some(5_000),
        back_button_timeout: None,
        controller: None,
        gamepad: None,
        ds4_back_as_touchpad_click: None,
        motion_as_ds4: None,
        touchpad_as_ds4: None,
        ds5_inputtino_randomize_mac: None,
        native_pen_touch: None,
        keyboard: None,
        mouse: None,
        high_resolution_scrolling: None,
        always_send_scancodes: None,
        key_rightalt_to_key_win: None,
        keybindings: None,
        key_repeat_delay: Some(100),
        key_repeat_frequency: Some(frequency),
    };

    let acquired = controller.handle("owner", acquire(20.0), start, &mut || Ok(()));
    assert_eq!(acquired["ok"], true);
    assert_eq!(acquired["key_repeat_frequency"], 20.0);
    let token = acquired["lease"].as_str().unwrap();
    assert_eq!(controller.effective_key_repeat_policy(), (100, 20.0));

    let key_down_at = start + Duration::from_millis(10);
    let key_down = controller.handle(
        "owner",
        Operation::Batch {
            lease: token.into(),
            events: vec![key(30, KeyState::Down)],
        },
        key_down_at,
        &mut || Ok(()),
    );
    assert_eq!(key_down["ok"], true);
    assert_eq!(trace.borrow().as_slice(), &[key(30, KeyState::Down)]);
    assert_eq!(
        controller.repeating_key.as_ref().unwrap().next_due,
        key_down_at + Duration::from_millis(100)
    );

    controller.expire(key_down_at + Duration::from_millis(99));
    assert_eq!(trace.borrow().len(), 1);
    controller.expire(key_down_at + Duration::from_millis(100));
    assert_eq!(
        trace.borrow().as_slice(),
        &[key(30, KeyState::Down), key(30, KeyState::Repeat)]
    );
    assert_eq!(
        controller.repeating_key.as_ref().unwrap().next_due,
        key_down_at + Duration::from_millis(150)
    );

    controller.expire(key_down_at + Duration::from_millis(149));
    assert_eq!(trace.borrow().len(), 2);
    controller.expire(key_down_at + Duration::from_millis(150));
    assert_eq!(
        trace.borrow().as_slice(),
        &[
            key(30, KeyState::Down),
            key(30, KeyState::Repeat),
            key(30, KeyState::Repeat),
        ]
    );

    let retry_at = key_down_at + Duration::from_millis(150);
    let retry = controller.handle("owner", acquire(20.0), retry_at, &mut || Ok(()));
    assert_eq!(retry["ok"], true);
    assert_eq!(retry["lease"], token);

    let changed_retry = controller.handle("owner", acquire(40.0), retry_at, &mut || Ok(()));
    assert_eq!(changed_retry["error"]["code"], "operation_conflict");
    assert_eq!(controller.lease.as_ref().unwrap().token, token);
    assert_eq!(controller.effective_key_repeat_policy(), (100, 20.0));
}

#[test]
fn acquire_retry_with_a_different_scoped_timeout_conflicts() {
    let mut controller = Controller::new(FakeBackend::default(), true);
    let now = Instant::now();
    let first = controller.handle(
        "owner",
        Operation::Acquire {
            operation_id: Some("timeout-retry".into()),
            ttl_ms: Some(2000),
            back_button_timeout: Some(400),
            controller: None,
            gamepad: None,
            ds4_back_as_touchpad_click: None,
            motion_as_ds4: None,
            touchpad_as_ds4: None,
            ds5_inputtino_randomize_mac: None,
            native_pen_touch: None,
            keyboard: None,
            mouse: None,
            high_resolution_scrolling: None,
            always_send_scancodes: None,
            key_rightalt_to_key_win: None,
            keybindings: None,
            key_repeat_delay: None,
            key_repeat_frequency: None,
        },
        now,
        &mut || Ok(()),
    );
    assert_eq!(first["ok"], true);
    let retry = controller.handle(
        "owner",
        Operation::Acquire {
            operation_id: Some("timeout-retry".into()),
            ttl_ms: Some(2000),
            back_button_timeout: None,
            controller: None,
            gamepad: None,
            ds4_back_as_touchpad_click: None,
            motion_as_ds4: None,
            touchpad_as_ds4: None,
            ds5_inputtino_randomize_mac: None,
            native_pen_touch: None,
            keyboard: None,
            mouse: None,
            high_resolution_scrolling: None,
            always_send_scancodes: None,
            key_rightalt_to_key_win: None,
            keybindings: None,
            key_repeat_delay: None,
            key_repeat_frequency: None,
        },
        now,
        &mut || Ok(()),
    );
    assert_eq!(retry["error"]["code"], "operation_conflict");
}

#[test]
fn controller_capability_is_present_in_status_and_active_lease_responses() {
    let mut controller = Controller::new(
        FakeBackend {
            controller_input_supported: true,
            effective_gamepad_profile: Some("x360"),
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
            back_button_timeout: None,
            controller: None,
            gamepad: None,
            ds4_back_as_touchpad_click: None,
            motion_as_ds4: None,
            touchpad_as_ds4: None,
            ds5_inputtino_randomize_mac: None,
            native_pen_touch: None,
            keyboard: None,
            mouse: None,
            high_resolution_scrolling: None,
            always_send_scancodes: None,
            key_rightalt_to_key_win: None,
            keybindings: None,
            key_repeat_delay: None,
            key_repeat_frequency: None,
        },
        now,
        &mut || Ok(()),
    );
    assert_eq!(acquired["controller_input"], true);
    assert_eq!(acquired["gamepad_profile"], "auto");
    assert_eq!(acquired["effective_gamepad_profile"], "x360");

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
    assert_eq!(status["effective_gamepad_profile"], "x360");
    assert_eq!(status["supported_profiles"][0], "auto");
}

#[test]
fn active_gamepad_readback_tracks_recovery_without_changing_lease_identity() {
    let mut controller = Controller::new(
        FakeBackend {
            gamepad_unavailable_reason: Some("bus unavailable"),
            ..Default::default()
        },
        true,
    );
    let now = Instant::now();
    let operation = || {
        let mut request = acquire_operation();
        if let Operation::Acquire {
            operation_id,
            gamepad,
            ..
        } = &mut request
        {
            *operation_id = Some("reconnecting-gamepad".into());
            *gamepad = Some(selected_supported_profile());
        }
        request
    };
    let acquired = controller.handle("owner", operation(), now, &mut || Ok(()));
    let first_token = acquired["lease"].as_str().unwrap().to_owned();
    assert_eq!(
        acquired["gamepad_profile"],
        selected_supported_profile().as_str()
    );
    assert_eq!(acquired["controller_input"], false);
    assert_eq!(acquired["effective_gamepad_profile"], Value::Null);
    assert_eq!(acquired["gamepad_unavailable_reason"], "bus unavailable");

    controller.backend.controller_input_supported = true;
    controller.backend.effective_gamepad_profile = Some("x360");
    controller.backend.gamepad_unavailable_reason = None;
    let recovered = controller.handle("owner", operation(), now, &mut || Ok(()));
    let lookup = controller.handle(
        "owner",
        Operation::Lookup {
            operation_id: "reconnecting-gamepad".into(),
        },
        now,
        &mut || Ok(()),
    );
    let status = controller.handle("owner", Operation::Status, now, &mut || Ok(()));
    for readback in [&recovered, &lookup] {
        assert_eq!(readback["lease"], first_token);
        assert_eq!(
            readback["gamepad_profile"],
            selected_supported_profile().as_str()
        );
        assert_eq!(readback["controller_input"], true);
        assert_eq!(readback["effective_gamepad_profile"], "x360");
        assert_eq!(readback["gamepad_unavailable_reason"], Value::Null);
    }
    assert_eq!(status["effective_gamepad_profile"], "x360");
    assert_eq!(status["lease"]["effective_gamepad_profile"], "x360");
    assert_eq!(status["lease"]["gamepad_unavailable_reason"], Value::Null);

    controller.backend.controller_input_supported = false;
    controller.backend.effective_gamepad_profile = None;
    controller.backend.gamepad_unavailable_reason = Some("bus lost");
    let lost = controller.handle("owner", Operation::Status, now, &mut || Ok(()));
    assert_eq!(lost["controller_input"], false);
    assert_eq!(lost["effective_gamepad_profile"], Value::Null);
    assert_eq!(lost["lease"]["gamepad_unavailable_reason"], "bus lost");
    let other = controller.handle("other", Operation::Status, now, &mut || Ok(()));
    assert_eq!(other["lease"]["token"], Value::Null);
    assert_eq!(other["lease"]["operation_id"], Value::Null);
    let other_lookup = controller.handle(
        "other",
        Operation::Lookup {
            operation_id: "reconnecting-gamepad".into(),
        },
        now,
        &mut || Ok(()),
    );
    assert_eq!(other_lookup["error"]["code"], "not_found");

    assert_eq!(
        controller.handle(
            "owner",
            Operation::Release {
                lease: first_token.clone(),
            },
            now,
            &mut || Ok(()),
        )["released"],
        true
    );
    controller.backend.controller_input_supported = true;
    controller.backend.effective_gamepad_profile = Some("x360");
    controller.backend.gamepad_unavailable_reason = None;
    let reacquired = controller.handle("owner", acquire_operation(), now, &mut || Ok(()));
    assert_ne!(reacquired["lease"], first_token);
    assert_eq!(reacquired["effective_gamepad_profile"], "x360");
    assert_eq!(reacquired["gamepad_profile"], "auto");
}

#[test]
fn active_lease_keeps_the_profile_selected_at_acquire_time() {
    let mut controller = Controller::new(
        FakeBackend {
            controller_input_supported: true,
            effective_gamepad_profile: Some("x360"),
            ..Default::default()
        },
        true,
    );
    let now = Instant::now();
    let acquired = controller.handle(
        "owner",
        Operation::Acquire {
            operation_id: Some("profile-snapshot".into()),
            ttl_ms: Some(1000),
            back_button_timeout: None,
            controller: None,
            gamepad: None,
            ds4_back_as_touchpad_click: None,
            motion_as_ds4: None,
            touchpad_as_ds4: None,
            ds5_inputtino_randomize_mac: None,
            native_pen_touch: None,
            keyboard: None,
            mouse: None,
            high_resolution_scrolling: None,
            always_send_scancodes: None,
            key_rightalt_to_key_win: None,
            keybindings: None,
            key_repeat_delay: None,
            key_repeat_frequency: None,
        },
        now,
        &mut || Ok(()),
    );
    let updated_global_profile = selected_supported_profile();
    controller.set_gamepad_profile(updated_global_profile);

    let status = controller.handle("owner", Operation::Status, now, &mut || Ok(()));
    assert_eq!(acquired["gamepad_profile"], "auto");
    assert_eq!(status["gamepad"], updated_global_profile.as_str());
    assert_eq!(controller.backend.gamepad_profile, GamepadProfile::Auto);
    assert_eq!(status["lease"]["gamepad_profile"], "auto");
    assert_eq!(
        status["effective_gamepad_profile"],
        acquired["effective_gamepad_profile"]
    );
    assert_eq!(
        status["lease"]["effective_gamepad_profile"],
        acquired["effective_gamepad_profile"]
    );

    let released = controller.handle(
        "owner",
        Operation::Release {
            lease: acquired["lease"].as_str().unwrap().into(),
        },
        now,
        &mut || Ok(()),
    );
    assert_eq!(released["released"], true);
    assert_eq!(controller.backend.gamepad_profile, updated_global_profile);
}

#[test]
fn scoped_input_policy_overrides_host_defaults_then_restores_them_on_release() {
    let mut controller = Controller::new(
        FakeBackend {
            controller_input_supported: true,
            ..Default::default()
        },
        true,
    );
    let now = Instant::now();
    let global_gamepad = selected_supported_profile();
    controller.set_gamepad_profile(global_gamepad);
    controller.set_controller_enabled(false, now).unwrap();
    controller
        .set_device_policy(
            DevicePolicy {
                keyboard: false,
                mouse: false,
            },
            now,
        )
        .unwrap();

    let acquired = controller.handle(
        "owner",
        Operation::Acquire {
            operation_id: Some("scoped-input-policy".into()),
            ttl_ms: Some(1_000),
            back_button_timeout: None,
            controller: Some(true),
            gamepad: Some(GamepadProfile::Auto),
            ds4_back_as_touchpad_click: None,
            motion_as_ds4: None,
            touchpad_as_ds4: None,
            ds5_inputtino_randomize_mac: None,
            native_pen_touch: None,
            keyboard: Some(true),
            mouse: Some(false),
            high_resolution_scrolling: None,
            always_send_scancodes: None,
            key_rightalt_to_key_win: None,
            keybindings: None,
            key_repeat_delay: None,
            key_repeat_frequency: None,
        },
        now,
        &mut || Ok(()),
    );
    assert_eq!(acquired["ok"], true);
    assert_eq!(acquired["gamepad_profile"], "auto");
    assert_eq!(controller.backend.gamepad_profile, GamepadProfile::Auto);

    let lease = acquired["lease"].as_str().unwrap();
    let mixed = batch(
        &mut controller,
        lease,
        vec![
            key(30, KeyState::Down),
            Event::Button {
                button: Button::Left,
                pressed: true,
            },
            controller_event(),
        ],
    );
    assert_eq!(mixed["ok"], true);
    assert_eq!(mixed["accepted"], 3);
    assert_eq!(mixed["suppressed"], 1);
    assert_eq!(controller.backend.trace.borrow().len(), 2);
    assert_eq!(
        controller.backend.active_controller_slots,
        BTreeSet::from([0])
    );

    // Host changes stay recorded as defaults while an active session keeps its
    // explicit policy; in particular, it can still release its held key.
    controller
        .set_device_policy(
            DevicePolicy {
                keyboard: false,
                mouse: true,
            },
            now,
        )
        .unwrap();
    let key_up = batch(&mut controller, lease, vec![key(30, KeyState::Up)]);
    assert_eq!(key_up["ok"], true);
    assert_eq!(key_up["suppressed"], 0);

    controller.handle(
        "owner",
        Operation::Release {
            lease: lease.into(),
        },
        now,
        &mut || Ok(()),
    );
    assert!(controller.lease.is_none());
    assert_eq!(controller.backend.gamepad_profile, global_gamepad);
    assert!(!controller.controller_enabled());
    assert!(!controller.device_policy().keyboard);
    assert!(controller.device_policy().mouse);

    let next_lease = acquire(&mut controller, now);
    let suppressed = batch(&mut controller, &next_lease, vec![controller_event()]);
    assert_eq!(suppressed["suppressed"], 1);
    controller.handle(
        "owner",
        Operation::Release { lease: next_lease },
        now,
        &mut || Ok(()),
    );

    let second_owner = controller.handle(
        "second-owner",
        Operation::Acquire {
            operation_id: Some("second-owner-stream".into()),
            ttl_ms: Some(1_000),
            back_button_timeout: None,
            controller: Some(true),
            gamepad: Some(global_gamepad),
            ds4_back_as_touchpad_click: None,
            motion_as_ds4: None,
            touchpad_as_ds4: None,
            ds5_inputtino_randomize_mac: None,
            native_pen_touch: None,
            keyboard: Some(true),
            mouse: None,
            high_resolution_scrolling: None,
            always_send_scancodes: None,
            key_rightalt_to_key_win: None,
            keybindings: None,
            key_repeat_delay: None,
            key_repeat_frequency: None,
        },
        now,
        &mut || Ok(()),
    );
    assert_eq!(second_owner["ok"], true);
    assert_eq!(second_owner["gamepad_profile"], global_gamepad.as_str());
    assert_eq!(controller.backend.gamepad_profile, global_gamepad);
    let second_input = controller.handle(
        "second-owner",
        Operation::Batch {
            lease: second_owner["lease"].as_str().unwrap().into(),
            events: vec![controller_event()],
        },
        now,
        &mut || Ok(()),
    );
    assert_eq!(second_input["ok"], true);
    assert_eq!(second_input["suppressed"], 0);
    controller.handle(
        "second-owner",
        Operation::Release {
            lease: second_owner["lease"].as_str().unwrap().into(),
        },
        now,
        &mut || Ok(()),
    );
    assert_eq!(controller.backend.gamepad_profile, global_gamepad);
}

#[test]
fn scoped_keyboard_and_scroll_overrides_hold_through_cleanup_and_restore_defaults() {
    let mut controller = Controller::new(FakeBackend::default(), true);
    controller.set_high_resolution_scrolling(true);
    controller.set_always_send_scancodes(true);
    controller.set_key_rightalt_to_key_win(false);
    controller.set_key_repeat_policy(500, 24.9);
    let now = Instant::now();
    let acquired = controller.handle(
        "owner",
        Operation::Acquire {
            operation_id: Some("keyboard-policy-owner".into()),
            ttl_ms: Some(1_000),
            back_button_timeout: None,
            controller: None,
            gamepad: None,
            ds4_back_as_touchpad_click: None,
            motion_as_ds4: None,
            touchpad_as_ds4: None,
            ds5_inputtino_randomize_mac: None,
            native_pen_touch: None,
            keyboard: None,
            mouse: None,
            high_resolution_scrolling: Some(false),
            always_send_scancodes: Some(false),
            key_rightalt_to_key_win: Some(true),
            keybindings: None,
            key_repeat_delay: Some(0),
            key_repeat_frequency: Some(0.0),
        },
        now,
        &mut || Ok(()),
    );
    assert_eq!(acquired["ok"], true);
    assert_eq!(acquired["high_resolution_scrolling"], false);
    assert_eq!(acquired["always_send_scancodes"], false);
    assert_eq!(acquired["key_rightalt_to_key_win"], true);
    assert_eq!(acquired["key_repeat_delay"], 0);
    assert_eq!(acquired["key_repeat_frequency"], 0.0);
    assert!(!controller.backend.always_send_scancodes);
    assert!(controller.backend.key_rightalt_to_key_win);

    let token = acquired["lease"].as_str().unwrap();
    let right_alt_down = Event::Key {
        key: Key::MoonlightVk {
            code: 0xa5,
            normalized: false,
            repeat_modifiers: 0,
            temporary_modifier: false,
        },
        state: KeyState::Down,
    };
    assert_eq!(
        batch(
            &mut controller,
            token,
            vec![
                right_alt_down,
                Event::ScrollUnits {
                    axis: Axis::Vertical,
                    amount: 60,
                },
            ]
        )["ok"],
        true
    );
    assert!(controller.repeating_key.is_none());
    assert_eq!(controller.vertical_scroll_remainder, 60);

    // Host updates remain visible as defaults without changing an explicit
    // stream selection or the keyboard mapping used for its eventual key-up.
    controller.set_high_resolution_scrolling(true);
    controller.set_always_send_scancodes(true);
    controller.set_key_rightalt_to_key_win(false);
    controller.set_key_repeat_policy(10, 100.0);
    assert!(!controller.effective_high_resolution_scrolling());
    assert!(!controller.effective_always_send_scancodes());
    assert!(controller.effective_key_rightalt_to_key_win());
    assert_eq!(controller.effective_key_repeat_policy(), (0, 0.0));
    assert!(!controller.backend.always_send_scancodes);
    assert!(controller.backend.key_rightalt_to_key_win);
    assert_eq!(
        batch(
            &mut controller,
            token,
            vec![Event::ScrollUnits {
                axis: Axis::Vertical,
                amount: 60,
            }]
        )["ok"],
        true
    );
    assert_eq!(controller.vertical_scroll_remainder, 0);
    assert_eq!(
        controller.backend.trace.borrow().last(),
        Some(&Event::Scroll {
            axis: Axis::Vertical,
            steps: 1,
        })
    );

    assert_eq!(
        controller.handle(
            "owner",
            Operation::Release {
                lease: token.into()
            },
            now,
            &mut || Ok(()),
        )["released"],
        true
    );
    assert_eq!(
        controller.backend.right_alt_mapping_trace,
        [(KeyState::Down, true), (KeyState::Up, true)]
    );
    assert!(controller.backend.always_send_scancodes);
    assert!(!controller.backend.key_rightalt_to_key_win);
    assert_eq!(controller.key_repeat_policy(), (10, 100.0));
    assert!(controller.high_resolution_scrolling());

    let next = controller.handle(
        "second-owner",
        Operation::Acquire {
            operation_id: None,
            ttl_ms: Some(1_000),
            back_button_timeout: None,
            controller: None,
            gamepad: None,
            ds4_back_as_touchpad_click: None,
            motion_as_ds4: None,
            touchpad_as_ds4: None,
            ds5_inputtino_randomize_mac: None,
            native_pen_touch: None,
            keyboard: None,
            mouse: None,
            high_resolution_scrolling: None,
            always_send_scancodes: None,
            key_rightalt_to_key_win: None,
            keybindings: None,
            key_repeat_delay: None,
            key_repeat_frequency: None,
        },
        now + Duration::from_millis(1),
        &mut || Ok(()),
    );
    assert_eq!(next["ok"], true, "{next}");
    let next_token = next["lease"].as_str().unwrap();
    let second_input = controller.handle(
        "second-owner",
        Operation::Batch {
            lease: next_token.into(),
            events: vec![key(30, KeyState::Down)],
        },
        now + Duration::from_millis(2),
        &mut || Ok(()),
    );
    assert_eq!(second_input["ok"], true, "{second_input}");
    assert_eq!(
        controller.repeating_key.as_ref().unwrap().period,
        repeat_period(100.0)
    );
}

#[test]
fn scoped_repeat_overrides_reject_values_outside_product_policy_bounds() {
    for (key, value) in [
        ("key_repeat_delay", json!(i32::MAX as u64 + 1)),
        ("key_repeat_frequency", json!(-0.1)),
    ] {
        let mut controller = Controller::new(FakeBackend::default(), true);
        let mut payload = json!({"action":"acquire"});
        payload[key] = value;
        let operation = serde_json::from_value(payload).unwrap();
        let response = controller.handle("owner", operation, Instant::now(), &mut || Ok(()));
        assert_eq!(response["error"]["code"], "invalid_request");
        assert_eq!(controller.backend.starts, 0);
    }
}

fn selected_supported_profile() -> GamepadProfile {
    #[cfg(windows)]
    {
        GamepadProfile::X360
    }
    #[cfg(target_os = "linux")]
    {
        GamepadProfile::Xone
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        GamepadProfile::Auto
    }
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
fn controller_policy_live_disables_slots_without_stopping_keyboard_or_mouse() {
    let mut controller = Controller::new(
        FakeBackend {
            controller_input_supported: true,
            ..Default::default()
        },
        true,
    );
    let now = Instant::now();
    let token = acquire(&mut controller, now);
    assert_eq!(
        batch(&mut controller, &token, vec![controller_event()])["ok"],
        true
    );
    assert_eq!(
        controller.backend.active_controller_slots,
        BTreeSet::from([0])
    );

    assert!(controller.set_controller_enabled(false, now).is_ok());
    assert!(!controller.controller_enabled());
    assert!(controller.backend.active_controller_slots.is_empty());
    assert_eq!(controller.backend.controller_disable_calls, 1);

    let result = batch(
        &mut controller,
        &token,
        vec![
            controller_event(),
            key(31, KeyState::Down),
            key(31, KeyState::Up),
            Event::Relative { dx: 2, dy: 3 },
        ],
    );
    assert_eq!(result["ok"], true);
    assert_eq!(result["accepted"], 4);
    assert_eq!(result["suppressed"], 1);
    assert!(controller.backend.active_controller_slots.is_empty());

    assert!(
        controller
            .backend
            .trace
            .borrow()
            .iter()
            .any(|event| matches!(event, Event::Relative { dx: 2, dy: 3 }))
    );
    assert_eq!(
        controller
            .backend
            .trace
            .borrow()
            .iter()
            .filter(|event| matches!(event, Event::Controller { .. }))
            .count(),
        1,
    );

    controller
        .set_controller_enabled(true, Instant::now())
        .unwrap();
    assert!(controller.backend.active_controller_slots.is_empty());
    assert_eq!(
        batch(&mut controller, &token, vec![controller_event()])["ok"],
        true
    );
    assert_eq!(
        controller.backend.active_controller_slots,
        BTreeSet::from([0])
    );
    controller.handle(
        "owner",
        Operation::Release { lease: token },
        Instant::now(),
        &mut || Ok(()),
    );
}

#[test]
fn failed_controller_slot_removal_retries_without_blocking_keyboard_or_mouse() {
    let mut controller = Controller::new(
        FakeBackend {
            controller_input_supported: true,
            fail_controller_disable: true,
            ..Default::default()
        },
        true,
    );
    let now = Instant::now();
    controller.set_key_repeat_policy(10, 50.0);
    let token = acquire(&mut controller, now);
    assert_eq!(
        batch(
            &mut controller,
            &token,
            vec![controller_event(), key(30, KeyState::Down)],
        )["ok"],
        true
    );
    assert_eq!(
        controller.backend.active_controller_slots,
        BTreeSet::from([0])
    );

    assert!(controller.set_controller_enabled(false, now).is_err());
    assert!(controller.cleanup_required());
    assert!(controller.lease.is_some());
    let status = controller.handle("owner", Operation::Status, now, &mut || Ok(()));
    assert_eq!(status["controller_enabled"], false);
    assert_eq!(status["controller_cleanup_required"], true);
    assert_eq!(status["lease"]["cleanup_required"], false);

    let response = batch(
        &mut controller,
        &token,
        vec![
            controller_event(),
            key(31, KeyState::Down),
            Event::Relative { dx: -1, dy: 4 },
        ],
    );
    assert_eq!(response["ok"], true);
    assert_eq!(response["accepted"], 3);
    assert_eq!(response["suppressed"], 1);
    assert_eq!(
        controller.backend.active_controller_slots,
        BTreeSet::from([0])
    );

    controller.expire(Instant::now() + Duration::from_millis(11));
    assert!(
        controller
            .backend
            .trace
            .borrow()
            .iter()
            .any(|event| matches!(
                event,
                Event::Key {
                    key: Key::WindowsScan { code: 31, .. },
                    state: KeyState::Repeat
                }
            ))
    );
    assert_eq!(
        batch(&mut controller, &token, vec![key(31, KeyState::Up)])["ok"],
        true
    );

    controller.expire(Instant::now() + Duration::from_millis(101));
    assert!(!controller.cleanup_required());
    assert!(controller.backend.active_controller_slots.is_empty());
    assert_eq!(controller.backend.controller_disable_calls, 2);
    assert_eq!(
        batch(&mut controller, &token, vec![controller_event()])["suppressed"],
        1
    );

    controller
        .set_controller_enabled(true, Instant::now())
        .unwrap();
    assert!(controller.backend.active_controller_slots.is_empty());
    assert_eq!(
        batch(&mut controller, &token, vec![controller_event()])["ok"],
        true
    );
    assert_eq!(
        controller.backend.active_controller_slots,
        BTreeSet::from([0])
    );
}

#[test]
fn pen_touch_refresh_continues_while_controller_removal_retries() {
    let mut controller = Controller::new(
        FakeBackend {
            controller_input_supported: true,
            supports_pen_touch: true,
            fail_controller_disable: true,
            ..Default::default()
        },
        true,
    );
    let now = Instant::now();
    let lease = acquire(&mut controller, now);
    assert_eq!(
        batch(
            &mut controller,
            &lease,
            vec![
                controller_event(),
                pen_touch(7, PenTouchDevice::Touch, PenTouchPhase::Down, 100, 200),
            ],
        )["ok"],
        true
    );
    assert!(controller.set_controller_enabled(false, now).is_err());
    let full_advances = controller.backend.full_advances;

    controller.expire(now + Duration::from_millis(50));
    assert_eq!(controller.backend.full_advances, full_advances);
    assert!(controller.backend.pen_touch_advances > 0);
    assert_eq!(controller.pen_touch_contacts.len(), 1);
    assert_eq!(
        batch(
            &mut controller,
            &lease,
            vec![pen_touch(
                7,
                PenTouchDevice::Touch,
                PenTouchPhase::Move,
                101,
                201
            )],
        )["ok"],
        true
    );

    controller.expire(now + Duration::from_millis(100));
    assert!(!controller.cleanup_required());
    assert!(controller.backend.full_advances > full_advances);
    assert_eq!(
        batch(
            &mut controller,
            &lease,
            vec![pen_touch(
                7,
                PenTouchDevice::Touch,
                PenTouchPhase::Up,
                101,
                201
            )],
        )["ok"],
        true
    );
    assert_eq!(
        controller.handle("owner", Operation::Release { lease }, now, &mut || Ok(()))["released"],
        true
    );
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
            back_button_timeout: None,
            controller: None,
            gamepad: None,
            ds4_back_as_touchpad_click: None,
            motion_as_ds4: None,
            touchpad_as_ds4: None,
            ds5_inputtino_randomize_mac: None,
            native_pen_touch: None,
            keyboard: None,
            mouse: None,
            high_resolution_scrolling: None,
            always_send_scancodes: None,
            key_rightalt_to_key_win: None,
            keybindings: None,
            key_repeat_delay: None,
            key_repeat_frequency: None,
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
fn original_modifier_release_failure_preserves_owner_until_recovery() {
    let mut controller = Controller::new(FakeBackend::default(), true);
    let now = Instant::now();
    let token = acquire(&mut controller, now);
    let down = moonlight_key(0x10, KeyState::Down, 0, false);
    let up = moonlight_key(0x10, KeyState::Up, 0, false);
    assert_eq!(
        batch(&mut controller, &token, vec![down.clone()])["ok"],
        true
    );
    assert_eq!(controller.pressed.len(), 1);

    let unrelated = controller.handle(
        "other",
        Operation::Release {
            lease: token.clone(),
        },
        now,
        &mut || Ok(()),
    );
    assert_eq!(unrelated["error"]["code"], "permission_denied");
    assert_eq!(controller.pressed.len(), 1);

    controller.backend.fail_release = true;
    let failed = batch(&mut controller, &token, vec![up.clone()]);
    assert_eq!(failed["error"]["code"], "injection_failed");
    assert_eq!(failed["cleanup_required"], true);
    assert_eq!(controller.pressed.len(), 1);
    assert_eq!(
        controller.handle("other", acquire_operation(), now, &mut || Ok(()))["error"]["code"],
        "input_busy"
    );

    controller.backend.fail_release = false;
    controller.expire(now + Duration::from_millis(101));
    assert!(controller.lease.is_none());
    assert!(controller.pressed.is_empty());
    let trace = controller.backend.trace.borrow();
    let cleanup_up = moonlight_key(0x10, KeyState::Up, 0, false);
    assert_eq!(
        trace.as_slice(),
        &[down, cleanup_up.clone(), cleanup_up.clone(), cleanup_up]
    );
    drop(trace);

    let replacement = controller.handle("other", acquire_operation(), now, &mut || Ok(()));
    assert_eq!(replacement["ok"], true);
    assert_ne!(replacement["lease"], token);
    assert_eq!(
        controller.handle(
            "owner",
            Operation::Release { lease: token },
            now,
            &mut || Ok(()),
        )["error"]["code"],
        "permission_denied"
    );
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
            back_button_timeout: None,
            controller: None,
            gamepad: None,
            ds4_back_as_touchpad_click: None,
            motion_as_ds4: None,
            touchpad_as_ds4: None,
            ds5_inputtino_randomize_mac: None,
            native_pen_touch: None,
            keyboard: None,
            mouse: None,
            high_resolution_scrolling: None,
            always_send_scancodes: None,
            key_rightalt_to_key_win: None,
            keybindings: None,
            key_repeat_delay: None,
            key_repeat_frequency: None,
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
            back_button_timeout: None,
            controller: None,
            gamepad: None,
            ds4_back_as_touchpad_click: None,
            motion_as_ds4: None,
            touchpad_as_ds4: None,
            ds5_inputtino_randomize_mac: None,
            native_pen_touch: None,
            keyboard: None,
            mouse: None,
            high_resolution_scrolling: None,
            always_send_scancodes: None,
            key_rightalt_to_key_win: None,
            keybindings: None,
            key_repeat_delay: None,
            key_repeat_frequency: None,
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
            back_button_timeout: None,
            controller: None,
            gamepad: None,
            ds4_back_as_touchpad_click: None,
            motion_as_ds4: None,
            touchpad_as_ds4: None,
            ds5_inputtino_randomize_mac: None,
            native_pen_touch: None,
            keyboard: None,
            mouse: None,
            high_resolution_scrolling: None,
            always_send_scancodes: None,
            key_rightalt_to_key_win: None,
            keybindings: None,
            key_repeat_delay: None,
            key_repeat_frequency: None,
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
            back_button_timeout: None,
            controller: None,
            gamepad: None,
            ds4_back_as_touchpad_click: None,
            motion_as_ds4: None,
            touchpad_as_ds4: None,
            ds5_inputtino_randomize_mac: None,
            native_pen_touch: None,
            keyboard: None,
            mouse: None,
            high_resolution_scrolling: None,
            always_send_scancodes: None,
            key_rightalt_to_key_win: None,
            keybindings: None,
            key_repeat_delay: None,
            key_repeat_frequency: None,
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
        back_button_timeout: None,
        controller: None,
        gamepad: None,
        ds4_back_as_touchpad_click: None,
        motion_as_ds4: None,
        touchpad_as_ds4: None,
        ds5_inputtino_randomize_mac: None,
        native_pen_touch: None,
        keyboard: None,
        mouse: None,
        high_resolution_scrolling: None,
        always_send_scancodes: None,
        key_rightalt_to_key_win: None,
        keybindings: None,
        key_repeat_delay: None,
        key_repeat_frequency: None,
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
            back_button_timeout: None,
            controller: None,
            gamepad: None,
            ds4_back_as_touchpad_click: None,
            motion_as_ds4: None,
            touchpad_as_ds4: None,
            ds5_inputtino_randomize_mac: None,
            native_pen_touch: None,
            keyboard: None,
            mouse: None,
            high_resolution_scrolling: None,
            always_send_scancodes: None,
            key_rightalt_to_key_win: None,
            keybindings: None,
            key_repeat_delay: None,
            key_repeat_frequency: None,
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
fn explicitly_remapped_modifier_sources_are_distinct_and_native_encodings_cannot_mix() {
    let mut controller = Controller::new(FakeBackend::default(), true);
    controller.set_keybindings(&KeyBindings::from_flat(vec![0xa0, 0x41]).unwrap());
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
    let rejected = batch(
        &mut controller,
        &token,
        vec![generic_shift.clone(), left_shift_up.clone()],
    );
    assert_eq!(rejected["error"]["code"], "invalid_sequence");
    assert!(controller.pressed.is_empty());

    assert_eq!(
        batch(
            &mut controller,
            &token,
            vec![
                generic_shift.clone(),
                moonlight_key(0xa0, KeyState::Down, 0, false)
            ]
        )["ok"],
        true
    );
    assert_eq!(controller.pressed.len(), 2);
    assert_eq!(
        batch(
            &mut controller,
            &token,
            vec![left_shift_up, moonlight_key(0x10, KeyState::Up, 0, false)]
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
fn global_repeat_policy_updates_preserve_pending_deadlines_and_release_cancels() {
    let backend = FakeBackend::default();
    let trace = backend.trace.clone();
    let mut controller = Controller::new(backend, true);
    controller.set_key_repeat_policy(10, 100.0);
    let start = Instant::now();
    let token = acquire(&mut controller, start);
    let key_down_at = start + Duration::from_millis(1);
    let key_down = controller.handle(
        "owner",
        Operation::Batch {
            lease: token.clone(),
            events: vec![key(30, KeyState::Down)],
        },
        key_down_at,
        &mut || Ok(()),
    );
    assert_eq!(key_down["ok"], true);

    let first_due = key_down_at + Duration::from_millis(10);
    assert_eq!(
        controller.repeating_key.as_ref().unwrap().next_due,
        first_due
    );
    controller.set_key_repeat_policy(250, 20.0);
    assert_eq!(
        controller.repeating_key.as_ref().unwrap().next_due,
        first_due
    );

    controller.expire(first_due - Duration::from_millis(1));
    assert_eq!(trace.borrow().len(), 1);
    controller.expire(first_due);
    assert_eq!(
        trace.borrow().as_slice(),
        &[key(30, KeyState::Down), key(30, KeyState::Repeat)]
    );

    let second_due = first_due + Duration::from_millis(50);
    assert_eq!(
        controller.repeating_key.as_ref().unwrap().next_due,
        second_due
    );
    controller.set_key_repeat_policy(25, 5.0);
    assert_eq!(
        controller.repeating_key.as_ref().unwrap().next_due,
        second_due
    );

    controller.expire(second_due - Duration::from_millis(1));
    assert_eq!(trace.borrow().len(), 2);
    controller.expire(second_due);
    assert_eq!(trace.borrow().len(), 3);
    assert_eq!(trace.borrow()[2], key(30, KeyState::Repeat));

    let third_due = second_due + Duration::from_millis(200);
    assert_eq!(
        controller.repeating_key.as_ref().unwrap().next_due,
        third_due
    );
    assert_eq!(controller.key_repeat_policy(), (25, 5.0));
    let release = controller.handle(
        "owner",
        Operation::Release { lease: token },
        second_due + Duration::from_millis(1),
        &mut || Ok(()),
    );
    assert_eq!(release["released"], true);
    assert!(controller.repeating_key.is_none());

    let event_count = trace.borrow().len();
    controller.expire(third_due);
    assert_eq!(trace.borrow().len(), event_count);
}

#[test]
fn lease_repeat_overrides_survive_global_policy_updates() {
    let backend = FakeBackend::default();
    let trace = backend.trace.clone();
    let mut controller = Controller::new(backend, true);
    controller.set_key_repeat_policy(500, 100.0);
    let start = Instant::now();
    let acquired = controller.handle(
        "owner",
        Operation::Acquire {
            operation_id: None,
            ttl_ms: Some(1_000),
            back_button_timeout: None,
            controller: None,
            gamepad: None,
            ds4_back_as_touchpad_click: None,
            motion_as_ds4: None,
            touchpad_as_ds4: None,
            ds5_inputtino_randomize_mac: None,
            native_pen_touch: None,
            keyboard: None,
            mouse: None,
            high_resolution_scrolling: None,
            always_send_scancodes: None,
            key_rightalt_to_key_win: None,
            keybindings: None,
            key_repeat_delay: Some(20),
            key_repeat_frequency: Some(25.0),
        },
        start,
        &mut || Ok(()),
    );
    assert_eq!(acquired["ok"], true);
    let token = acquired["lease"].as_str().unwrap().to_owned();

    controller.set_key_repeat_policy(5, 1.0);
    assert_eq!(controller.effective_key_repeat_policy(), (20, 25.0));
    let key_down_at = start + Duration::from_millis(1);
    let key_down = controller.handle(
        "owner",
        Operation::Batch {
            lease: token.clone(),
            events: vec![key(30, KeyState::Down)],
        },
        key_down_at,
        &mut || Ok(()),
    );
    assert_eq!(key_down["ok"], true);

    let first_due = key_down_at + Duration::from_millis(20);
    controller.set_key_repeat_policy(250, 200.0);
    assert_eq!(controller.effective_key_repeat_policy(), (20, 25.0));
    assert_eq!(
        controller.repeating_key.as_ref().unwrap().next_due,
        first_due
    );
    controller.expire(first_due);

    assert_eq!(
        trace.borrow().as_slice(),
        &[key(30, KeyState::Down), key(30, KeyState::Repeat)]
    );
    assert_eq!(
        controller.repeating_key.as_ref().unwrap().next_due,
        first_due + Duration::from_millis(40)
    );
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

#[test]
fn pen_touch_tracks_identity_and_deduplicates_only_identical_moves() {
    let mut controller = Controller::new(
        FakeBackend {
            supports_pen_touch: true,
            ..Default::default()
        },
        true,
    );
    let token = acquire(&mut controller, Instant::now());
    let down_touch = pen_touch(5, PenTouchDevice::Touch, PenTouchPhase::Down, 100, 200);
    let down_pen = Event::PenTouch {
        pointer_id: PEN_POINTER_ID,
        device: PenTouchDevice::Pen,
        phase: PenTouchPhase::Down,
        x: 300,
        y: 400,
        pressure: Some(0.75),
        contact_area_major: Some(0.8),
        contact_area_minor: Some(0.3),
        rotation_degrees: Some(90),
        pen_tool: Some(PenTool::Eraser),
        pen_buttons: Some(PEN_BUTTON_MASK),
        tilt: Some(45),
    };
    let repeated_move = pen_touch(5, PenTouchDevice::Touch, PenTouchPhase::Move, 120, 220);
    let events = vec![
        down_touch,
        down_pen,
        repeated_move.clone(),
        repeated_move,
        pen_touch(
            PEN_POINTER_ID,
            PenTouchDevice::Pen,
            PenTouchPhase::Up,
            300,
            400,
        ),
        pen_touch(5, PenTouchDevice::Touch, PenTouchPhase::Cancel, 120, 220),
    ];

    assert_eq!(batch(&mut controller, &token, events.clone())["ok"], true);
    assert!(controller.pen_touch_contacts.is_empty());
    assert_eq!(
        *controller.backend.trace.borrow(),
        [
            events[0].clone(),
            events[1].clone(),
            events[2].clone(),
            events[4].clone(),
            events[5].clone()
        ]
    );
}

#[test]
fn pen_touch_rejects_duplicate_and_unowned_transitions_before_injection() {
    let mut controller = Controller::new(
        FakeBackend {
            supports_pen_touch: true,
            ..Default::default()
        },
        true,
    );
    let token = acquire(&mut controller, Instant::now());
    let down = pen_touch(8, PenTouchDevice::Touch, PenTouchPhase::Down, 0, 0);
    assert_eq!(
        batch(&mut controller, &token, vec![down.clone()])["ok"],
        true
    );
    assert_eq!(
        batch(&mut controller, &token, vec![down])["error"]["code"],
        "invalid_sequence"
    );
    for stray in [
        pen_touch(9, PenTouchDevice::Touch, PenTouchPhase::Move, 1, 1),
        pen_touch(9, PenTouchDevice::Touch, PenTouchPhase::Up, 1, 1),
        pen_touch(9, PenTouchDevice::Touch, PenTouchPhase::Cancel, 1, 1),
    ] {
        assert_eq!(
            batch(&mut controller, &token, vec![stray])["error"]["code"],
            "invalid_sequence"
        );
    }
    let up = pen_touch(8, PenTouchDevice::Touch, PenTouchPhase::Up, 2, 3);
    assert_eq!(batch(&mut controller, &token, vec![up.clone()])["ok"], true);
    assert_eq!(
        batch(&mut controller, &token, vec![up])["error"]["code"],
        "invalid_sequence"
    );
    assert_eq!(controller.backend.trace.borrow().len(), 2);
}

#[test]
fn pen_touch_limit_is_enforced_and_capability_is_reported_separately_from_policy() {
    let mut controller = Controller::new(
        FakeBackend {
            supports_pen_touch: true,
            ..Default::default()
        },
        true,
    );
    let token = acquire(&mut controller, Instant::now());
    let contacts = (0..MAX_PEN_TOUCH_CONTACTS)
        .map(|pointer_id| {
            pen_touch(
                pointer_id as u32,
                PenTouchDevice::Touch,
                PenTouchPhase::Down,
                0,
                0,
            )
        })
        .collect();
    assert_eq!(batch(&mut controller, &token, contacts)["ok"], true);
    assert_eq!(
        batch(
            &mut controller,
            &token,
            vec![pen_touch(
                MAX_PEN_TOUCH_CONTACTS as u32,
                PenTouchDevice::Touch,
                PenTouchPhase::Down,
                0,
                0,
            )]
        )["error"]["code"],
        "resource_exhausted"
    );
    assert_eq!(
        controller.status("owner", Instant::now())["native_pen_touch"],
        true
    );
    assert_eq!(
        controller.status("owner", Instant::now())["pen_touch_supported"],
        true
    );

    let mut unsupported = Controller::new(FakeBackend::default(), true);
    let unsupported_lease = acquire(&mut unsupported, Instant::now());
    assert_eq!(
        batch(
            &mut unsupported,
            &unsupported_lease,
            vec![pen_touch(
                PEN_POINTER_ID,
                PenTouchDevice::Pen,
                PenTouchPhase::Down,
                0,
                0
            )]
        )["error"]["code"],
        "unsupported_event"
    );
    assert_eq!(
        unsupported.status("owner", Instant::now())["native_pen_touch"],
        true
    );
    assert_eq!(
        unsupported.status("owner", Instant::now())["pen_touch_supported"],
        false
    );
}

#[test]
fn pen_touch_backend_capacity_rejects_an_overfull_batch_before_injection() {
    let mut controller = Controller::new(
        FakeBackend {
            supports_pen_touch: true,
            max_pen_contacts: Some(1),
            max_touch_contacts: Some(2),
            ..Default::default()
        },
        true,
    );
    let token = acquire(&mut controller, Instant::now());
    let overfull_batch = [
        pen_touch(1, PenTouchDevice::Touch, PenTouchPhase::Down, 10, 10),
        pen_touch(2, PenTouchDevice::Touch, PenTouchPhase::Down, 20, 20),
        pen_touch(3, PenTouchDevice::Touch, PenTouchPhase::Down, 30, 30),
    ];

    assert_eq!(
        batch(&mut controller, &token, overfull_batch.to_vec())["error"]["code"],
        "resource_exhausted"
    );
    assert!(controller.backend.trace.borrow().is_empty());
    assert!(controller.pen_touch_contacts.is_empty());

    assert_eq!(
        batch(
            &mut controller,
            &token,
            vec![
                pen_touch(1, PenTouchDevice::Touch, PenTouchPhase::Down, 10, 10),
                pen_touch(2, PenTouchDevice::Touch, PenTouchPhase::Down, 20, 20),
                pen_touch(1, PenTouchDevice::Pen, PenTouchPhase::Down, 30, 30),
            ]
        )["ok"],
        true
    );
    assert_eq!(controller.backend.trace.borrow().len(), 3);
    assert_eq!(controller.backend.trace.borrow().len(), 3);

    let mut unavailable_pen = Controller::new(
        FakeBackend {
            supports_pen_touch: true,
            max_pen_contacts: Some(0),
            ..Default::default()
        },
        true,
    );
    let unavailable_pen_token = acquire(&mut unavailable_pen, Instant::now());
    assert_eq!(
        batch(
            &mut unavailable_pen,
            &unavailable_pen_token,
            vec![pen_touch(
                PEN_POINTER_ID,
                PenTouchDevice::Pen,
                PenTouchPhase::Down,
                40,
                40,
            )]
        )["error"]["code"],
        "resource_exhausted"
    );
    assert!(unavailable_pen.backend.trace.borrow().is_empty());
}

#[test]
fn pen_touch_policy_suppresses_global_disable_but_honors_scoped_lease_override() {
    let mut disabled = Controller::new(
        FakeBackend {
            supports_pen_touch: true,
            ..Default::default()
        },
        true,
    );
    disabled.set_native_pen_touch(false);
    let disabled_token = acquire(&mut disabled, Instant::now());
    let suppressed = batch(
        &mut disabled,
        &disabled_token,
        vec![pen_touch(
            41,
            PenTouchDevice::Touch,
            PenTouchPhase::Down,
            1,
            2,
        )],
    );
    assert_eq!(suppressed["ok"], true);
    assert_eq!(suppressed["suppressed"], 1);
    assert!(disabled.backend.trace.borrow().is_empty());
    assert!(disabled.pen_touch_contacts.is_empty());
    assert_eq!(
        disabled.status("owner", Instant::now())["lease"]["native_pen_touch"],
        false
    );
    disabled.set_native_pen_touch(true);
    assert_eq!(
        batch(
            &mut disabled,
            &disabled_token,
            vec![pen_touch(
                41,
                PenTouchDevice::Touch,
                PenTouchPhase::Down,
                1,
                2,
            )]
        )["suppressed"],
        1
    );
    assert!(disabled.backend.trace.borrow().is_empty());

    let mut scoped = Controller::new(
        FakeBackend {
            supports_pen_touch: true,
            ..Default::default()
        },
        true,
    );
    let acquired = scoped.handle(
        "owner",
        Operation::Acquire {
            operation_id: None,
            ttl_ms: Some(1000),
            back_button_timeout: None,
            controller: None,
            gamepad: None,
            ds4_back_as_touchpad_click: None,
            motion_as_ds4: None,
            touchpad_as_ds4: None,
            ds5_inputtino_randomize_mac: None,
            native_pen_touch: Some(true),
            keyboard: None,
            mouse: None,
            high_resolution_scrolling: None,
            always_send_scancodes: None,
            key_rightalt_to_key_win: None,
            keybindings: None,
            key_repeat_delay: None,
            key_repeat_frequency: None,
        },
        Instant::now(),
        &mut || Ok(()),
    );
    let scoped_token = acquired["lease"].as_str().unwrap();
    assert_eq!(
        batch(
            &mut scoped,
            scoped_token,
            vec![pen_touch(
                PEN_POINTER_ID,
                PenTouchDevice::Pen,
                PenTouchPhase::Down,
                3,
                4,
            )]
        )["ok"],
        true
    );
    scoped.set_native_pen_touch(false);
    assert!(!scoped.native_pen_touch());
    assert_eq!(scoped.pen_touch_contacts.len(), 1);
    assert_eq!(
        batch(
            &mut scoped,
            scoped_token,
            vec![pen_touch(
                PEN_POINTER_ID,
                PenTouchDevice::Pen,
                PenTouchPhase::Move,
                5,
                6,
            )]
        )["ok"],
        true
    );
    assert_eq!(
        scoped.status("owner", Instant::now())["lease"]["native_pen_touch"],
        true
    );
}

#[test]
fn failed_pen_touch_down_is_conservatively_cancelled() {
    let mut controller = Controller::new(
        FakeBackend {
            supports_pen_touch: true,
            failed_pen_touch_events: vec![(PEN_POINTER_ID, PenTouchPhase::Down)],
            ..Default::default()
        },
        true,
    );
    let token = acquire(&mut controller, Instant::now());
    let down = Event::PenTouch {
        pointer_id: PEN_POINTER_ID,
        device: PenTouchDevice::Pen,
        phase: PenTouchPhase::Down,
        x: 50,
        y: 60,
        pressure: Some(0.6),
        contact_area_major: Some(0.7),
        contact_area_minor: Some(0.4),
        rotation_degrees: Some(270),
        pen_tool: Some(PenTool::Pen),
        pen_buttons: Some(0b101),
        tilt: Some(35),
    };

    let reply = batch(&mut controller, &token, vec![down.clone()]);
    assert_eq!(reply["ok"], false);
    assert_eq!(reply["accepted"], 0);
    assert!(controller.lease.is_none());
    assert!(controller.pen_touch_contacts.is_empty());
    let mut cancel = down.clone();
    let Event::PenTouch { phase, .. } = &mut cancel else {
        unreachable!();
    };
    *phase = PenTouchPhase::Cancel;
    assert_eq!(*controller.backend.trace.borrow(), [down, cancel]);
}

#[test]
fn failed_pen_touch_move_is_retried_before_deduplication_and_released() {
    for (pointer_id, device) in [
        (PEN_POINTER_ID, PenTouchDevice::Pen),
        (42, PenTouchDevice::Touch),
    ] {
        let mut controller = Controller::new(
            FakeBackend {
                supports_pen_touch: true,
                failed_pen_touch_events: vec![(pointer_id, PenTouchPhase::Move)],
                ..Default::default()
            },
            true,
        );
        let now = Instant::now();
        let token = acquire(&mut controller, now);
        let down = pen_touch(pointer_id, device, PenTouchPhase::Down, 10, 20);
        let move_event = pen_touch(pointer_id, device, PenTouchPhase::Move, 30, 40);
        controller.inject(&down).unwrap();

        assert_eq!(
            controller.inject(&move_event).unwrap_err().code,
            "injection_failed"
        );
        assert_eq!(controller.pen_touch_contacts[0].last_event, move_event);
        assert!(controller.pen_touch_contacts[0].last_move.is_none());

        controller.inject(&move_event).unwrap();
        controller.inject(&move_event).unwrap();
        assert_eq!(
            controller.pen_touch_contacts[0].last_move,
            Some(move_event.clone())
        );

        let released = controller.handle(
            "owner",
            Operation::Release { lease: token },
            now,
            &mut || Ok(()),
        );
        assert_eq!(released["released"], true);
        assert!(controller.pen_touch_contacts.is_empty());
        assert!(controller.lease.is_none());

        let mut cancel = move_event.clone();
        let Event::PenTouch { phase, .. } = &mut cancel else {
            unreachable!();
        };
        *phase = PenTouchPhase::Cancel;
        assert_eq!(
            *controller.backend.trace.borrow(),
            [down, move_event.clone(), move_event, cancel]
        );
    }
}

#[test]
fn pen_touch_capability_loss_allows_release_and_reacquire_after_recovery() {
    let mut controller = Controller::new(
        FakeBackend {
            supports_pen_touch: true,
            ..Default::default()
        },
        true,
    );
    let now = Instant::now();
    let token = acquire(&mut controller, now);
    let down = pen_touch(42, PenTouchDevice::Touch, PenTouchPhase::Down, 10, 20);
    assert_eq!(
        batch(&mut controller, &token, vec![down.clone()])["ok"],
        true
    );

    controller.backend.supports_pen_touch = false;
    let move_event = pen_touch(42, PenTouchDevice::Touch, PenTouchPhase::Move, 30, 40);
    let unavailable = batch(&mut controller, &token, vec![move_event]);
    assert_eq!(unavailable["error"]["code"], "unsupported_event");
    assert_eq!(controller.pen_touch_contacts.len(), 1);

    let release = controller.handle(
        "owner",
        Operation::Release { lease: token },
        now,
        &mut || Ok(()),
    );
    assert_eq!(release["released"], true);
    assert!(controller.pen_touch_contacts.is_empty());
    assert!(!controller.cleanup_required());

    controller.backend.supports_pen_touch = true;
    let next_token = acquire(&mut controller, now);
    assert_eq!(batch(&mut controller, &next_token, vec![down])["ok"], true);
    assert_eq!(
        controller.handle(
            "owner",
            Operation::Release { lease: next_token },
            now,
            &mut || Ok(()),
        )["released"],
        true
    );
    assert_eq!(controller.backend.starts, 2);
    assert_eq!(controller.backend.stops, 2);
}

#[test]
fn failed_pen_touch_release_retries_only_the_contact_whose_cancel_failed() {
    let mut controller = Controller::new(
        FakeBackend {
            supports_pen_touch: true,
            ..Default::default()
        },
        true,
    );
    let token = acquire(&mut controller, Instant::now());
    let down_one = pen_touch(
        PEN_POINTER_ID,
        PenTouchDevice::Touch,
        PenTouchPhase::Down,
        10,
        11,
    );
    let down_two = Event::PenTouch {
        pointer_id: PEN_POINTER_ID,
        device: PenTouchDevice::Pen,
        phase: PenTouchPhase::Down,
        x: 20,
        y: 21,
        pressure: Some(0.9),
        contact_area_major: Some(0.6),
        contact_area_minor: Some(0.2),
        rotation_degrees: Some(0),
        pen_tool: Some(PenTool::Unknown),
        pen_buttons: Some(0b010),
        tilt: Some(20),
    };
    assert_eq!(
        batch(&mut controller, &token, vec![down_one, down_two.clone()])["ok"],
        true
    );
    controller
        .backend
        .failed_pen_touch_events
        .push((PEN_POINTER_ID, PenTouchPhase::Cancel));

    let release = controller.handle(
        "owner",
        Operation::Release {
            lease: token.clone(),
        },
        Instant::now(),
        &mut || Ok(()),
    );
    assert_eq!(release["ok"], false);
    assert_eq!(release["error"]["code"], "release_failed");
    assert_eq!(controller.pen_touch_contacts.len(), 1);
    assert_eq!(controller.pen_touch_contacts[0].pointer_id, PEN_POINTER_ID);
    assert!(controller.cleanup_required());

    let retry_at = Instant::now() + Duration::from_millis(101);
    controller.expire(retry_at);
    assert!(controller.lease.is_none());
    assert!(controller.pen_touch_contacts.is_empty());
    let cancels = controller
        .backend
        .trace
        .borrow()
        .iter()
        .filter_map(|event| match event {
            Event::PenTouch {
                pointer_id,
                device,
                phase: PenTouchPhase::Cancel,
                ..
            } => Some((*device, *pointer_id)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        cancels,
        [
            (PenTouchDevice::Pen, PEN_POINTER_ID),
            (PenTouchDevice::Touch, PEN_POINTER_ID),
            (PenTouchDevice::Pen, PEN_POINTER_ID),
        ]
    );
    let mut expected_pen_cancel = down_two;
    let Event::PenTouch { phase, .. } = &mut expected_pen_cancel else {
        unreachable!();
    };
    *phase = PenTouchPhase::Cancel;
    let cancel_events = controller
        .backend
        .trace
        .borrow()
        .iter()
        .filter(|event| {
            matches!(
                event,
                Event::PenTouch {
                    phase: PenTouchPhase::Cancel,
                    ..
                }
            )
        })
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(
        cancel_events,
        [
            expected_pen_cancel.clone(),
            pen_touch(
                PEN_POINTER_ID,
                PenTouchDevice::Touch,
                PenTouchPhase::Cancel,
                10,
                11
            ),
            expected_pen_cancel,
        ]
    );
}

#[test]
fn pen_touch_shutdown_cancels_each_owned_pointer() {
    let trace = Rc::new(RefCell::new(Vec::new()));
    {
        let mut controller = Controller::new(
            FakeBackend {
                trace: trace.clone(),
                supports_pen_touch: true,
                ..Default::default()
            },
            true,
        );
        let token = acquire(&mut controller, Instant::now());
        assert_eq!(
            batch(
                &mut controller,
                &token,
                vec![
                    pen_touch(31, PenTouchDevice::Touch, PenTouchPhase::Down, 1, 2),
                    pen_touch(
                        PEN_POINTER_ID,
                        PenTouchDevice::Pen,
                        PenTouchPhase::Down,
                        3,
                        4
                    ),
                ]
            )["ok"],
            true
        );
    }

    let cancellations = trace
        .borrow()
        .iter()
        .filter_map(|event| match event {
            Event::PenTouch {
                pointer_id,
                phase: PenTouchPhase::Cancel,
                ..
            } => Some(*pointer_id),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(cancellations, [PEN_POINTER_ID, 31]);
}

fn acquire_with_keybindings(operation_id: &str, keybindings: Option<KeyBindings>) -> Operation {
    let mut operation = acquire_operation();
    if let Operation::Acquire {
        operation_id: requested_id,
        keybindings: requested_bindings,
        ..
    } = &mut operation
    {
        *requested_id = Some(operation_id.into());
        *requested_bindings = keybindings;
    }
    operation
}

#[test]
fn inherited_keybindings_follow_live_global_policy_and_status() {
    let mut controller = Controller::new(FakeBackend::default(), true);
    assert_eq!(controller.backend.keybindings, KeyBindings::default());
    let first = KeyBindings::from_flat(vec![65, 66]).unwrap();
    let second = KeyBindings::from_flat(vec![65, 67]).unwrap();
    controller.set_keybindings(&first);
    let now = Instant::now();
    let acquired = controller.handle(
        "owner",
        acquire_with_keybindings("inherited-bindings", None),
        now,
        &mut || Ok(()),
    );
    assert_eq!(acquired["ok"], true);
    assert!(acquired["keybindings"].is_null());
    assert_eq!(controller.backend.keybindings, first);
    controller.set_keybindings(&second);
    assert_eq!(controller.backend.keybindings, second);
    let status = controller.status("owner", now);
    assert_eq!(status["keybindings"], json!([65, 67]));
    assert_eq!(status["lease"]["keybindings"], json!([65, 67]));
    assert!(status["lease"]["requested_keybindings"].is_null());
    let retry = controller.handle(
        "owner",
        acquire_with_keybindings("inherited-bindings", None),
        now,
        &mut || Ok(()),
    );
    assert_eq!(retry["lease"], acquired["lease"]);
    assert_eq!(controller.backend.starts, 1);
}

#[test]
fn explicit_empty_keybindings_suppress_global_custom_values_until_cleanup_finishes() {
    let mut controller = Controller::new(FakeBackend::default(), true);
    let global = KeyBindings::from_flat(vec![65, 66]).unwrap();
    let newer_global = KeyBindings::from_flat(vec![65, 67]).unwrap();
    controller.set_keybindings(&global);
    let now = Instant::now();
    let acquired = controller.handle(
        "owner",
        acquire_with_keybindings("empty-bindings", Some(KeyBindings::empty())),
        now,
        &mut || Ok(()),
    );
    assert_eq!(acquired["keybindings"], json!([]));
    assert_eq!(controller.backend.keybindings.resolve(65, false), 65);
    assert_eq!(controller.backend.keybindings.resolve(16, false), 160);
    let update_count = controller.backend.keybinding_updates.len();
    controller.set_keybindings(&newer_global);
    assert_eq!(controller.backend.keybinding_updates.len(), update_count);
    assert_eq!(
        controller.status("owner", now)["lease"]["keybindings"],
        json!([])
    );
    assert_eq!(controller.keybindings(), &newer_global);

    controller.backend.fail_stop = true;
    let token = acquired["lease"].as_str().unwrap().to_owned();
    assert_eq!(
        controller.handle(
            "owner",
            Operation::Release {
                lease: token.clone()
            },
            now,
            &mut || Ok(())
        )["error"]["code"],
        "release_failed"
    );
    assert_eq!(controller.backend.keybindings, KeyBindings::empty());
    controller.backend.fail_stop = false;
    assert_eq!(
        controller.handle(
            "owner",
            Operation::Release { lease: token },
            now,
            &mut || Ok(())
        )["ok"],
        true
    );
    assert_eq!(controller.backend.keybindings, newer_global);
    let terminal = controller.lookup("owner", "empty-bindings", now).unwrap();
    assert_eq!(terminal["keybindings"], json!([]));
    assert!(terminal["lease"].is_null());
}

#[test]
fn exact_ordered_keybinding_request_is_required_for_active_and_terminal_replay() {
    let mut controller = Controller::new(FakeBackend::default(), true);
    let now = Instant::now();
    let original = KeyBindings::from_flat(vec![65, 66, 65, 67]).unwrap();
    let reordered = KeyBindings::from_flat(vec![65, 67, 65, 66]).unwrap();
    let acquired = controller.handle(
        "owner",
        acquire_with_keybindings("ordered-bindings", Some(original.clone())),
        now,
        &mut || Ok(()),
    );
    assert_eq!(acquired["keybindings"], json!([65, 66, 65, 67]));
    let different_requests = [None, Some(KeyBindings::empty()), Some(reordered)];
    for request in &different_requests {
        assert_eq!(
            controller.handle(
                "owner",
                acquire_with_keybindings("ordered-bindings", request.clone()),
                now,
                &mut || Ok(())
            )["error"]["code"],
            "operation_conflict"
        );
    }
    assert_eq!(
        controller.handle(
            "owner",
            Operation::Release {
                lease: acquired["lease"].as_str().unwrap().into()
            },
            now,
            &mut || Ok(())
        )["ok"],
        true
    );
    for request in different_requests {
        assert_eq!(
            controller.handle(
                "owner",
                acquire_with_keybindings("ordered-bindings", request),
                now,
                &mut || Ok(())
            )["error"]["code"],
            "operation_conflict"
        );
    }
    let retry = controller.handle(
        "owner",
        acquire_with_keybindings("ordered-bindings", Some(original)),
        now,
        &mut || Ok(()),
    );
    assert_eq!(retry["state"], "released");
    assert_eq!(retry["keybindings"], json!([65, 66, 65, 67]));
    assert_eq!(
        controller
            .lookup("foreign", "ordered-bindings", now)
            .unwrap_err()
            .code,
        "not_found"
    );
}

#[test]
fn failed_acquisition_restores_global_keybindings_before_the_next_attempt() {
    let mut controller = Controller::new(
        FakeBackend {
            fail_start_once: true,
            ..FakeBackend::default()
        },
        true,
    );
    let global = KeyBindings::from_flat(vec![65, 66]).unwrap();
    controller.set_keybindings(&global);
    let now = Instant::now();
    assert_eq!(
        controller.handle(
            "owner",
            acquire_with_keybindings("failed-bindings", Some(KeyBindings::empty())),
            now,
            &mut || Ok(())
        )["ok"],
        false
    );
    assert_eq!(controller.backend.keybindings, global);
    assert!(controller.lease.is_none());
    assert_eq!(
        controller.handle(
            "owner",
            acquire_with_keybindings("next-bindings", None),
            now,
            &mut || Ok(())
        )["ok"],
        true
    );
    assert_eq!(controller.backend.keybindings, global);
}

#[test]
fn gamepad_feedback_poll_requires_the_lease_owner_and_enabled_backend() {
    let feedback = amaterasu_input_companion::GamepadFeedback {
        slot: 1,
        strong_motor: 32000,
        weak_motor: 16000,
        duration_ms: 250,
    };
    let mut controller = Controller::new(
        FakeBackend {
            controller_input_supported: true,
            gamepad_feedback: Some(feedback),
            ..Default::default()
        },
        true,
    );
    let now = Instant::now();
    let lease = acquire(&mut controller, now);

    let denied = controller.handle(
        "other",
        Operation::PollGamepadFeedback {
            lease: lease.clone(),
        },
        now,
        &mut || Ok(()),
    );
    assert_eq!(denied["error"]["code"], "permission_denied");

    let failed_delivery = controller.handle(
        "owner",
        Operation::PollGamepadFeedback {
            lease: lease.clone(),
        },
        now,
        &mut || Err(InputError::new("cancelled", "request ended before poll")),
    );
    assert_eq!(failed_delivery["error"]["code"], "cancelled");
    assert_eq!(controller.backend.gamepad_feedback, Some(feedback));

    let received = controller.handle(
        "owner",
        Operation::PollGamepadFeedback {
            lease: lease.clone(),
        },
        now,
        &mut || Ok(()),
    );
    assert_eq!(received["ok"], true);
    assert_eq!(received["lease"], lease);
    assert_eq!(received["feedback"]["slot"], 1);
    assert_eq!(received["feedback"]["strong_motor"], 32000);

    controller.backend.controller_input_supported = false;
    let unavailable = controller.handle(
        "owner",
        Operation::PollGamepadFeedback { lease },
        now,
        &mut || Ok(()),
    );
    assert_eq!(unavailable["feedback"], serde_json::Value::Null);
}

#[test]
fn controller_disable_still_allows_terminal_rumble_zero_to_flush() {
    let terminal_zero = amaterasu_input_companion::GamepadFeedback {
        slot: 1,
        strong_motor: 0,
        weak_motor: 0,
        duration_ms: 0,
    };
    let mut controller = Controller::new(
        FakeBackend {
            controller_input_supported: true,
            gamepad_feedback: Some(terminal_zero),
            ..Default::default()
        },
        true,
    );
    let now = Instant::now();
    let lease = acquire(&mut controller, now);
    controller.set_controller_enabled(false, now).unwrap();

    let response = controller.handle(
        "owner",
        Operation::PollGamepadFeedback { lease },
        now,
        &mut || Ok(()),
    );
    assert_eq!(response["feedback"]["duration_ms"], 0);
}

#[test]
fn scoped_ds4_back_mapping_survives_global_updates_and_restores_after_release() {
    let mut controller = Controller::new(FakeBackend::default(), true);
    let now = Instant::now();
    let acquired = controller.handle(
        "owner",
        Operation::Acquire {
            operation_id: Some("ds4-back-mapping".into()),
            ttl_ms: None,
            back_button_timeout: None,
            controller: None,
            gamepad: Some(GamepadProfile::Auto),
            ds4_back_as_touchpad_click: Some(false),
            motion_as_ds4: None,
            touchpad_as_ds4: None,
            ds5_inputtino_randomize_mac: None,
            keybindings: None,
            native_pen_touch: None,
            keyboard: None,
            mouse: None,
            high_resolution_scrolling: None,
            always_send_scancodes: None,
            key_rightalt_to_key_win: None,
            key_repeat_delay: None,
            key_repeat_frequency: None,
        },
        now,
        &mut || Ok(()),
    );
    assert_eq!(acquired["ok"], true);
    assert_eq!(acquired["ds4_back_as_touchpad_click"], false);
    let lease = controller.lease.as_ref().unwrap();
    assert_eq!(lease.requested_ds4_back_as_touchpad_click, Some(false));
    assert!(!lease.ds4_back_as_touchpad_click);
    assert!(!controller.backend.ds4_back_as_touchpad_click);

    controller.set_ds4_back_as_touchpad_click(false);
    controller.set_ds4_back_as_touchpad_click(true);
    assert!(!controller.backend.ds4_back_as_touchpad_click);

    controller.handle(
        "owner",
        Operation::Release {
            lease: acquired["lease"].as_str().unwrap().into(),
        },
        now,
        &mut || Ok(()),
    );
    assert!(controller.backend.ds4_back_as_touchpad_click);
}

#[test]
fn modifier_alias_release_failure_preserves_owner_until_recovery() {
    let mut controller = Controller::new(FakeBackend::default(), true);
    let now = Instant::now();
    let token = acquire(&mut controller, now);
    let down = moonlight_key(0x10, KeyState::Down, 0, false);
    let up = moonlight_key(0xa0, KeyState::Up, 0, false);
    assert_eq!(
        batch(&mut controller, &token, vec![down.clone()])["ok"],
        true
    );
    assert_eq!(controller.pressed.len(), 1);

    let unrelated = controller.handle(
        "other",
        Operation::Release {
            lease: token.clone(),
        },
        now,
        &mut || Ok(()),
    );
    assert_eq!(unrelated["error"]["code"], "permission_denied");
    assert_eq!(controller.pressed.len(), 1);

    controller.backend.fail_release = true;
    let failed = batch(&mut controller, &token, vec![up.clone()]);
    assert_eq!(failed["error"]["code"], "injection_failed");
    assert_eq!(failed["cleanup_required"], true);
    assert_eq!(controller.pressed.len(), 1);
    assert_eq!(
        controller.handle("other", acquire_operation(), now, &mut || Ok(()))["error"]["code"],
        "input_busy"
    );

    controller.backend.fail_release = false;
    controller.expire(now + Duration::from_millis(101));
    assert!(controller.lease.is_none());
    assert!(controller.pressed.is_empty());
    let trace = controller.backend.trace.borrow();
    let cleanup_up = moonlight_key(0x10, KeyState::Up, 0, false);
    assert_eq!(
        trace.as_slice(),
        &[down, cleanup_up.clone(), cleanup_up.clone(), cleanup_up]
    );
    drop(trace);

    let replacement = controller.handle("other", acquire_operation(), now, &mut || Ok(()));
    assert_eq!(replacement["ok"], true);
    assert_ne!(replacement["lease"], token);
    assert_eq!(
        controller.handle(
            "owner",
            Operation::Release { lease: token },
            now,
            &mut || Ok(()),
        )["error"]["code"],
        "permission_denied"
    );
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
fn touch_as_mouse_status_does_not_claim_native_pen_touch_support() {
    let mut controller = Controller::new(
        FakeBackend {
            touch_as_mouse_max_contacts: Some(1),
            ..Default::default()
        },
        true,
    );
    let now = Instant::now();
    let response = controller.handle("owner", acquire_operation(), now, &mut || Ok(()));
    assert_eq!(response["ok"], true);
    assert_eq!(response["native_pen_touch"], true); // Event-delivery gate.
    assert_eq!(response["pen_touch_supported"], false); // No native device.
    assert_eq!(response["touch_as_mouse_max_contacts"], 1);
    let status = controller.status("owner", now);
    assert_eq!(status["native_pen_touch"], true); // Gate, not capability.
    assert_eq!(status["pen_touch_supported"], false);
    assert_eq!(status["touch_as_mouse_max_contacts"], 1);
    assert_eq!(status["lease"]["pen_touch_supported"], false);
    assert_eq!(status["lease"]["touch_as_mouse_max_contacts"], 1);

    let lease = response["lease"].as_str().unwrap();
    assert_eq!(
        batch(
            &mut controller,
            lease,
            vec![pen_touch(
                7,
                PenTouchDevice::Touch,
                PenTouchPhase::Down,
                1,
                2
            )]
        )["ok"],
        true
    );
    assert_eq!(
        batch(
            &mut controller,
            lease,
            vec![pen_touch(
                8,
                PenTouchDevice::Touch,
                PenTouchPhase::Down,
                3,
                4
            )]
        )["error"]["code"],
        "resource_exhausted"
    );
    assert_eq!(
        batch(
            &mut controller,
            lease,
            vec![pen_touch(
                PEN_POINTER_ID,
                PenTouchDevice::Pen,
                PenTouchPhase::Down,
                1,
                2,
            )]
        )["error"]["code"],
        "unsupported_event"
    );
    assert_eq!(controller.backend.trace.borrow().len(), 1);
}

#[test]
fn native_pen_touch_setting_is_only_the_event_gate_for_mouse_emulation() {
    let mut controller = Controller::new(
        FakeBackend {
            touch_as_mouse_max_contacts: Some(1),
            ..Default::default()
        },
        true,
    );
    controller.set_native_pen_touch(false);
    let token = acquire(&mut controller, Instant::now());
    assert_eq!(
        batch(
            &mut controller,
            &token,
            vec![pen_touch(
                7,
                PenTouchDevice::Touch,
                PenTouchPhase::Down,
                1,
                2
            )]
        )["ok"],
        true
    );
    assert!(controller.backend.trace.borrow().is_empty());
    assert_eq!(
        controller.status("owner", Instant::now())["pen_touch_supported"],
        false
    );
    assert_eq!(
        controller.status("owner", Instant::now())["native_pen_touch"],
        false
    );
    assert_eq!(
        controller.status("owner", Instant::now())["touch_as_mouse_max_contacts"],
        1
    );
}

#[test]
fn touch_as_mouse_still_requires_the_existing_mouse_device_policy() {
    let mut controller = Controller::new(
        FakeBackend {
            touch_as_mouse_max_contacts: Some(1),
            ..Default::default()
        },
        true,
    );
    controller
        .set_device_policy(
            DevicePolicy {
                keyboard: true,
                mouse: false,
            },
            Instant::now(),
        )
        .unwrap();
    let token = acquire(&mut controller, Instant::now());

    assert_eq!(
        batch(
            &mut controller,
            &token,
            vec![pen_touch(
                7,
                PenTouchDevice::Touch,
                PenTouchPhase::Down,
                1,
                2
            )]
        )["ok"],
        true
    );
    assert!(controller.backend.trace.borrow().is_empty());
}
