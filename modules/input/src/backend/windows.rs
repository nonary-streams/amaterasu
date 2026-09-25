use super::Backend;
use crate::gamepad_profile::GamepadProfile;
#[path = "windows_gamepad.rs"]
mod windows_gamepad;
use crate::model::{
    Axis, Button, Event, InputError, InputResult, Key, KeyState, effective_virtual_key,
};
use std::collections::HashMap;
use windows_gamepad::WindowsGamepads;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;

pub struct WindowsBackend {
    always_send_scancodes: bool,
    key_rightalt_to_key_win: bool,
    held_key_plans: HashMap<u8, KeyboardPlan>,
    gamepads: WindowsGamepads,
}

impl WindowsBackend {
    pub fn new(_: String) -> Self {
        Self {
            always_send_scancodes: true,
            key_rightalt_to_key_win: false,
            held_key_plans: HashMap::new(),
            gamepads: WindowsGamepads::new(),
        }
    }

    fn keyboard_plan(&self, key: &Key) -> InputResult<KeyboardPlan> {
        match key {
            Key::WindowsScan { code, extended } => Ok(KeyboardPlan::Scan {
                code: *code,
                extended: *extended,
            }),
            Key::MoonlightVk {
                code, normalized, ..
            } => {
                let code = effective_virtual_key(*code, self.key_rightalt_to_key_win);
                if *normalized {
                    normalized_keyboard_plan(code)
                } else {
                    raw_keyboard_plan(code, self.always_send_scancodes, map_virtual_key)
                }
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
        let Event::Key { key, state } = event else {
            return Ok(KeyboardPlan::Scan {
                code: 0,
                extended: false,
            });
        };
        if let Some(identity) = Self::moonlight_key_identity(key)
            && matches!(state, KeyState::Up | KeyState::Repeat)
            && let Some(plan) = self.held_key_plans.get(&identity)
        {
            return Ok(*plan);
        }
        self.keyboard_plan(key)
    }

    fn inject_with_sender(
        &mut self,
        event: &Event,
        mut send_input: impl FnMut(&INPUT, Option<KeyboardPlan>) -> InputResult<()>,
    ) -> InputResult<()> {
        self.validate(event)?;
        if matches!(event, Event::Controller { .. }) {
            return self.gamepads.inject(event);
        }
        let key_identity = match event {
            Event::Key { key, .. } => Self::moonlight_key_identity(key),
            _ => None,
        };
        let plan = self.event_keyboard_plan(event)?;
        let input = native_input_with_plan(event, plan)?;
        if let Some(identity) = key_identity
            && matches!(
                event,
                Event::Key {
                    state: KeyState::Down,
                    ..
                }
            )
        {
            // SendInput failure can leave delivery uncertain. Retain the plan
            // before attempting the down so cleanup uses the same representation.
            self.held_key_plans.insert(identity, plan);
        }
        let held_plan_snapshot =
            key_identity.and_then(|identity| self.held_key_plans.get(&identity).copied());
        send_input(&input, held_plan_snapshot)?;
        if let (Some(identity), Event::Key { state, .. }) = (key_identity, event) {
            match state {
                KeyState::Down => {
                    self.held_key_plans.insert(identity, plan);
                }
                KeyState::Up => {
                    self.held_key_plans.remove(&identity);
                }
                KeyState::Repeat => {}
            }
        }
        Ok(())
    }
}

impl Backend for WindowsBackend {
    fn name(&self) -> &'static str {
        "windows_send_input"
    }

    fn supported(&self) -> bool {
        true
    }

    fn absolute_pointer(&self) -> bool {
        true
    }

    fn controller_input_supported(&self) -> bool {
        self.gamepads.supported()
    }

    fn effective_gamepad_profile(&self) -> Option<&'static str> {
        self.gamepads.effective_profile()
    }

    fn gamepad_unavailable_reason(&self) -> Option<&str> {
        self.gamepads.unavailable_reason()
    }

    fn key_encoding(&self) -> &'static str {
        "windows_scan"
    }

    fn validate(&self, event: &Event) -> InputResult<()> {
        event.validate()?;
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
        self.gamepads.start()?;
        self.held_key_plans.clear();
        Ok(())
    }

    fn inject(&mut self, event: &Event) -> InputResult<()> {
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

    fn advance(&mut self, now: std::time::Instant) -> InputResult<()> {
        self.gamepads.advance(now);
        Ok(())
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

    fn disable_controller_input(&mut self) -> InputResult<()> {
        self.gamepads.disable()
    }

    fn stop(&mut self) -> InputResult<()> {
        self.gamepads.stop()?;
        self.held_key_plans.clear();
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum KeyboardPlan {
    Scan { code: u16, extended: bool },
    Virtual { code: u8, extended: bool },
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
        backend.held_key_plans.insert(0x41, down_plan);
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
            backend.held_key_plans.insert(0xa5, plan);

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
}
