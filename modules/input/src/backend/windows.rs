use super::Backend;
use super::owned_keys::{HeldKeyPlans, HostModifier, OwnedKeyPlan};
use crate::gamepad_profile::GamepadProfile;
#[path = "windows_gamepad.rs"]
mod windows_gamepad;
#[path = "windows_pen_touch.rs"]
mod windows_pen_touch;
use crate::model::{
    Axis, Button, Event, InputError, InputResult, Key, KeyState, PenTouchDevice,
    is_temporary_modifier,
};
use amaterasu_input_companion::WindowsInputClient;
use amaterasu_input_policy::{KeyBindings, host_modifier_identity};
use std::collections::HashMap;
use windows_gamepad::WindowsGamepads;
use windows_pen_touch::WindowsPenTouch;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;

pub struct WindowsBackend {
    vhf_companion: bool,
    companion: Option<WindowsInputClient>,
    always_send_scancodes: bool,
    key_rightalt_to_key_win: bool,
    keybindings: KeyBindings,
    held_key_plans: HeldKeyPlans<KeyboardPlan>,
    companion_key_plans: HashMap<u8, KeyboardPlan>,
    gamepads: WindowsGamepads,
    pen_touch: WindowsPenTouch,
}

impl WindowsBackend {
    pub fn new(_: String) -> Self {
        Self::new_with_companion(false)
    }

    pub fn new_with_companion(vhf_companion: bool) -> Self {
        Self {
            vhf_companion,
            companion: None,
            always_send_scancodes: true,
            key_rightalt_to_key_win: false,
            keybindings: KeyBindings::default(),
            held_key_plans: HeldKeyPlans::new(),
            companion_key_plans: HashMap::new(),
            gamepads: WindowsGamepads::new(),
            pen_touch: WindowsPenTouch::new(),
        }
    }

    fn keyboard_plan(&self, key: &Key) -> InputResult<KeyboardPlan> {
        self.keyboard_binding(key).map(|binding| binding.native)
    }

    fn keyboard_binding(&self, key: &Key) -> InputResult<OwnedKeyPlan<KeyboardPlan>> {
        match key {
            Key::WindowsScan { code, extended } => {
                let native = KeyboardPlan::Scan {
                    code: *code,
                    extended: *extended,
                };
                Ok(OwnedKeyPlan {
                    target: *code,
                    native,
                    modifier: native.modifier(),
                })
            }
            Key::MoonlightVk {
                code,
                normalized,
                temporary_modifier,
                ..
            } => {
                if let Some(held) = self.held_key_plans.held(*code) {
                    return Ok(held);
                }
                let source = *code;
                let code = self
                    .keybindings
                    .resolve(*code, self.key_rightalt_to_key_win);
                let target = u16::from(host_modifier_identity(code));
                if let Some(held) = self.held_key_plans.target_plan(target) {
                    return Ok(held);
                }
                let native = if *normalized {
                    normalized_keyboard_plan(code)
                } else {
                    raw_keyboard_plan(code, self.always_send_scancodes, map_virtual_key)
                }?;
                Ok(self.held_key_plans.select(
                    source,
                    OwnedKeyPlan {
                        target,
                        native,
                        modifier: native.modifier(),
                    },
                    *temporary_modifier,
                ))
            }
            Key::LinuxEvdev { .. } => Err(InputError::new(
                "unsupported_event",
                "Windows requires scan codes or Moonlight virtual keys",
            )),
        }
    }

    fn moonlight_key_identity(key: &Key) -> Option<u8> {
        match key {
            Key::MoonlightVk { code, .. } => Some(*code),
            _ => None,
        }
    }

    fn event_keyboard_plan(&self, event: &Event) -> InputResult<KeyboardPlan> {
        let Event::Key { key, .. } = event else {
            return Ok(KeyboardPlan::Scan {
                code: 0,
                extended: false,
            });
        };
        self.keyboard_plan(key)
    }

    fn inject_with_sender(
        &mut self,
        event: &Event,
        mut send_input: impl FnMut(&INPUT, Option<KeyboardPlan>) -> InputResult<()>,
    ) -> InputResult<()> {
        self.validate(event)?;
        if matches!(
            event,
            Event::Controller { .. }
                | Event::ControllerArrival { .. }
                | Event::ControllerTouch { .. }
                | Event::ControllerMotion { .. }
                | Event::ControllerBattery { .. }
        ) {
            return self.gamepads.inject(event);
        }
        if matches!(event, Event::PenTouch { .. }) {
            return self.pen_touch.inject(event);
        }
        if let Event::Key { key, state } = event
            && let Some(identity) = Self::moonlight_key_identity(key)
        {
            let binding = self.keyboard_binding(key)?;
            return self.held_key_plans.apply(
                identity,
                *state,
                binding,
                is_temporary_modifier(key),
                |plan| {
                    let input = native_input_with_plan(event, plan)?;
                    send_input(&input, Some(plan))
                },
            );
        }
        let plan = self.event_keyboard_plan(event)?;
        let input = native_input_with_plan(event, plan)?;
        send_input(&input, None)?;
        Ok(())
    }
}

impl Backend for WindowsBackend {
    fn name(&self) -> &'static str {
        if self.vhf_companion {
            "windows_vhf_companion"
        } else {
            "windows_send_input"
        }
    }

    fn supported(&self) -> bool {
        true
    }

    fn absolute_pointer(&self) -> bool {
        !self.vhf_companion
    }

    fn controller_input_supported(&self) -> bool {
        !self.vhf_companion && self.gamepads.supported()
    }

    fn pen_touch_supported(&self) -> bool {
        !self.vhf_companion && self.pen_touch.supported()
    }

    fn pen_touch_max_contacts(&self, device: PenTouchDevice) -> Option<usize> {
        if self.vhf_companion {
            None
        } else {
            Some(self.pen_touch.max_contacts(device))
        }
    }

    fn effective_gamepad_profile(&self) -> Option<&'static str> {
        if self.vhf_companion {
            None
        } else {
            self.gamepads.effective_profile()
        }
    }

    fn gamepad_unavailable_reason(&self) -> Option<&str> {
        if self.vhf_companion {
            Some("VHF companion currently supports keyboard and mouse only")
        } else {
            self.gamepads.unavailable_reason()
        }
    }

    fn key_encoding(&self) -> &'static str {
        "windows_scan"
    }

    fn validate(&self, event: &Event) -> InputResult<()> {
        if self.vhf_companion {
            return companion_intent(
                event,
                self.key_rightalt_to_key_win,
                &self.keybindings,
                &self.companion_key_plans,
            )
            .map(|_| ());
        }
        event.validate()?;
        if matches!(event, Event::PenTouch { .. }) {
            // Controller batch validation calls this before any batch event is
            // injected, so contact lifecycle checks belong in inject instead.
            return self.pen_touch.validate(event);
        }
        self.gamepads.validate(event)?;
        if matches!(
            event,
            Event::Key {
                key: Key::LinuxEvdev { .. },
                ..
            }
        ) {
            return Err(InputError::new(
                "unsupported_event",
                "Windows requires scan codes or Moonlight virtual keys",
            ));
        }
        Ok(())
    }

    fn start(&mut self) -> InputResult<()> {
        if self.vhf_companion {
            self.companion = Some(WindowsInputClient::connect().map_err(companion_error)?);
            self.companion_key_plans.clear();
            return Ok(());
        }
        self.gamepads.start()?;
        self.pen_touch.start()?;
        self.held_key_plans.clear();
        Ok(())
    }

    fn inject(&mut self, event: &Event) -> InputResult<()> {
        if self.vhf_companion {
            let Some(translated) = companion_intent(
                event,
                self.key_rightalt_to_key_win,
                &self.keybindings,
                &self.companion_key_plans,
            )?
            else {
                // Windows HID typematic repeats a held keyboard report. An
                // application-level Repeat must not submit a second down.
                return Ok(());
            };
            if let Some(companion) = &mut self.companion {
                if let Err(error) = companion.send(translated.clone()) {
                    // Closing the pipe makes the companion destroy its HID
                    // child. Let subsequent releases clear controller state.
                    self.companion = None;
                    self.companion_key_plans.clear();
                    if !companion_release(event) {
                        return Err(companion_error(error));
                    }
                }
                record_companion_key_plan(event, &translated, &mut self.companion_key_plans);
                return Ok(());
            }
            self.companion_key_plans.clear();
            return if companion_release(event) {
                Ok(())
            } else {
                Err(InputError::new(
                    "companion_unavailable",
                    "VHF companion lease is not active",
                ))
            };
        }
        self.inject_with_sender(event, |input, _held_plan_snapshot| {
            // SAFETY: input is a fully initialized INPUT with the matching union tag.
            // SendInput reads this one owned record synchronously and retains no pointer.
            let accepted = unsafe { SendInput(1, input, std::mem::size_of::<INPUT>() as i32) };
            if accepted != 1 {
                return Err(InputError::new(
                    "injection_failed",
                    format!(
                        "SendInput accepted {accepted}/1 events (desktop/UIPI restrictions may apply): {}",
                        std::io::Error::last_os_error()
                    ),
                ));
            }
            Ok(())
        })
    }

    fn set_always_send_scancodes(&mut self, enabled: bool) {
        self.always_send_scancodes = enabled;
    }

    fn set_key_rightalt_to_key_win(&mut self, enabled: bool) {
        self.key_rightalt_to_key_win = enabled;
    }

    fn set_keybindings(&mut self, keybindings: &KeyBindings) {
        self.keybindings = keybindings.clone();
    }

    fn advance(&mut self, now: std::time::Instant) -> InputResult<()> {
        if self.vhf_companion {
            return Ok(());
        }
        self.gamepads.advance(now);
        self.pen_touch.advance(now)
    }

    fn advance_pen_touch(&mut self, now: std::time::Instant) -> InputResult<()> {
        if self.vhf_companion {
            return Ok(());
        }
        self.pen_touch.advance(now)
    }

    fn set_back_button_timeout(&mut self, timeout_ms: i32) {
        self.gamepads.set_back_button_timeout(timeout_ms);
    }

    fn set_gamepad_profile(&mut self, profile: GamepadProfile) {
        self.gamepads.set_profile(profile);
    }

    fn set_ds4_back_as_touchpad_click(&mut self, enabled: bool) {
        self.gamepads.set_ds4_back_as_touchpad_click(enabled);
    }

    fn set_motion_as_ds4(&mut self, enabled: bool) {
        self.gamepads.set_motion_as_ds4(enabled);
    }

    fn set_touchpad_as_ds4(&mut self, enabled: bool) {
        self.gamepads.set_touchpad_as_ds4(enabled);
    }

    fn disable_controller_input(&mut self) -> InputResult<()> {
        if self.vhf_companion {
            return Ok(());
        }
        self.gamepads.disable()
    }

    fn stop(&mut self) -> InputResult<()> {
        if self.vhf_companion {
            self.companion_key_plans.clear();
            if let Some(mut companion) = self.companion.take() {
                // The server destroys its child on disconnect. Discard the
                // terminal pipe even if its close acknowledgement is lost.
                companion.close().map_err(companion_error)?;
            }
            return Ok(());
        }
        let gamepad_result = self.gamepads.stop();
        let pen_touch_result = self.pen_touch.stop();
        self.held_key_plans.clear();
        gamepad_result?;
        pen_touch_result
    }
}

fn companion_intent(
    event: &Event,
    key_rightalt_to_key_win: bool,
    keybindings: &KeyBindings,
    held_key_plans: &HashMap<u8, KeyboardPlan>,
) -> InputResult<Option<amaterasu_input_companion::InputEvent>> {
    event.validate()?;
    if let Event::Key {
        key: Key::MoonlightVk { code, .. },
        state: state @ (KeyState::Up | KeyState::Repeat),
    } = event
        && let Some(KeyboardPlan::Scan {
            code: held_scan,
            extended,
        }) = held_key_plans.get(code)
    {
        event.validate()?;
        if *state == KeyState::Repeat {
            return Ok(None);
        }
        return Ok(Some(amaterasu_input_companion::InputEvent::Key {
            scan_code: u8::try_from(*held_scan).expect("held VHF scan code fits u8"),
            extended: *extended,
            pressed: false,
        }));
    }
    // Resolve both explicit bindings and the Right Alt policy before either
    // normalized or layout-dependent translation. Held releases above keep
    // the accepted down plan across subsequent configuration changes.
    let mut mapped_event = event.clone();
    if let Event::Key {
        key: Key::MoonlightVk { code, .. },
        ..
    } = &mut mapped_event
    {
        *code = keybindings.resolve(*code, key_rightalt_to_key_win);
    }
    crate::companion_client::translate(&mapped_event, false, |code| {
        let scan_code = map_virtual_key(code);
        (scan_code != 0).then_some((scan_code, is_extended_virtual_key(code)))
    })
}

fn record_companion_key_plan(
    event: &Event,
    translated: &amaterasu_input_companion::InputEvent,
    held_key_plans: &mut HashMap<u8, KeyboardPlan>,
) {
    let Event::Key {
        key: Key::MoonlightVk { code, .. },
        state,
    } = event
    else {
        return;
    };
    match (state, translated) {
        (
            KeyState::Down,
            amaterasu_input_companion::InputEvent::Key {
                scan_code,
                extended,
                pressed: true,
            },
        ) => {
            held_key_plans.insert(
                *code,
                KeyboardPlan::Scan {
                    code: u16::from(*scan_code),
                    extended: *extended,
                },
            );
        }
        (KeyState::Up, _) => {
            held_key_plans.remove(code);
        }
        _ => {}
    }
}

fn companion_release(event: &Event) -> bool {
    matches!(
        event,
        Event::Key {
            state: KeyState::Up,
            ..
        } | Event::Button { pressed: false, .. }
    )
}

fn companion_error(error: amaterasu_input_companion::InputError) -> InputError {
    InputError::new(
        "companion_unavailable",
        format!("VHF companion denied input: {error:?}"),
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum KeyboardPlan {
    Scan { code: u16, extended: bool },
    Virtual { code: u8, extended: bool },
}

impl KeyboardPlan {
    const fn modifier(self) -> Option<HostModifier> {
        match self {
            Self::Scan {
                code: 0x2a | 0x36,
                extended: false,
            } => Some(HostModifier::Shift),
            Self::Scan { code: 0x1d, .. } => Some(HostModifier::Control),
            Self::Scan { code: 0x38, .. } => Some(HostModifier::Alt),
            Self::Virtual {
                code: 0x10 | 0xa0 | 0xa1,
                ..
            } => Some(HostModifier::Shift),
            Self::Virtual {
                code: 0x11 | 0xa2 | 0xa3,
                ..
            } => Some(HostModifier::Control),
            Self::Virtual {
                code: 0x12 | 0xa4 | 0xa5,
                ..
            } => Some(HostModifier::Alt),
            _ => None,
        }
    }
}

fn normalized_keyboard_plan(code: u8) -> InputResult<KeyboardPlan> {
    let mapping = amaterasu_gamestream::control::virtual_key_mapping(code)
        .ok_or_else(|| InputError::new("unsupported_event", "unmapped normalized Moonlight key"))?;
    Ok(KeyboardPlan::Scan {
        code: mapping.windows_scan_code,
        extended: mapping.windows_extended,
    })
}

fn raw_keyboard_plan(
    code: u8,
    always_send_scancodes: bool,
    mut map_virtual_key: impl FnMut(u8) -> u16,
) -> InputResult<KeyboardPlan> {
    if always_send_scancodes && !matches!(code, 0x5b | 0x5c | 0x13) {
        let scan_code = map_virtual_key(code);
        if scan_code != 0 {
            return Ok(KeyboardPlan::Scan {
                code: scan_code,
                extended: is_extended_virtual_key(code),
            });
        }
    }
    Ok(KeyboardPlan::Virtual {
        code,
        extended: is_extended_virtual_key(code),
    })
}

fn is_extended_virtual_key(code: u8) -> bool {
    matches!(
        code,
        0x5b | 0x5c
            | 0xa3
            | 0xa5
            | 0x2d
            | 0x2e
            | 0x24
            | 0x23
            | 0x21
            | 0x22
            | 0x25
            | 0x27
            | 0x26
            | 0x28
            | 0x6f
            | 0x5d
    )
}

fn map_virtual_key(code: u8) -> u16 {
    // SAFETY: MapVirtualKeyW consumes scalar key/mapping arguments and retains
    // no pointers or references. Its result is a numeric scan code.
    unsafe { MapVirtualKeyW(u32::from(code), MAPVK_VK_TO_VSC) as u16 }
}

fn native_input_with_plan(event: &Event, plan: KeyboardPlan) -> InputResult<INPUT> {
    if let Event::Key { state, .. } = event {
        let (w_vk, w_scan, mut flags) = match plan {
            KeyboardPlan::Scan { code, extended } => {
                (0, code, KEYEVENTF_SCANCODE | extended_flag(extended))
            }
            KeyboardPlan::Virtual { code, extended } => {
                (u16::from(code), 0, extended_flag(extended))
            }
        };
        if *state == KeyState::Up {
            flags |= KEYEVENTF_KEYUP;
        }
        return Ok(INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: w_vk,
                    wScan: w_scan,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        });
    }

    let mut mouse = MOUSEINPUT::default();
    match event {
        Event::Button { button, pressed } => {
            mouse.dwFlags = match (button, pressed) {
                (Button::Left, true) => MOUSEEVENTF_LEFTDOWN,
                (Button::Left, false) => MOUSEEVENTF_LEFTUP,
                (Button::Right, true) => MOUSEEVENTF_RIGHTDOWN,
                (Button::Right, false) => MOUSEEVENTF_RIGHTUP,
                (Button::Middle, true) => MOUSEEVENTF_MIDDLEDOWN,
                (Button::Middle, false) => MOUSEEVENTF_MIDDLEUP,
                (_, true) => MOUSEEVENTF_XDOWN,
                (_, false) => MOUSEEVENTF_XUP,
            };
            mouse.mouseData = match button {
                Button::Back => 1,
                Button::Forward => 2,
                _ => 0,
            };
        }
        Event::Relative { dx, dy } => {
            mouse.dx = *dx;
            mouse.dy = *dy;
            mouse.dwFlags = MOUSEEVENTF_MOVE | MOUSEEVENTF_MOVE_NOCOALESCE;
        }
        Event::Absolute { x, y } => {
            mouse.dx = i32::from(*x);
            mouse.dy = i32::from(*y);
            mouse.dwFlags = MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK;
        }
        Event::Scroll { axis, steps } => {
            mouse.dwFlags = match axis {
                Axis::Horizontal => MOUSEEVENTF_HWHEEL,
                Axis::Vertical => MOUSEEVENTF_WHEEL,
            };
            mouse.mouseData = (*steps * 120) as u32;
        }
        Event::ScrollUnits { axis, amount } => {
            mouse.dwFlags = match axis {
                Axis::Horizontal => MOUSEEVENTF_HWHEEL,
                Axis::Vertical => MOUSEEVENTF_WHEEL,
            };
            mouse.mouseData = i32::from(*amount) as u32;
        }
        Event::Controller { .. } => {
            return Err(InputError::new(
                "unsupported_event",
                "Windows SendInput does not create virtual gamepads",
            ));
        }
        Event::ControllerArrival { .. }
        | Event::ControllerTouch { .. }
        | Event::ControllerMotion { .. }
        | Event::ControllerBattery { .. } => {
            return Err(InputError::new(
                "unsupported_event",
                "Windows SendInput does not create virtual gamepads",
            ));
        }
        Event::PenTouch { .. } => {
            return Err(InputError::new(
                "unsupported_event",
                "Windows pen/touch events use synthetic pointer injection",
            ));
        }
        Event::Key { .. } => unreachable!("keyboard events handled above"),
    }
    Ok(INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 { mi: mouse },
    })
}

const fn extended_flag(extended: bool) -> u32 {
    if extended { KEYEVENTF_EXTENDEDKEY } else { 0 }
}

#[cfg(test)]
fn native_input(event: &Event) -> InputResult<INPUT> {
    let plan = match event {
        Event::Key { key, .. } => match key {
            Key::WindowsScan { code, extended } => KeyboardPlan::Scan {
                code: *code,
                extended: *extended,
            },
            Key::MoonlightVk {
                code,
                normalized: true,
                ..
            } => return native_input_with_plan(event, normalized_keyboard_plan(*code)?),
            Key::MoonlightVk {
                code,
                normalized: false,
                ..
            } => raw_keyboard_plan(*code, true, |_| 0)?,
            Key::LinuxEvdev { .. } => {
                return Err(InputError::new(
                    "unsupported_event",
                    "Windows requires scan codes or Moonlight virtual keys",
                ));
            }
        },
        _ => KeyboardPlan::Scan {
            code: 0,
            extended: false,
        },
    };
    native_input_with_plan(event, plan)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vhf_release_and_repeat_use_the_accepted_down_mapping() {
        let key = Key::MoonlightVk {
            code: 0x41,
            normalized: false,
            repeat_modifiers: 0,
            temporary_modifier: false,
        };
        let mut held = HashMap::new();
        let down = Event::Key {
            key: key.clone(),
            state: KeyState::Down,
        };
        record_companion_key_plan(
            &down,
            &amaterasu_input_companion::InputEvent::Key {
                scan_code: 0x1e,
                extended: true,
                pressed: true,
            },
            &mut held,
        );
        let released = Event::Key {
            key: key.clone(),
            state: KeyState::Up,
        };
        assert_eq!(
            companion_intent(&released, true, &KeyBindings::empty(), &held).unwrap(),
            Some(amaterasu_input_companion::InputEvent::Key {
                scan_code: 0x1e,
                extended: true,
                pressed: false,
            })
        );
        let repeat = Event::Key {
            key,
            state: KeyState::Repeat,
        };
        assert_eq!(
            companion_intent(&repeat, true, &KeyBindings::empty(), &held).unwrap(),
            None
        );
        record_companion_key_plan(
            &released,
            &amaterasu_input_companion::InputEvent::Key {
                scan_code: 0x1e,
                extended: true,
                pressed: false,
            },
            &mut held,
        );
        assert!(held.is_empty());
    }

    #[test]
    fn vhf_keybindings_preserve_the_accepted_release_after_policy_changes() {
        let mut backend = WindowsBackend::new_with_companion(true);
        backend.set_keybindings(&KeyBindings::from_flat(vec![0x41, 0x42]).unwrap());
        let down = key(0x41, true, KeyState::Down);
        let translated = companion_intent(
            &down,
            backend.key_rightalt_to_key_win,
            &backend.keybindings,
            &backend.companion_key_plans,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            translated,
            amaterasu_input_companion::InputEvent::Key {
                scan_code: 0x30,
                extended: false,
                pressed: true,
            }
        );
        record_companion_key_plan(&down, &translated, &mut backend.companion_key_plans);
        backend.set_keybindings(&KeyBindings::from_flat(vec![0x41, 0x43]).unwrap());
        let release = key(0x41, true, KeyState::Up);
        let translated = companion_intent(
            &release,
            backend.key_rightalt_to_key_win,
            &backend.keybindings,
            &backend.companion_key_plans,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            translated,
            amaterasu_input_companion::InputEvent::Key {
                scan_code: 0x30,
                extended: false,
                pressed: false,
            }
        );
        record_companion_key_plan(&release, &translated, &mut backend.companion_key_plans);
        assert!(backend.companion_key_plans.is_empty());
        assert!(backend.held_key_plans.is_empty());
    }

    #[test]
    fn vhf_companion_requires_a_live_lease_without_native_fallback() {
        let mut selected = WindowsBackend::new_with_companion(true);
        let event = Event::Key {
            key: Key::WindowsScan {
                code: 0x1e,
                extended: false,
            },
            state: KeyState::Down,
        };
        assert_eq!(
            WindowsBackend::new(String::new()).name(),
            "windows_send_input"
        );
        assert_eq!(selected.name(), "windows_vhf_companion");
        selected.validate(&event).unwrap();
        assert_eq!(
            selected.inject(&event).unwrap_err().code,
            "companion_unavailable"
        );
        assert!(!selected.absolute_pointer());
        assert!(!selected.controller_input_supported());
        let repeated = Event::Key {
            key: Key::WindowsScan {
                code: 0x1e,
                extended: false,
            },
            state: KeyState::Repeat,
        };
        selected.validate(&repeated).unwrap();
        selected.inject(&repeated).unwrap();
        let released = Event::Key {
            key: Key::WindowsScan {
                code: 0x1e,
                extended: false,
            },
            state: KeyState::Up,
        };
        selected.inject(&released).unwrap();
    }

    fn key(code: u8, normalized: bool, state: KeyState) -> Event {
        Event::Key {
            key: Key::MoonlightVk {
                code,
                normalized,
                repeat_modifiers: 0,
                temporary_modifier: false,
            },
            state,
        }
    }

    fn temporary_key(code: u8, state: KeyState) -> Event {
        Event::Key {
            key: Key::MoonlightVk {
                code,
                normalized: true,
                repeat_modifiers: 0,
                temporary_modifier: true,
            },
            state,
        }
    }

    fn keyboard_fields(input: &INPUT) -> (u16, u16, u32) {
        let record = keyboard_record_ref(input);
        (record.wVk, record.wScan, record.dwFlags)
    }

    #[test]
    fn temporary_chords_reuse_remapped_right_modifiers_without_changing_flags_or_real_left_keys() {
        for (generic, left, right, left_scan, right_scan, extended) in [
            (0x10, 0xa0, 0xa1, 0x2a, 0x36, false),
            (0x11, 0xa2, 0xa3, 0x1d, 0x1d, true),
            (0x12, 0xa4, 0xa5, 0x38, 0x38, true),
        ] {
            for first_normalized in [true, false] {
                let mut backend = WindowsBackend::new(String::new());
                backend.set_always_send_scancodes(false);
                backend.set_keybindings(
                    &KeyBindings::from_flat(vec![0x41, right, right, 0x44]).unwrap(),
                );
                let first_plan = if first_normalized {
                    KeyboardPlan::Scan {
                        code: right_scan,
                        extended,
                    }
                } else {
                    KeyboardPlan::Virtual {
                        code: right,
                        extended,
                    }
                };
                let first_fields = if first_normalized {
                    (0, right_scan, KEYEVENTF_SCANCODE | extended_flag(extended))
                } else {
                    (u16::from(right), 0, extended_flag(extended))
                };
                let mut records = Vec::new();
                backend
                    .inject_with_sender(
                        &key(0x41, first_normalized, KeyState::Down),
                        |input, held| {
                            assert_eq!(held, Some(first_plan));
                            assert_eq!(keyboard_fields(input), first_fields);
                            records.push(keyboard_fields(input));
                            Ok(())
                        },
                    )
                    .unwrap();
                let first = backend.held_key_plans.held(0x41).unwrap();
                // The source and mode now resolve differently; reuse its saved host plan.
                backend.set_keybindings(&KeyBindings::empty());
                backend.set_always_send_scancodes(true);
                let synthetic_down = temporary_key(generic, KeyState::Down);
                backend
                    .inject_with_sender(&synthetic_down, |_, _| {
                        panic!("temporary generic chord must not emit an opposite-side down")
                    })
                    .unwrap();
                assert!(is_temporary_modifier(match &synthetic_down {
                    Event::Key { key, .. } => key,
                    _ => unreachable!(),
                }));
                assert_eq!(backend.held_key_plans.held(generic), Some(first));
                assert_eq!(records.len(), 1);
                backend
                    .inject_with_sender(&key(left, true, KeyState::Down), |input, held| {
                        assert_eq!(
                            held,
                            Some(KeyboardPlan::Scan {
                                code: left_scan,
                                extended: false
                            })
                        );
                        assert_eq!(keyboard_fields(input), (0, left_scan, KEYEVENTF_SCANCODE));
                        records.push(keyboard_fields(input));
                        Ok(())
                    })
                    .unwrap();
                backend
                    .inject_with_sender(&key(left, true, KeyState::Up), |input, _| {
                        assert_eq!(
                            keyboard_fields(input),
                            (0, left_scan, KEYEVENTF_SCANCODE | KEYEVENTF_KEYUP)
                        );
                        records.push(keyboard_fields(input));
                        Ok(())
                    })
                    .unwrap();
                backend
                    .inject_with_sender(&temporary_key(generic, KeyState::Up), |_, _| {
                        panic!("temporary release must retain the original real owner")
                    })
                    .unwrap();
                assert_eq!(backend.held_key_plans.held(0x41), Some(first));
                assert!(!backend.held_key_plans.contains_key(&generic));
                for state in [KeyState::Repeat, KeyState::Up] {
                    backend
                        .inject_with_sender(&key(0x41, !first_normalized, state), |input, held| {
                            assert_eq!(held, Some(first_plan));
                            let expected_flags = first_fields.2
                                | if state == KeyState::Up {
                                    KEYEVENTF_KEYUP
                                } else {
                                    0
                                };
                            assert_eq!(
                                keyboard_fields(input),
                                (first_fields.0, first_fields.1, expected_flags)
                            );
                            records.push(keyboard_fields(input));
                            Ok(())
                        })
                        .unwrap();
                }
                assert_eq!(records.len(), 5);
                assert!(backend.held_key_plans.is_empty());
            }
        }
    }

    #[test]
    fn ambiguous_right_modifier_and_failed_final_temporary_release_preserve_original_plan() {
        let mut backend = WindowsBackend::new(String::new());
        backend.set_always_send_scancodes(false);
        backend.set_keybindings(&KeyBindings::from_flat(vec![0x41, 0xa3]).unwrap());
        let first_plan = KeyboardPlan::Virtual {
            code: 0xa3,
            extended: true,
        };
        assert!(
            backend
                .inject_with_sender(&key(0x41, false, KeyState::Down), |input, held| {
                    assert_eq!(held, Some(first_plan));
                    assert_eq!(keyboard_fields(input), (0xa3, 0, KEYEVENTF_EXTENDEDKEY));
                    Err(InputError::new(
                        "injection_failed",
                        "ambiguous right control down",
                    ))
                })
                .is_err()
        );
        let first = backend.held_key_plans.held(0x41).unwrap();
        backend.set_keybindings(&KeyBindings::empty());
        backend.set_always_send_scancodes(true);
        backend
            .inject_with_sender(&temporary_key(0x11, KeyState::Down), |_, _| {
                panic!("uncertain owned right control still satisfies synthetic control")
            })
            .unwrap();
        backend
            .inject_with_sender(&key(0x41, true, KeyState::Up), |_, _| {
                panic!("temporary source owns the original host plan")
            })
            .unwrap();
        assert!(!backend.held_key_plans.contains_key(&0x41));
        assert_eq!(backend.held_key_plans.held(0x11), Some(first));
        let release = temporary_key(0x11, KeyState::Up);
        assert!(
            backend
                .inject_with_sender(&release, |input, held| {
                    assert_eq!(held, Some(first_plan));
                    assert_eq!(
                        keyboard_fields(input),
                        (0xa3, 0, KEYEVENTF_EXTENDEDKEY | KEYEVENTF_KEYUP)
                    );
                    Err(InputError::new(
                        "injection_failed",
                        "ambiguous final temporary release",
                    ))
                })
                .is_err()
        );
        assert_eq!(backend.held_key_plans.held(0x11), Some(first));
        backend
            .inject_with_sender(&release, |input, held| {
                assert_eq!(held, Some(first_plan));
                assert_eq!(
                    keyboard_fields(input),
                    (0xa3, 0, KEYEVENTF_EXTENDEDKEY | KEYEVENTF_KEYUP)
                );
                Ok(())
            })
            .unwrap();
        assert!(backend.held_key_plans.is_empty());
    }

    fn keyboard_record(input: INPUT) -> KEYBDINPUT {
        keyboard_record_ref(&input)
    }

    fn keyboard_record_ref(input: &INPUT) -> KEYBDINPUT {
        assert_eq!(input.r#type, INPUT_KEYBOARD);
        // SAFETY: callers pass an INPUT generated as INPUT_KEYBOARD.
        unsafe { input.Anonymous.ki }
    }

    #[test]
    fn normalized_keys_keep_the_existing_fixed_us_mapping() {
        let record = keyboard_record(native_input(&key(0x41, true, KeyState::Down)).unwrap());
        assert_eq!(record.wVk, 0);
        assert_eq!(record.wScan, 0x1e);
        assert_eq!(record.dwFlags, KEYEVENTF_SCANCODE);
    }

    #[test]
    fn raw_keys_use_layout_scan_codes_by_default_and_direct_virtual_keys_when_disabled() {
        let mapped = raw_keyboard_plan(0x41, true, |code| {
            assert_eq!(code, 0x41);
            0x1e
        })
        .unwrap();
        assert_eq!(
            mapped,
            KeyboardPlan::Scan {
                code: 0x1e,
                extended: false
            }
        );
        let scan_record = keyboard_record(
            native_input_with_plan(&key(0x41, false, KeyState::Down), mapped).unwrap(),
        );
        assert_eq!(scan_record.wVk, 0);
        assert_eq!(scan_record.wScan, 0x1e);
        assert_eq!(scan_record.dwFlags, KEYEVENTF_SCANCODE);

        let direct =
            raw_keyboard_plan(0x41, false, |_| panic!("disabled mapping is unused")).unwrap();
        let direct_record = keyboard_record(
            native_input_with_plan(&key(0x41, false, KeyState::Down), direct).unwrap(),
        );
        assert_eq!(direct_record.wVk, 0x41);
        assert_eq!(direct_record.wScan, 0);
        assert_eq!(direct_record.dwFlags, 0);
    }

    #[test]
    fn raw_special_keys_and_unmapped_keys_fall_back_to_virtual_key_input() {
        for code in [0x5b, 0x5c, 0x13, 0xff] {
            let mut mapper_called = false;
            let plan = raw_keyboard_plan(code, true, |mapped_code| {
                mapper_called = true;
                if mapped_code == 0xff { 0 } else { 0x1e }
            })
            .unwrap();
            assert_eq!(
                plan,
                KeyboardPlan::Virtual {
                    code,
                    extended: is_extended_virtual_key(code),
                }
            );
            assert_eq!(mapper_called, code == 0xff);
            let record = keyboard_record(
                native_input_with_plan(&key(code, false, KeyState::Up), plan).unwrap(),
            );
            assert_eq!(record.wVk, u16::from(code));
            assert_eq!(record.wScan, 0);
            assert_eq!(record.dwFlags & KEYEVENTF_KEYUP, KEYEVENTF_KEYUP);
        }
    }

    #[test]
    fn raw_right_control_uses_extended_scan_and_virtual_flags() {
        let scan = raw_keyboard_plan(0xa3, true, |_| 0x1d).unwrap();
        let scan_record = keyboard_record(
            native_input_with_plan(&key(0xa3, false, KeyState::Down), scan).unwrap(),
        );
        assert_eq!(
            scan_record.dwFlags,
            KEYEVENTF_SCANCODE | KEYEVENTF_EXTENDEDKEY
        );

        let virtual_key = raw_keyboard_plan(0xa3, false, |_| 0).unwrap();
        let vk_record = keyboard_record(
            native_input_with_plan(&key(0xa3, false, KeyState::Down), virtual_key).unwrap(),
        );
        assert_eq!(vk_record.wVk, 0xa3);
        assert_eq!(vk_record.dwFlags, KEYEVENTF_EXTENDEDKEY);
    }

    #[test]
    fn configuration_changes_do_not_change_the_matching_release_representation() {
        let mut backend = WindowsBackend::new(String::new());
        let down_plan = KeyboardPlan::Scan {
            code: 0x1e,
            extended: false,
        };
        backend.held_key_plans.insert(
            0x41,
            OwnedKeyPlan {
                target: 0x41,
                native: down_plan,
                modifier: None,
            },
        );
        backend.set_always_send_scancodes(false);
        let active = backend
            .event_keyboard_plan(&key(0x41, false, KeyState::Up))
            .unwrap();
        assert_eq!(active, down_plan);
        let release = keyboard_record(
            native_input_with_plan(&key(0x41, false, KeyState::Up), active).unwrap(),
        );
        assert_eq!(release.wVk, 0);
        assert_eq!(release.wScan, 0x1e);
        assert_eq!(release.dwFlags, KEYEVENTF_SCANCODE | KEYEVENTF_KEYUP);
    }

    #[test]
    fn injection_retains_held_plan_across_failed_delivery_and_setting_changes() {
        let mut backend = WindowsBackend::new(String::new());
        backend.set_always_send_scancodes(false);
        let plan = KeyboardPlan::Virtual {
            code: 0x41,
            extended: false,
        };

        let down = key(0x41, false, KeyState::Down);
        let down_result = backend.inject_with_sender(&down, |input, held_plan_snapshot| {
            assert_eq!(held_plan_snapshot, Some(plan));
            let record = keyboard_record_ref(input);
            assert_eq!(record.wVk, 0x41);
            assert_eq!(record.wScan, 0);
            assert_eq!(record.dwFlags, 0);
            Err(InputError::new(
                "injection_failed",
                "simulated key-down failure",
            ))
        });
        assert!(down_result.is_err());
        assert_eq!(backend.held_key_plans.get(&0x41), Some(&plan));

        backend.set_always_send_scancodes(true);

        let repeat = key(0x41, false, KeyState::Repeat);
        backend
            .inject_with_sender(&repeat, |input, held_plan_snapshot| {
                assert_eq!(held_plan_snapshot, Some(plan));
                let record = keyboard_record_ref(input);
                assert_eq!(record.wVk, 0x41);
                assert_eq!(record.wScan, 0);
                assert_eq!(record.dwFlags, 0);
                Ok(())
            })
            .unwrap();
        assert_eq!(backend.held_key_plans.get(&0x41), Some(&plan));

        let up = key(0x41, false, KeyState::Up);
        let up_result = backend.inject_with_sender(&up, |input, held_plan_snapshot| {
            assert_eq!(held_plan_snapshot, Some(plan));
            let record = keyboard_record_ref(input);
            assert_eq!(record.wVk, 0x41);
            assert_eq!(record.wScan, 0);
            assert_eq!(record.dwFlags, KEYEVENTF_KEYUP);
            Err(InputError::new(
                "injection_failed",
                "simulated key-up failure",
            ))
        });
        assert!(up_result.is_err());
        assert_eq!(backend.held_key_plans.get(&0x41), Some(&plan));

        backend
            .inject_with_sender(&up, |input, held_plan_snapshot| {
                assert_eq!(held_plan_snapshot, Some(plan));
                let record = keyboard_record_ref(input);
                assert_eq!(record.wVk, 0x41);
                assert_eq!(record.wScan, 0);
                assert_eq!(record.dwFlags, KEYEVENTF_KEYUP);
                Ok(())
            })
            .unwrap();
        assert!(!backend.held_key_plans.contains_key(&0x41));
    }

    #[test]
    fn modifier_sources_keep_first_host_plan_until_final_source_release() {
        for (generic, left) in [(0x10, 0xa0), (0x11, 0xa2), (0x12, 0xa4)] {
            let mut backend = WindowsBackend::new(String::new());
            backend.set_always_send_scancodes(false);
            let plan = KeyboardPlan::Virtual {
                code: left,
                extended: false,
            };
            backend
                .inject_with_sender(&key(generic, false, KeyState::Down), |input, held| {
                    assert_eq!(held, Some(plan));
                    assert_eq!(keyboard_record_ref(input).wVk, u16::from(left));
                    Ok(())
                })
                .unwrap();
            assert_eq!(backend.held_key_plans.get(&generic), Some(&plan));
            backend.set_always_send_scancodes(true);
            backend
                .inject_with_sender(&key(left, true, KeyState::Down), |_, _| {
                    panic!("second source must share the first host plan without another down")
                })
                .unwrap();
            assert_eq!(backend.held_key_plans.get(&left), Some(&plan));
            assert_eq!(backend.held_key_plans.len(), 2);
            backend
                .set_keybindings(&serde_json::from_value(serde_json::json!([left, 0x41])).unwrap());
            backend
                .inject_with_sender(&key(left, true, KeyState::Repeat), |input, held| {
                    assert_eq!(held, Some(plan));
                    assert_eq!(keyboard_record_ref(input).wVk, u16::from(left));
                    Ok(())
                })
                .unwrap();
            backend
                .inject_with_sender(&key(generic, true, KeyState::Up), |_, _| {
                    panic!("one source cannot release the other source's host key")
                })
                .unwrap();
            assert_eq!(backend.held_key_plans.len(), 1);
            let up = key(left, true, KeyState::Up);
            assert!(
                backend
                    .inject_with_sender(&up, |input, held| {
                        assert_eq!(held, Some(plan));
                        assert_eq!(keyboard_record_ref(input).wVk, u16::from(left));
                        Err(InputError::new(
                            "injection_failed",
                            "simulated release failure",
                        ))
                    })
                    .is_err()
            );
            assert_eq!(backend.held_key_plans.get(&left), Some(&plan));
            backend
                .inject_with_sender(&up, |input, held| {
                    assert_eq!(held, Some(plan));
                    let record = keyboard_record_ref(input);
                    assert_eq!(record.wVk, u16::from(left));
                    assert_eq!(record.dwFlags, KEYEVENTF_KEYUP);
                    Ok(())
                })
                .unwrap();
            assert!(backend.held_key_plans.is_empty());
        }
    }

    #[test]
    fn right_alt_mapping_is_captured_for_key_up_across_setting_changes() {
        for normalized in [false, true] {
            let mut backend = WindowsBackend::new(String::new());
            backend.set_key_rightalt_to_key_win(true);
            let down = key(0xa5, normalized, KeyState::Down);
            let plan = backend
                .keyboard_plan(match &down {
                    Event::Key { key, .. } => key,
                    _ => unreachable!(),
                })
                .unwrap();
            backend.held_key_plans.insert(
                0xa5,
                OwnedKeyPlan {
                    target: 0x5b,
                    native: plan,
                    modifier: plan.modifier(),
                },
            );

            backend.set_key_rightalt_to_key_win(false);
            let release = backend
                .event_keyboard_plan(&key(0xa5, !normalized, KeyState::Up))
                .unwrap();
            assert_eq!(release, plan);
        }
    }

    #[test]
    fn right_alt_mapping_uses_left_windows_key_and_preserves_raw_packet_mode() {
        let mut backend = WindowsBackend::new(String::new());
        let raw_right_alt = key(0xa5, false, KeyState::Down);
        let unmapped = backend
            .keyboard_plan(match &raw_right_alt {
                Event::Key { key, .. } => key,
                _ => unreachable!(),
            })
            .unwrap();
        assert_eq!(
            unmapped,
            raw_keyboard_plan(0xa5, true, map_virtual_key).unwrap()
        );

        backend.set_key_rightalt_to_key_win(true);
        let remapped = backend
            .keyboard_plan(match &raw_right_alt {
                Event::Key { key, .. } => key,
                _ => unreachable!(),
            })
            .unwrap();
        assert_eq!(
            remapped,
            KeyboardPlan::Virtual {
                code: 0x5b,
                extended: true,
            }
        );

        backend.set_always_send_scancodes(false);
        let raw_plan = backend
            .keyboard_plan(match &raw_right_alt {
                Event::Key { key, .. } => key,
                _ => unreachable!(),
            })
            .unwrap();
        assert_eq!(raw_plan, remapped);
    }

    #[test]
    fn encoding_preserves_extended_keys_virtual_desktop_and_signed_scroll() {
        let key = native_input(&Event::Key {
            key: Key::WindowsScan {
                code: 0x1d,
                extended: true,
            },
            state: KeyState::Up,
        })
        .unwrap();
        let key = keyboard_record(key);
        assert_eq!(key.wVk, 0);
        assert_eq!(key.wScan, 0x1d);
        assert_eq!(
            key.dwFlags,
            KEYEVENTF_SCANCODE | KEYEVENTF_EXTENDEDKEY | KEYEVENTF_KEYUP
        );
        let absolute = native_input(&Event::Absolute { x: 65535, y: 0 }).unwrap();
        // SAFETY: absolute events encode INPUT_MOUSE and initialize mi.
        let absolute = unsafe { absolute.Anonymous.mi };
        assert_eq!(absolute.dx, 65535);
        assert_eq!(
            absolute.dwFlags,
            MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK
        );
        let scroll = native_input(&Event::Scroll {
            axis: Axis::Vertical,
            steps: -2,
        })
        .unwrap();
        // SAFETY: scroll events encode INPUT_MOUSE and initialize mi.
        let scroll = unsafe { scroll.Anonymous.mi };
        assert_eq!(scroll.mouseData as i32, -240);

        let high_resolution = native_input(&Event::ScrollUnits {
            axis: Axis::Horizontal,
            amount: -30,
        })
        .unwrap();
        // SAFETY: high-resolution scroll events encode INPUT_MOUSE and initialize mi.
        let high_resolution = unsafe { high_resolution.Anonymous.mi };
        assert_eq!(high_resolution.mouseData as i32, -30);
        assert_eq!(high_resolution.dwFlags, MOUSEEVENTF_HWHEEL);
    }

    #[test]
    fn send_input_rejects_controller_events_without_virtual_gamepad_support() {
        let event = Event::Controller {
            number: 0,
            active_mask: 1,
            buttons: 1,
            left_trigger: 0,
            right_trigger: 0,
            left_x: 0,
            left_y: 0,
            right_x: 0,
            right_y: 0,
        };
        let backend = WindowsBackend::new(String::new());

        assert!(backend.supported());
        assert!(!backend.controller_input_supported());
        assert_eq!(
            backend.validate(&event).unwrap_err().code,
            "controller_unavailable"
        );
        assert_eq!(
            native_input(&event).err().unwrap().code,
            "unsupported_event"
        );

        assert!(
            backend
                .validate(&event)
                .unwrap_err()
                .message
                .contains("ViGEm")
        );
    }

    #[test]
    fn send_input_rejects_pen_touch_that_requires_synthetic_pointer() {
        let event = Event::PenTouch {
            pointer_id: 1,
            device: crate::model::PenTouchDevice::Touch,
            phase: crate::model::PenTouchPhase::Down,
            x: 0,
            y: 0,
            pressure: None,
            contact_area_major: None,
            contact_area_minor: None,
            rotation_degrees: None,
            pen_tool: None,
            pen_buttons: None,
            tilt: None,
        };

        assert_eq!(
            native_input(&event).err().unwrap().code,
            "unsupported_event"
        );
    }
}
