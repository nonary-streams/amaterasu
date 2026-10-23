//! Deliberately narrow translation for a future VHF companion connection.
//!
//! The current Windows backend does not use this adapter. Unsupported events
//! fail closed until HID usages and driver reports are qualified; it never
//! falls back to the existing SendInput path.

use crate::model::{Axis, Button, Event, InputError, InputResult, Key, KeyState};
use amaterasu_input_companion::{InputEvent, MouseButton};

pub use crate::model::Event as UserInputEvent;

pub fn translate(event: &UserInputEvent) -> InputResult<InputEvent> {
    event.validate()?;
    match event {
        Event::Key {
            key: Key::WindowsScan { code, extended },
            state: KeyState::Down | KeyState::Up,
        } => Ok(InputEvent::Key {
            scan_code: u8::try_from(*code).expect("validated Windows scan code fits u8"),
            extended: *extended,
            pressed: matches!(
                event,
                Event::Key {
                    state: KeyState::Down,
                    ..
                }
            ),
        }),
        Event::Button { button, pressed } => Ok(InputEvent::MouseButton {
            button: match button {
                Button::Left => MouseButton::Left,
                Button::Right => MouseButton::Right,
                Button::Middle => MouseButton::Middle,
                Button::Back => MouseButton::Back,
                Button::Forward => MouseButton::Forward,
            },
            pressed: *pressed,
        }),
        Event::Relative { dx, dy } => Ok(InputEvent::Relative {
            dx: i16::try_from(*dx).expect("validated relative delta fits i16"),
            dy: i16::try_from(*dy).expect("validated relative delta fits i16"),
        }),
        Event::Scroll { axis, steps } => Ok(InputEvent::Scroll {
            vertical: if *axis == Axis::Vertical {
                i16::try_from(*steps).expect("validated scroll delta fits i16")
            } else {
                0
            },
            horizontal: if *axis == Axis::Horizontal {
                i16::try_from(*steps).expect("validated scroll delta fits i16")
            } else {
                0
            },
        }),
        _ => Err(InputError::new(
            "unsupported_event",
            "VHF keyboard/mouse contract does not support this event yet",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translates_qualified_windows_keyboard_and_mouse_intent() {
        assert_eq!(
            translate(&Event::Key {
                key: Key::WindowsScan {
                    code: 30,
                    extended: false
                },
                state: KeyState::Down,
            })
            .unwrap(),
            InputEvent::Key {
                scan_code: 30,
                extended: false,
                pressed: true
            }
        );
        assert_eq!(
            translate(&Event::Scroll {
                axis: Axis::Horizontal,
                steps: -2
            })
            .unwrap(),
            InputEvent::Scroll {
                vertical: 0,
                horizontal: -2
            }
        );
    }

    #[test]
    fn unsupported_input_cannot_silently_use_the_old_backend() {
        assert_eq!(
            translate(&Event::Key {
                key: Key::MoonlightVk {
                    code: 0x41,
                    normalized: true,
                    repeat_modifiers: 0,
                    temporary_modifier: false
                },
                state: KeyState::Down,
            })
            .unwrap_err()
            .code,
            "unsupported_event"
        );
        assert_eq!(
            translate(&Event::Absolute { x: 5, y: 5 }).unwrap_err().code,
            "unsupported_event"
        );
    }
}
