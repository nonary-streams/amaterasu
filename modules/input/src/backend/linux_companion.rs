//! Linux desktop input through the host-owned Unix companion transport.
use super::Backend;
use crate::gamepad_profile::GamepadProfile;
use crate::model::{
    Axis, Button, Event, InputError, InputResult, Key, KeyState, PenTouchDevice,
    effective_virtual_key,
};
use amaterasu_input_companion::{InputEvent, LinuxInputClient};
use std::collections::HashMap;
use std::path::Path;

const MAX_COMPANION_SCROLL_DELTA: i32 = 12_000;

fn evdev_key_event(code: u16, state: KeyState) -> InputEvent {
    match state {
        KeyState::Repeat => InputEvent::EvdevKeyRepeat { code },
        KeyState::Down | KeyState::Up => InputEvent::EvdevKey {
            code,
            pressed: state == KeyState::Down,
        },
    }
}

fn absolute_pointer_event(x: u16, y: u16) -> InputEvent {
    InputEvent::Absolute { x, y }
}

fn feedback_belongs_to_active_slot(
    feedback: amaterasu_input_companion::GamepadFeedback,
    active_slots: u16,
) -> InputResult<bool> {
    let terminal_zero = feedback.strong_motor == 0 && feedback.weak_motor == 0;
    if feedback.slot >= 16
        || feedback.duration_ms > 10_000
        || terminal_zero && feedback.duration_ms != 0
        || !terminal_zero && feedback.duration_ms == 0
    {
        return Err(invalid_event());
    }
    Ok(terminal_zero || active_slots & (1u16 << feedback.slot) != 0)
}

fn companion_gamepad_event(event: &Event) -> InputResult<InputEvent> {
    let Event::Controller {
        number,
        active_mask,
        buttons,
        left_trigger,
        right_trigger,
        left_x,
        left_y,
        right_x,
        right_y,
    } = event
    else {
        return Err(invalid_event());
    };
    if *number >= 16 || *buttons & !SUPPORTED_GAMEPAD_BUTTONS != 0 {
        return Err(invalid_event());
    }
    Ok(InputEvent::Gamepad {
        number: *number,
        active_mask: *active_mask,
        buttons: *buttons,
        left_trigger: *left_trigger,
        right_trigger: *right_trigger,
        left_x: *left_x,
        left_y: *left_y,
        right_x: *right_x,
        right_y: *right_y,
    })
}

#[derive(Default)]
struct SingleTouchAsMouse {
    active_pointer: Option<u32>,
}

impl SingleTouchAsMouse {
    #[cfg(test)]
    fn translate(&mut self, event: &Event) -> InputResult<Vec<InputEvent>> {
        let (intents, next_pointer) = self.plan(event)?;
        self.active_pointer = next_pointer;
        Ok(intents)
    }

    fn plan(&self, event: &Event) -> InputResult<(Vec<InputEvent>, Option<u32>)> {
        let (pointer_id, phase, x, y) = touch_mouse_fields(event)?;
        let absolute = absolute_pointer_event(x, y);
        match phase {
            crate::model::PenTouchPhase::Down => {
                if self.active_pointer.is_some() {
                    return Err(invalid_touch_sequence(
                        "only one touch contact may be active",
                    ));
                }
                Ok((vec![absolute, left_button_event(true)], Some(pointer_id)))
            }
            crate::model::PenTouchPhase::Move => {
                self.require_active_pointer(pointer_id)?;
                Ok((vec![absolute], self.active_pointer))
            }
            crate::model::PenTouchPhase::Up | crate::model::PenTouchPhase::Cancel => {
                self.require_active_pointer(pointer_id)?;
                Ok((vec![absolute, left_button_event(false)], None))
            }
        }
    }

    fn require_active_pointer(&self, pointer_id: u32) -> InputResult<()> {
        if self.active_pointer == Some(pointer_id) {
            Ok(())
        } else {
            Err(invalid_touch_sequence(
                "touch move or release requires the active pointer",
            ))
        }
    }

    fn reset(&mut self) {
        self.active_pointer = None;
    }
}

fn touch_mouse_fields(event: &Event) -> InputResult<(u32, crate::model::PenTouchPhase, u16, u16)> {
    event.validate()?;
    let Event::PenTouch {
        pointer_id,
        device,
        phase,
        x,
        y,
        pressure,
        contact_area_major,
        contact_area_minor,
        rotation_degrees,
        pen_tool,
        pen_buttons,
        tilt,
    } = event
    else {
        return Err(unsupported_touch_event());
    };
    if *device != PenTouchDevice::Touch {
        return Err(unsupported_touch_event());
    }
    if pressure.is_some()
        || contact_area_major.is_some()
        || contact_area_minor.is_some()
        || rotation_degrees.is_some()
        || pen_tool.is_some()
        || pen_buttons.is_some()
        || tilt.is_some()
    {
        return Err(unsupported_touch_event());
    }
    Ok((*pointer_id, *phase, *x, *y))
}

fn left_button_event(pressed: bool) -> InputEvent {
    InputEvent::MouseButton {
        button: amaterasu_input_companion::MouseButton::Left,
        pressed,
    }
}

fn invalid_touch_sequence(message: &'static str) -> InputError {
    InputError::new("invalid_sequence", message)
}

fn unsupported_touch_event() -> InputError {
    InputError::new(
        "unsupported_event",
        "Linux companion maps one coordinate-only touch contact to a mouse button; native pen, multitouch, and touch metadata are unsupported",
    )
}

pub struct LinuxCompanionBackend {
    socket_path: String,
    client: Option<LinuxInputClient>,
    held_virtual_keycodes: HashMap<u8, u16>,
    key_rightalt_to_key_win: bool,
    profile: GamepadProfile,
    touch_as_mouse: SingleTouchAsMouse,
    active_gamepad_slots: u16,
}

impl LinuxCompanionBackend {
    pub fn new(socket_path: String) -> Self {
        Self {
            socket_path,
            client: None,
            held_virtual_keycodes: HashMap::new(),
            key_rightalt_to_key_win: false,
            profile: GamepadProfile::Auto,
            touch_as_mouse: SingleTouchAsMouse::default(),
            active_gamepad_slots: 0,
        }
    }

    fn event_keycode(&self, key: &Key, state: KeyState) -> InputResult<u16> {
        if let Key::MoonlightVk { code, .. } = key
            && matches!(state, KeyState::Up | KeyState::Repeat)
            && let Some(keycode) = self.held_virtual_keycodes.get(code)
        {
            return Ok(*keycode);
        }
        match key {
            Key::LinuxEvdev { code } => Ok(*code),
            Key::MoonlightVk { code, .. } => {
                let virtual_key = effective_virtual_key(*code, self.key_rightalt_to_key_win);
                amaterasu_gamestream::control::virtual_key_mapping(virtual_key)
                    .map(|mapping| mapping.linux_evdev_code)
                    .ok_or_else(unsupported_key)
            }
            Key::WindowsScan { .. } => Err(unsupported_key()),
        }
    }

    fn send(&mut self, event: InputEvent) -> InputResult<()> {
        let client = self.client.as_mut().ok_or_else(not_started)?;
        client.send(event).map_err(companion_error)
    }

    fn poll_feedback(&mut self) -> InputResult<Option<amaterasu_input_companion::GamepadFeedback>> {
        let client = self.client.as_mut().ok_or_else(not_started)?;
        if !client.gamepad_enabled() {
            return Ok(None);
        }
        let Some(feedback) = client.poll_gamepad_feedback().map_err(companion_error)? else {
            return Ok(None);
        };
        if !feedback_belongs_to_active_slot(feedback, self.active_gamepad_slots)? {
            return Ok(None);
        }
        Ok(Some(feedback))
    }
}

impl Backend for LinuxCompanionBackend {
    fn name(&self) -> &'static str {
        "linux_uinput_companion_touch_as_mouse"
    }

    fn supported(&self) -> bool {
        true
    }

    fn absolute_pointer(&self) -> bool {
        true
    }

    fn pen_touch_supported(&self) -> bool {
        false
    }

    fn supports_pen_touch_event(&self, device: PenTouchDevice) -> bool {
        device == PenTouchDevice::Touch
    }

    fn touch_as_mouse_max_contacts(&self) -> Option<usize> {
        Some(1)
    }

    fn controller_input_supported(&self) -> bool {
        self.client
            .as_ref()
            .is_some_and(LinuxInputClient::gamepad_enabled)
            && matches!(self.profile, GamepadProfile::Auto | GamepadProfile::Xone)
    }

    fn gamepad_unavailable_reason(&self) -> Option<&str> {
        match self.profile {
            GamepadProfile::Auto | GamepadProfile::Xone if self.controller_input_supported() => {
                None
            }
            GamepadProfile::Auto | GamepadProfile::Xone => {
                Some("Linux gamepad input requires the host-owned digest-bound gamepad grant")
            }
            _ => Some("selected Linux gamepad profile is unavailable through the input companion"),
        }
    }

    fn supported_gamepad_profiles(&self) -> &'static [&'static str] {
        &["auto", "xone"]
    }

    fn key_encoding(&self) -> &'static str {
        "linux_evdev"
    }

    fn validate(&self, event: &Event) -> InputResult<()> {
        event.validate()?;
        match event {
            Event::Key { key, state } => {
                let code = self.event_keycode(key, *state)?;
                if !(1..=248).contains(&code) || code == 240 {
                    return Err(unsupported_key());
                }
                Ok(())
            }
            Event::Button { .. }
            | Event::Relative { .. }
            | Event::Absolute { .. }
            | Event::Scroll { .. }
            | Event::ScrollUnits { .. } => Ok(()),
            Event::ControllerArrival { .. }
            | Event::ControllerTouch { .. }
            | Event::ControllerMotion { .. }
            | Event::ControllerBattery { .. } => Err(InputError::new(
                "unsupported_event",
                "companion controller metadata requires a native controller profile",
            )),
            Event::PenTouch { .. } => touch_mouse_fields(event).map(|_| ()),
            Event::Controller {
                number, buttons, ..
            } => {
                if *number >= 16 || *buttons & !SUPPORTED_GAMEPAD_BUTTONS != 0 {
                    return Err(invalid_event());
                }
                if !matches!(self.profile, GamepadProfile::Auto | GamepadProfile::Xone)
                    || !self.controller_input_supported()
                {
                    return Err(InputError::new(
                        "controller_unavailable",
                        self.gamepad_unavailable_reason()
                            .unwrap_or("Linux gamepad input is unavailable")
                            .to_owned(),
                    ));
                }
                Ok(())
            }
        }
    }

    fn start(&mut self) -> InputResult<()> {
        if self.client.is_some() {
            return Err(InputError::new(
                "backend_busy",
                "input companion lease is active",
            ));
        }
        self.client =
            Some(LinuxInputClient::connect(Path::new(&self.socket_path)).map_err(companion_error)?);
        self.held_virtual_keycodes.clear();
        self.touch_as_mouse.reset();
        self.active_gamepad_slots = 0;
        Ok(())
    }

    fn inject(&mut self, event: &Event) -> InputResult<()> {
        self.validate(event)?;
        match event {
            Event::Key { key, state } => {
                let code = self.event_keycode(key, *state)?;
                if let Key::MoonlightVk { code: identity, .. } = key
                    && *state == KeyState::Down
                {
                    self.held_virtual_keycodes.insert(*identity, code);
                }
                self.send(evdev_key_event(code, *state))?;
                if let (Key::MoonlightVk { code: identity, .. }, KeyState::Up) = (key, state) {
                    self.held_virtual_keycodes.remove(identity);
                }
                Ok(())
            }
            Event::Button { button, pressed } => self.send(InputEvent::MouseButton {
                button: match button {
                    Button::Left => amaterasu_input_companion::MouseButton::Left,
                    Button::Right => amaterasu_input_companion::MouseButton::Right,
                    Button::Middle => amaterasu_input_companion::MouseButton::Middle,
                    Button::Back => amaterasu_input_companion::MouseButton::Back,
                    Button::Forward => amaterasu_input_companion::MouseButton::Forward,
                },
                pressed: *pressed,
            }),
            Event::Relative { dx, dy } => self.send(InputEvent::Relative {
                dx: i16::try_from(*dx).map_err(|_| invalid_event())?,
                dy: i16::try_from(*dy).map_err(|_| invalid_event())?,
            }),
            Event::Absolute { x, y } => self.send(absolute_pointer_event(*x, *y)),
            Event::Controller { active_mask, .. } => {
                self.send(companion_gamepad_event(event)?)?;
                self.active_gamepad_slots = *active_mask;
                Ok(())
            }
            Event::PenTouch { .. } => {
                let (intents, next_pointer) = self.touch_as_mouse.plan(event)?;
                for intent in intents {
                    self.send(intent)?;
                }
                self.touch_as_mouse.active_pointer = next_pointer;
                Ok(())
            }
            Event::Scroll { axis, steps } => {
                let units = i16::try_from(steps * 120).map_err(|_| invalid_event())?;
                self.scroll(*axis, units)
            }
            Event::ScrollUnits { axis, amount } => self.scroll(*axis, *amount),
            Event::ControllerArrival { .. }
            | Event::ControllerTouch { .. }
            | Event::ControllerMotion { .. }
            | Event::ControllerBattery { .. } => Err(InputError::new(
                "unsupported_event",
                "companion controller metadata requires a native controller profile",
            )),
        }
    }

    fn set_key_rightalt_to_key_win(&mut self, enabled: bool) {
        self.key_rightalt_to_key_win = enabled;
    }

    fn set_gamepad_profile(&mut self, profile: GamepadProfile) {
        self.profile = profile;
    }

    fn disable_controller_input(&mut self) -> InputResult<()> {
        if !self
            .client
            .as_ref()
            .is_some_and(LinuxInputClient::gamepad_enabled)
        {
            self.active_gamepad_slots = 0;
            return Ok(());
        }
        self.send(InputEvent::Gamepad {
            number: 0,
            active_mask: 0,
            buttons: 0,
            left_trigger: 0,
            right_trigger: 0,
            left_x: 0,
            left_y: 0,
            right_x: 0,
            right_y: 0,
        })?;
        self.active_gamepad_slots = 0;
        Ok(())
    }

    fn poll_gamepad_feedback(
        &mut self,
    ) -> InputResult<Option<amaterasu_input_companion::GamepadFeedback>> {
        self.poll_feedback()
    }

    fn stop(&mut self) -> InputResult<()> {
        let Some(mut client) = self.client.take() else {
            return Ok(());
        };
        self.held_virtual_keycodes.clear();
        self.touch_as_mouse.reset();
        self.active_gamepad_slots = 0;
        client.close().map_err(companion_error)
    }
}

const SUPPORTED_GAMEPAD_BUTTONS: u32 = ((1 << 11) - 1) | ((1 << 22) - (1 << 12));

impl LinuxCompanionBackend {
    fn scroll(&mut self, axis: Axis, amount: i16) -> InputResult<()> {
        for event in scroll_events(axis, amount) {
            self.send(event)?;
        }
        Ok(())
    }
}

fn scroll_events(axis: Axis, amount: i16) -> Vec<InputEvent> {
    let mut remaining = i32::from(amount);
    let mut events = Vec::with_capacity(3);
    while remaining != 0 {
        let chunk = remaining.clamp(-MAX_COMPANION_SCROLL_DELTA, MAX_COMPANION_SCROLL_DELTA);
        let chunk = i16::try_from(chunk).expect("bounded scroll chunk fits i16");
        events.push(match axis {
            Axis::Vertical => InputEvent::Scroll {
                vertical: chunk,
                horizontal: 0,
            },
            Axis::Horizontal => InputEvent::Scroll {
                vertical: 0,
                horizontal: chunk,
            },
        });
        remaining -= i32::from(chunk);
    }
    events
}

fn not_started() -> InputError {
    InputError::new("backend_inactive", "Linux companion lease is not active")
}

fn unsupported_key() -> InputError {
    InputError::new("unsupported_event", "Linux cannot map this key to evdev")
}

fn invalid_event() -> InputError {
    InputError::new(
        "invalid_event",
        "Linux input event is outside its supported range",
    )
}

fn companion_error(error: amaterasu_input_companion::InputError) -> InputError {
    InputError::new(
        "companion_unavailable",
        format!("Linux input companion rejected the request: {error:?}"),
    )
}

#[cfg(test)]
mod tests {
    use super::{
        Axis, Backend, InputEvent, KeyState, LinuxCompanionBackend, SingleTouchAsMouse,
        absolute_pointer_event, companion_gamepad_event, evdev_key_event,
        feedback_belongs_to_active_slot, left_button_event, scroll_events,
    };
    use crate::gamepad_profile::GamepadProfile;
    use crate::model::{Event, PenTouchDevice, PenTouchPhase};
    use amaterasu_input_companion::{
        DeviceSink, GamepadFeedback, GrantRegistry, InputError as CompanionError, InputPacket,
        InputResult, InputSession, MouseButton as CompanionMouseButton, PROTOCOL_VERSION,
        PeerIdentity,
    };
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    fn touch(pointer_id: u32, phase: PenTouchPhase, x: u16, y: u16) -> Event {
        Event::PenTouch {
            pointer_id,
            device: PenTouchDevice::Touch,
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

    #[test]
    fn one_touch_contact_maps_down_move_up_and_cancel_in_order() {
        let mut translator = SingleTouchAsMouse::default();
        assert_eq!(
            translator
                .translate(&touch(4, PenTouchPhase::Down, 100, 200))
                .unwrap(),
            [
                InputEvent::Absolute { x: 100, y: 200 },
                left_button_event(true),
            ]
        );
        assert_eq!(
            translator
                .translate(&touch(4, PenTouchPhase::Move, 300, 400))
                .unwrap(),
            [InputEvent::Absolute { x: 300, y: 400 }]
        );
        assert_eq!(
            translator
                .translate(&touch(4, PenTouchPhase::Up, 500, 600))
                .unwrap(),
            [
                InputEvent::Absolute { x: 500, y: 600 },
                left_button_event(false),
            ]
        );
        assert_eq!(
            translator
                .translate(&touch(5, PenTouchPhase::Down, 0, u16::MAX))
                .unwrap(),
            [
                InputEvent::Absolute { x: 0, y: u16::MAX },
                left_button_event(true),
            ]
        );
        assert_eq!(
            translator
                .translate(&touch(5, PenTouchPhase::Cancel, 0, u16::MAX))
                .unwrap(),
            [
                InputEvent::Absolute { x: 0, y: u16::MAX },
                left_button_event(false),
            ]
        );
    }

    #[test]
    fn rumble_feedback_is_returned_only_for_a_current_bounded_slot() {
        let feedback = GamepadFeedback {
            slot: 3,
            strong_motor: 40000,
            weak_motor: 12000,
            duration_ms: 500,
        };
        assert!(feedback_belongs_to_active_slot(feedback, 1 << 3).unwrap());
        assert!(!feedback_belongs_to_active_slot(feedback, 1 << 2).unwrap());
        assert!(
            feedback_belongs_to_active_slot(
                GamepadFeedback {
                    strong_motor: 0,
                    weak_motor: 0,
                    duration_ms: 0,
                    ..feedback
                },
                0,
            )
            .unwrap()
        );
        assert!(
            feedback_belongs_to_active_slot(
                GamepadFeedback {
                    slot: 16,
                    ..feedback
                },
                u16::MAX,
            )
            .is_err()
        );
        assert!(
            feedback_belongs_to_active_slot(
                GamepadFeedback {
                    duration_ms: 10_001,
                    ..feedback
                },
                1 << 3,
            )
            .is_err()
        );
    }

    #[test]
    fn second_or_duplicate_touch_pointer_is_rejected_without_losing_owner() {
        let mut translator = SingleTouchAsMouse::default();
        translator
            .translate(&touch(4, PenTouchPhase::Down, 100, 200))
            .unwrap();

        assert!(
            translator
                .translate(&touch(4, PenTouchPhase::Down, 100, 200))
                .is_err()
        );
        assert!(
            translator
                .translate(&touch(5, PenTouchPhase::Down, 100, 200))
                .is_err()
        );
        assert!(
            translator
                .translate(&touch(5, PenTouchPhase::Move, 300, 400))
                .is_err()
        );
        assert_eq!(
            translator
                .translate(&touch(4, PenTouchPhase::Up, 100, 200))
                .unwrap(),
            [
                InputEvent::Absolute { x: 100, y: 200 },
                left_button_event(false),
            ]
        );
    }

    #[test]
    fn native_pen_and_touch_metadata_are_explicitly_unsupported() {
        let mut translator = SingleTouchAsMouse::default();
        let mut pen = touch(0, PenTouchPhase::Down, 0, 0);
        if let Event::PenTouch { device, .. } = &mut pen {
            *device = PenTouchDevice::Pen;
        }
        assert!(translator.translate(&pen).is_err());

        let mut pressure_touch = touch(0, PenTouchPhase::Down, 0, 0);
        if let Event::PenTouch { pressure, .. } = &mut pressure_touch {
            *pressure = Some(0.5);
        }
        assert!(translator.translate(&pressure_touch).is_err());

        let mut area_touch = touch(0, PenTouchPhase::Down, 0, 0);
        if let Event::PenTouch {
            contact_area_major,
            contact_area_minor,
            ..
        } = &mut area_touch
        {
            *contact_area_major = Some(0.5);
            *contact_area_minor = Some(0.5);
        }
        assert!(translator.translate(&area_touch).is_err());

        let mut rotated_touch = touch(0, PenTouchPhase::Down, 0, 0);
        if let Event::PenTouch {
            rotation_degrees, ..
        } = &mut rotated_touch
        {
            *rotation_degrees = Some(90);
        }
        assert!(translator.translate(&rotated_touch).is_err());
        assert_eq!(
            translator
                .translate(&touch(0, PenTouchPhase::Down, 0, 0))
                .unwrap(),
            vec![InputEvent::Absolute { x: 0, y: 0 }, left_button_event(true)]
        );
    }

    #[test]
    fn backend_reports_one_mouse_mapped_touch_contact_and_no_pen_contacts() {
        let backend = LinuxCompanionBackend::new("unused.sock".into());
        assert_eq!(backend.name(), "linux_uinput_companion_touch_as_mouse");
        assert!(!backend.pen_touch_supported());
        assert!(backend.supports_pen_touch_event(PenTouchDevice::Touch));
        assert!(!backend.supports_pen_touch_event(PenTouchDevice::Pen));
        assert_eq!(backend.touch_as_mouse_max_contacts(), Some(1));
    }

    #[derive(Default)]
    struct ObservedCompanion {
        submitted: Vec<InputEvent>,
        releases: usize,
        fail_on_left_down: bool,
    }

    struct FakeCompanionDevice(Arc<Mutex<ObservedCompanion>>);

    impl DeviceSink for FakeCompanionDevice {
        fn submit(&mut self, event: &InputEvent) -> InputResult<()> {
            let mut observed = self.0.lock().expect("companion fixture lock");
            observed.submitted.push(event.clone());
            if observed.fail_on_left_down
                && matches!(
                    event,
                    InputEvent::MouseButton {
                        button: CompanionMouseButton::Left,
                        pressed: true,
                    }
                )
            {
                observed.fail_on_left_down = false;
                return Err(CompanionError::Device);
            }
            Ok(())
        }

        fn release_all(&mut self) -> InputResult<()> {
            self.0.lock().expect("companion fixture lock").releases += 1;
            Ok(())
        }
    }

    fn companion_session(
        fail_on_left_down: bool,
    ) -> (
        InputSession<FakeCompanionDevice>,
        Arc<Mutex<ObservedCompanion>>,
        Instant,
    ) {
        let now = Instant::now();
        let peer = PeerIdentity::from_verified_connection("test-worker".into(), 1, "a".repeat(64))
            .unwrap();
        let mut registry = GrantRegistry::session_only();
        let grant = registry
            .issue(
                peer.clone(),
                "b".repeat(32),
                1,
                now + Duration::from_secs(10),
                false,
                true,
            )
            .unwrap();
        let observed = Arc::new(Mutex::new(ObservedCompanion {
            fail_on_left_down,
            ..Default::default()
        }));
        let session = InputSession::connect(
            &peer,
            grant,
            FakeCompanionDevice(Arc::clone(&observed)),
            now,
        )
        .unwrap();
        (session, observed, now)
    }

    fn submit_intents(
        session: &mut InputSession<FakeCompanionDevice>,
        events: Vec<InputEvent>,
        first_sequence: u64,
        now: Instant,
    ) -> InputResult<()> {
        for (offset, event) in events.into_iter().enumerate() {
            let packet = InputPacket {
                version: PROTOCOL_VERSION,
                generation: 1,
                sequence: first_sequence + offset as u64,
                lease_token: "b".repeat(32),
                event,
            };
            session.submit(&packet, now)?;
        }
        Ok(())
    }

    #[test]
    fn companion_revoke_releases_the_touch_mapped_left_button() {
        let mut translator = SingleTouchAsMouse::default();
        let down = translator
            .translate(&touch(4, PenTouchPhase::Down, 100, 200))
            .unwrap();
        let (mut session, observed, now) = companion_session(false);
        submit_intents(&mut session, down, 1, now).unwrap();
        session.revoke().unwrap();

        let observed = observed.lock().unwrap();
        assert_eq!(
            observed.submitted,
            [
                InputEvent::Absolute { x: 100, y: 200 },
                left_button_event(true),
                left_button_event(false),
            ]
        );
        assert_eq!(observed.releases, 1);
    }

    #[test]
    fn partial_touch_down_failure_triggers_companion_button_cleanup() {
        let mut translator = SingleTouchAsMouse::default();
        let down = translator
            .translate(&touch(4, PenTouchPhase::Down, 100, 200))
            .unwrap();
        let (mut session, observed, now) = companion_session(true);

        assert!(matches!(
            submit_intents(&mut session, down, 1, now),
            Err(CompanionError::Device)
        ));
        let observed = observed.lock().unwrap();
        assert_eq!(
            observed.submitted,
            [
                InputEvent::Absolute { x: 100, y: 200 },
                left_button_event(true),
                left_button_event(false),
            ]
        );
        assert_eq!(observed.releases, 1);
    }

    #[test]
    fn absolute_pointer_mapping_preserves_normalized_coordinate_bounds() {
        assert_eq!(
            absolute_pointer_event(0, u16::MAX),
            InputEvent::Absolute { x: 0, y: u16::MAX }
        );
    }

    #[test]
    fn key_repeat_uses_evdev_repeat_without_a_transition() {
        assert_eq!(
            evdev_key_event(30, KeyState::Repeat),
            InputEvent::EvdevKeyRepeat { code: 30 }
        );
        assert_eq!(
            evdev_key_event(30, KeyState::Down),
            InputEvent::EvdevKey {
                code: 30,
                pressed: true,
            }
        );
        assert_eq!(
            evdev_key_event(30, KeyState::Up),
            InputEvent::EvdevKey {
                code: 30,
                pressed: false,
            }
        );
    }

    #[test]
    fn high_resolution_scroll_chunks_stay_within_companion_limit_and_preserve_sum() {
        let cases: [(i16, &[i32]); 2] = [
            (i16::MAX, &[12_000, 12_000, 8_767]),
            (i16::MIN, &[-12_000, -12_000, -8_768]),
        ];
        for (amount, expected) in cases {
            let events = scroll_events(Axis::Vertical, amount);
            let actual = events
                .iter()
                .map(|event| match event {
                    InputEvent::Scroll {
                        vertical,
                        horizontal: 0,
                    } => i32::from(*vertical),
                    _ => panic!("vertical scroll must keep its axis"),
                })
                .collect::<Vec<_>>();

            assert_eq!(actual.as_slice(), expected);
            assert_eq!(actual.iter().copied().sum::<i32>(), i32::from(amount));
            assert!(actual.iter().all(|chunk| chunk.abs() <= 12_000));
        }
    }

    #[test]
    fn horizontal_scroll_chunks_keep_the_horizontal_axis() {
        assert_eq!(
            scroll_events(Axis::Horizontal, 25_000),
            vec![
                InputEvent::Scroll {
                    vertical: 0,
                    horizontal: 12_000,
                },
                InputEvent::Scroll {
                    vertical: 0,
                    horizontal: 12_000,
                },
                InputEvent::Scroll {
                    vertical: 0,
                    horizontal: 1_000,
                },
            ]
        );
    }

    #[test]
    fn gamepad_event_maps_slot_buttons_triggers_and_axes_without_widening_input() {
        let event = Event::Controller {
            number: 2,
            active_mask: 1 << 2,
            buttons: (1 << 1) | (1 << 12),
            left_trigger: u8::MAX,
            right_trigger: 64,
            left_x: i16::MIN,
            left_y: i16::MAX,
            right_x: -1,
            right_y: 1,
        };
        assert_eq!(
            companion_gamepad_event(&event).unwrap(),
            InputEvent::Gamepad {
                number: 2,
                active_mask: 1 << 2,
                buttons: (1 << 1) | (1 << 12),
                left_trigger: u8::MAX,
                right_trigger: 64,
                left_x: i16::MIN,
                left_y: i16::MAX,
                right_x: -1,
                right_y: 1,
            }
        );
        let mut invalid = event;
        if let Event::Controller { buttons, .. } = &mut invalid {
            *buttons = 1 << 11;
        }
        assert_eq!(
            companion_gamepad_event(&invalid).unwrap_err().code,
            "invalid_event"
        );
    }

    #[test]
    fn gamepad_capability_is_closed_until_a_host_grant_is_returned() {
        let mut backend = LinuxCompanionBackend::new("unused.sock".into());
        backend.set_gamepad_profile(GamepadProfile::Auto);
        assert!(!backend.controller_input_supported());
        assert_eq!(
            backend
                .validate(&Event::Controller {
                    number: 0,
                    active_mask: 1,
                    buttons: 0,
                    left_trigger: 0,
                    right_trigger: 0,
                    left_x: 0,
                    left_y: 0,
                    right_x: 0,
                    right_y: 0,
                })
                .unwrap_err()
                .code,
            "controller_unavailable"
        );
    }
}
