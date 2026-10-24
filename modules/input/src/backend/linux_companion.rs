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

pub struct LinuxCompanionBackend {
    socket_path: String,
    client: Option<LinuxInputClient>,
    held_virtual_keycodes: HashMap<u8, u16>,
    key_rightalt_to_key_win: bool,
    profile: GamepadProfile,
}

impl LinuxCompanionBackend {
    pub fn new(socket_path: String) -> Self {
        Self {
            socket_path,
            client: None,
            held_virtual_keycodes: HashMap::new(),
            key_rightalt_to_key_win: false,
            profile: GamepadProfile::Auto,
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
}

impl Backend for LinuxCompanionBackend {
    fn name(&self) -> &'static str {
        "linux_uinput_companion"
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

    fn pen_touch_max_contacts(&self, _: PenTouchDevice) -> Option<usize> {
        None
    }

    fn controller_input_supported(&self) -> bool {
        false
    }

    fn gamepad_unavailable_reason(&self) -> Option<&str> {
        Some(match self.profile {
            GamepadProfile::Auto | GamepadProfile::Xone => {
                "Linux companion currently grants keyboard and mouse input only"
            }
            _ => "selected Linux gamepad profile is unavailable through the input companion",
        })
    }

    fn supported_gamepad_profiles(&self) -> &'static [&'static str] {
        &["auto"]
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
            Event::PenTouch { .. } => Err(InputError::new(
                "pen_touch_unavailable",
                "Linux input companion does not grant pen or touch devices",
            )),
            Event::Controller { .. } => Err(InputError::new(
                "controller_unavailable",
                "Linux input companion currently grants keyboard and mouse input only",
            )),
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
            Event::Scroll { axis, steps } => {
                let units = i16::try_from(steps * 120).map_err(|_| invalid_event())?;
                self.scroll(*axis, units)
            }
            Event::ScrollUnits { axis, amount } => self.scroll(*axis, *amount),
            _ => Err(InputError::new(
                "unsupported_event",
                "unsupported Linux input event",
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
        Ok(())
    }

    fn stop(&mut self) -> InputResult<()> {
        let Some(mut client) = self.client.take() else {
            return Ok(());
        };
        self.held_virtual_keycodes.clear();
        client.close().map_err(companion_error)
    }
}

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
        Axis, InputEvent, KeyState, absolute_pointer_event, evdev_key_event, scroll_events,
    };

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
}
