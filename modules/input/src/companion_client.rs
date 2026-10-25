//! Translation from the input module's events to the separately installed VHF
//! companion protocol. Unsupported events fail closed; this adapter never
//! falls back to the Windows SendInput path.

use crate::model::{Axis, Button, Event, InputError, InputResult, Key, KeyState};
use amaterasu_input_companion::{InputEvent, MouseButton};

pub use crate::model::Event as UserInputEvent;

/// Translate one input-module event into the companion's narrower keyboard
/// and mouse protocol. `raw_virtual_key` must return the Windows scan code and
/// extended-key bit for an unnormalized virtual key, or `None` when Windows
/// cannot map that key to a supported scan code.
pub fn translate(
    event: &UserInputEvent,
    key_rightalt_to_key_win: bool,
    mut raw_virtual_key: impl FnMut(u8) -> Option<(u16, bool)>,
) -> InputResult<Option<InputEvent>> {
    event.validate()?;
    match event {
        Event::Key { key, state } => {
            let (scan_code, extended) = match key {
                Key::WindowsScan { code, extended } => (checked_scan_code(*code)?, *extended),
                Key::MoonlightVk {
                    code, normalized, ..
                } => {
                    let code = crate::model::effective_virtual_key(*code, key_rightalt_to_key_win);
                    if *normalized {
                        let mapping = amaterasu_gamestream::control::virtual_key_mapping(code)
                            .ok_or_else(unsupported_key)?;
                        (
                            checked_scan_code(mapping.windows_scan_code)?,
                            mapping.windows_extended,
                        )
                    } else {
                        let (scan_code, extended) =
                            raw_virtual_key(code).ok_or_else(unsupported_key)?;
                        (checked_scan_code(scan_code)?, extended)
                    }
                }
                Key::LinuxEvdev { .. } => return Err(unsupported_key()),
            };
            if *state == KeyState::Repeat {
                // The VHF keyboard report owns typematic behavior while the
                // key remains down. Validate the mapping, but do not submit a
                // duplicate key-down report for an application repeat.
                return Ok(None);
            }
            Ok(Some(InputEvent::Key {
                scan_code,
                extended,
                pressed: *state == KeyState::Down,
            }))
        }
        Event::Button { button, pressed } => Ok(Some(InputEvent::MouseButton {
            button: match button {
                Button::Left => MouseButton::Left,
                Button::Right => MouseButton::Right,
                Button::Middle => MouseButton::Middle,
                Button::Back => MouseButton::Back,
                Button::Forward => MouseButton::Forward,
            },
            pressed: *pressed,
        })),
        Event::Relative { dx, dy } => Ok(Some(InputEvent::Relative {
            dx: i16::try_from(*dx).expect("validated relative delta fits i16"),
            dy: i16::try_from(*dy).expect("validated relative delta fits i16"),
        })),
        Event::Scroll { axis, steps } => Ok(Some(InputEvent::Scroll {
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
        })),
        _ => Err(InputError::new(
            "unsupported_event",
            "VHF keyboard/mouse contract does not support this event yet",
        )),
    }
}

fn checked_scan_code(code: u16) -> InputResult<u8> {
    let code = u8::try_from(code).map_err(|_| unsupported_key())?;
    if !(1..=0x7f).contains(&code) {
        return Err(unsupported_key());
    }
    Ok(code)
}

fn unsupported_key() -> InputError {
    InputError::new(
        "unsupported_event",
        "VHF companion cannot represent this key as a keyboard scan code",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translates_qualified_windows_keyboard_and_mouse_intent() {
        assert_eq!(
            translate(
                &Event::Key {
                    key: Key::WindowsScan {
                        code: 30,
                        extended: false
                    },
                    state: KeyState::Down,
                },
                false,
                |_| None
            )
            .unwrap(),
            Some(InputEvent::Key {
                scan_code: 30,
                extended: false,
                pressed: true
            })
        );
        assert_eq!(
            translate(
                &Event::Scroll {
                    axis: Axis::Horizontal,
                    steps: -2
                },
                false,
                |_| None
            )
            .unwrap(),
            Some(InputEvent::Scroll {
                vertical: 0,
                horizontal: -2
            })
        );
    }

    #[test]
    fn stream_shaped_moonlight_keys_translate_to_companion_scan_reports() {
        // These are the key payloads emitted by stream-session's event_payload
        // for normalized Moonlight keys; serde exercises the real Event shape.
        let down: UserInputEvent = serde_json::from_value(serde_json::json!({
            "kind":"key",
            "key":{"platform":"moonlight_vk","code":65},
            "state":"down"
        }))
        .unwrap();
        let up: UserInputEvent = serde_json::from_value(serde_json::json!({
            "kind":"key",
            "key":{"platform":"moonlight_vk","code":65},
            "state":"up"
        }))
        .unwrap();
        assert_eq!(
            translate(&down, false, |_| panic!(
                "normalized key uses fixed mapping"
            ))
            .unwrap(),
            Some(InputEvent::Key {
                scan_code: 0x1e,
                extended: false,
                pressed: true,
            })
        );
        assert_eq!(
            translate(&up, false, |_| panic!("normalized key uses fixed mapping")).unwrap(),
            Some(InputEvent::Key {
                scan_code: 0x1e,
                extended: false,
                pressed: false,
            })
        );

        // Normalized navigation keys retain their extended-key bit as well.
        let extended: UserInputEvent = serde_json::from_value(serde_json::json!({
            "kind":"key",
            "key":{"platform":"moonlight_vk","code":38},
            "state":"down"
        }))
        .unwrap();
        assert_eq!(
            translate(&extended, false, |_| panic!(
                "normalized key uses fixed mapping"
            ))
            .unwrap(),
            Some(InputEvent::Key {
                scan_code: 0x48,
                extended: true,
                pressed: true,
            })
        );
    }

    #[test]
    fn stream_shaped_unnormalized_key_preserves_extended_release_mapping() {
        let down: UserInputEvent = serde_json::from_value(serde_json::json!({
            "kind":"key",
            "key":{"platform":"moonlight_vk","code":37,"normalized":false},
            "state":"down"
        }))
        .unwrap();
        let up: UserInputEvent = serde_json::from_value(serde_json::json!({
            "kind":"key",
            "key":{"platform":"moonlight_vk","code":37,"normalized":false},
            "state":"up"
        }))
        .unwrap();
        let mapper = |code| (code == 0x25).then_some((0x4b, true));
        assert_eq!(
            translate(&down, false, mapper).unwrap(),
            Some(InputEvent::Key {
                scan_code: 0x4b,
                extended: true,
                pressed: true,
            })
        );
        assert_eq!(
            translate(&up, false, mapper).unwrap(),
            Some(InputEvent::Key {
                scan_code: 0x4b,
                extended: true,
                pressed: false,
            })
        );
    }

    #[test]
    fn key_repeat_is_validated_without_a_second_down_report() {
        let repeat: UserInputEvent = serde_json::from_value(serde_json::json!({
            "kind":"key",
            "key":{"platform":"moonlight_vk","code":65},
            "state":"repeat"
        }))
        .unwrap();
        assert_eq!(
            translate(&repeat, false, |_| panic!(
                "normalized key uses fixed mapping"
            ))
            .unwrap(),
            None
        );
    }

    #[test]
    fn unsupported_or_unmappable_keys_fail_closed() {
        assert_eq!(
            translate(
                &Event::Key {
                    key: Key::MoonlightVk {
                        code: 0x07,
                        normalized: true,
                        repeat_modifiers: 0,
                        temporary_modifier: false
                    },
                    state: KeyState::Down,
                },
                false,
                |_| None
            )
            .unwrap_err()
            .code,
            "unsupported_event"
        );
        assert_eq!(
            translate(
                &Event::Key {
                    key: Key::MoonlightVk {
                        code: 0x41,
                        normalized: false,
                        repeat_modifiers: 0,
                        temporary_modifier: false
                    },
                    state: KeyState::Down,
                },
                false,
                |_| None
            )
            .unwrap_err()
            .code,
            "unsupported_event"
        );
        assert_eq!(
            translate(&Event::Absolute { x: 5, y: 5 }, false, |_| None)
                .unwrap_err()
                .code,
            "unsupported_event"
        );
    }
}
