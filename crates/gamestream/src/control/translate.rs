use super::{ControllerState, Input};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KeyState {
    Down,
    Up,
    Repeat,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
    Back,
    Forward,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScrollAxis {
    Horizontal,
    Vertical,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InputEvent {
    Key {
        virtual_key: u8,
        normalized: bool,
        repeat_modifiers: u8,
        temporary_modifier: bool,
        state: KeyState,
    },
    Button {
        button: MouseButton,
        pressed: bool,
    },
    Relative {
        dx: i32,
        dy: i32,
    },
    Absolute {
        x: u16,
        y: u16,
    },
    Scroll {
        axis: ScrollAxis,
        steps: i32,
    },
    ScrollUnits {
        axis: ScrollAxis,
        amount: i16,
    },
    Controller(ControllerState),
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct InputBatch {
    events: Vec<InputEvent>,
}

impl InputBatch {
    pub fn events(&self) -> &[InputEvent] {
        &self.events
    }

    pub fn into_events(self) -> Vec<InputEvent> {
        self.events
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TranslationError {
    UnsupportedInput,
    UnsupportedKey,
    UnsupportedKeyboardEncoding,
    UnsupportedModifiers,
    InvalidPointer,
    InvalidController,
}

/// An authoritative target rectangle in Windows virtual-desktop normalized
/// coordinates. The stream mode alone is not sufficient to construct this.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AbsoluteViewport {
    left: u16,
    top: u16,
    right: u16,
    bottom: u16,
}

impl AbsoluteViewport {
    pub fn new(left: u16, top: u16, right: u16, bottom: u16) -> Result<Self, TranslationError> {
        if left >= right || top >= bottom {
            return Err(TranslationError::InvalidPointer);
        }
        Ok(Self {
            left,
            top,
            right,
            bottom,
        })
    }
}

pub struct InputTranslator {
    held_keys: [Option<HeldKey>; 256],
    held_buttons: [bool; 5],
    absolute_viewport: Option<AbsoluteViewport>,
}

#[derive(Clone, Copy)]
struct HeldKey {
    virtual_key: u8,
    normalized: bool,
}

impl InputTranslator {
    pub fn new(absolute_viewport: Option<AbsoluteViewport>) -> Self {
        Self {
            held_keys: [None; 256],
            held_buttons: [false; 5],
            absolute_viewport,
        }
    }

    /// Translate one authenticated control input record into one bounded input
    /// service batch. Empty batches are deliberate for duplicate releases and
    /// zero-delta scroll records.
    pub fn translate(&mut self, input: Input<'_>) -> Result<InputBatch, TranslationError> {
        let events = match input {
            Input::Keyboard {
                pressed,
                key_code,
                modifiers,
                flags,
            } => self.keyboard(pressed, key_code, modifiers, flags)?,
            Input::RelativeMouse { x, y } => {
                if x == i16::MIN || y == i16::MIN {
                    return Err(TranslationError::InvalidPointer);
                }
                vec![InputEvent::Relative {
                    dx: i32::from(x),
                    dy: i32::from(y),
                }]
            }
            Input::AbsoluteMouse {
                x,
                y,
                reference_width,
                reference_height,
            } => vec![self.absolute(x, y, reference_width, reference_height)?],
            Input::MouseButton { pressed, button } => self.button(pressed, button)?,
            Input::VerticalScroll { amount } => self.scroll(ScrollAxis::Vertical, amount),
            Input::HorizontalScroll { amount } => self.scroll(ScrollAxis::Horizontal, amount),
            Input::Controller(state) if state.number < 16 => {
                vec![InputEvent::Controller(state)]
            }
            Input::Controller(_) => return Err(TranslationError::InvalidController),
            Input::Text(_)
            | Input::ControllerArrival(_)
            | Input::ControllerTouch(_)
            | Input::ControllerMotion(_)
            | Input::ControllerBattery(_)
            | Input::EnableHaptics { .. }
            | Input::PenTouch(_)
            | Input::Unsupported { .. } => {
                return Err(TranslationError::UnsupportedInput);
            }
        };
        debug_assert!(events.len() <= 7);
        Ok(InputBatch { events })
    }

    fn keyboard(
        &mut self,
        pressed: bool,
        key_code: u16,
        modifiers: u8,
        flags: u8,
    ) -> Result<Vec<InputEvent>, TranslationError> {
        if flags & !1 != 0 {
            return Err(TranslationError::UnsupportedKeyboardEncoding);
        }
        let normalized = flags & 1 == 0;
        if normalized && key_code & 0xff00 != 0x8000 {
            return Err(TranslationError::UnsupportedKeyboardEncoding);
        }
        let incoming_key = key_code as u8;
        // Preserve the original byte until the input consumer applies the
        // selected keybindings exactly once. Backend validation checks the
        // resolved destination; a remapped source need not have a native key.
        let key_index = incoming_key;
        let held = self.held_keys[usize::from(key_index)];
        if !pressed {
            let Some(held) = held else {
                return Ok(Vec::new());
            };
            self.held_keys[usize::from(key_index)] = None;
            return Ok(vec![InputEvent::Key {
                virtual_key: held.virtual_key,
                normalized: held.normalized,
                repeat_modifiers: 0,
                temporary_modifier: false,
                state: KeyState::Up,
            }]);
        }

        // Vibeshine ignores duplicate client downs and owns the repeat timer
        // per client. The input service now schedules that repeat by lease.
        if held.is_some() {
            return Ok(Vec::new());
        }
        let held_key = held.unwrap_or(HeldKey {
            virtual_key: incoming_key,
            normalized,
        });
        let mut synthetic = Vec::new();
        if !is_modifier(held_key.virtual_key) {
            for (mask, key) in [(1, 0x10), (2, 0x11), (4, 0x12)] {
                if modifiers & mask != 0 && !self.modifier_held(mask) {
                    synthetic.push(key);
                }
            }
            if modifiers & 8 != 0 && !self.modifier_held(8) {
                return Err(TranslationError::UnsupportedModifiers);
            }
        }
        let mut events = Vec::with_capacity(synthetic.len() * 2 + 1);
        events.extend(synthetic.iter().map(|key| InputEvent::Key {
            virtual_key: *key,
            normalized,
            repeat_modifiers: 0,
            temporary_modifier: true,
            state: KeyState::Down,
        }));
        let repeat_modifiers = synthetic.iter().fold(0, |mask, key| {
            mask | match key {
                0x10 => 1,
                0x11 => 2,
                0x12 => 4,
                _ => 0,
            }
        });
        events.push(InputEvent::Key {
            virtual_key: held_key.virtual_key,
            normalized: held_key.normalized,
            repeat_modifiers,
            temporary_modifier: false,
            state: KeyState::Down,
        });
        events.extend(synthetic.iter().rev().map(|key| InputEvent::Key {
            virtual_key: *key,
            normalized,
            repeat_modifiers: 0,
            temporary_modifier: true,
            state: KeyState::Up,
        }));
        self.held_keys[usize::from(key_index)] = Some(held_key);
        Ok(events)
    }

    fn modifier_held(&self, modifier: u8) -> bool {
        let keys: &[u8] = match modifier {
            1 => &[0x10, 0xa0, 0xa1],
            2 => &[0x11, 0xa2, 0xa3],
            4 => &[0x12, 0xa4, 0xa5],
            8 => &[0x5b, 0x5c],
            _ => return false,
        };
        keys.iter()
            .any(|key| self.held_keys[usize::from(*key)].is_some())
    }

    fn button(
        &mut self,
        pressed: bool,
        wire_button: u8,
    ) -> Result<Vec<InputEvent>, TranslationError> {
        let (index, button) = match wire_button {
            1 => (0, MouseButton::Left),
            2 => (1, MouseButton::Middle),
            3 => (2, MouseButton::Right),
            4 => (3, MouseButton::Back),
            5 => (4, MouseButton::Forward),
            _ => return Err(TranslationError::UnsupportedInput),
        };
        if self.held_buttons[index] == pressed {
            return Ok(Vec::new());
        }
        self.held_buttons[index] = pressed;
        Ok(vec![InputEvent::Button { button, pressed }])
    }

    fn absolute(
        &self,
        x: u16,
        y: u16,
        reference_width: u32,
        reference_height: u32,
    ) -> Result<InputEvent, TranslationError> {
        let viewport = self
            .absolute_viewport
            .ok_or(TranslationError::UnsupportedInput)?;
        if reference_width < 2
            || reference_height < 2
            || u32::from(x) >= reference_width
            || u32::from(y) >= reference_height
        {
            return Err(TranslationError::InvalidPointer);
        }
        Ok(InputEvent::Absolute {
            x: scale_coordinate(x, reference_width, viewport.left, viewport.right),
            y: scale_coordinate(y, reference_height, viewport.top, viewport.bottom),
        })
    }

    fn scroll(&mut self, axis: ScrollAxis, amount: i16) -> Vec<InputEvent> {
        if amount == 0 {
            return Vec::new();
        }
        vec![InputEvent::ScrollUnits { axis, amount }]
    }
}

impl Default for InputTranslator {
    fn default() -> Self {
        Self::new(None)
    }
}

fn scale_coordinate(value: u16, reference_size: u32, start: u16, end: u16) -> u16 {
    let denominator = u64::from(reference_size - 1);
    let span = u64::from(end - start);
    let scaled = (u64::from(value) * span + denominator / 2) / denominator;
    start + scaled as u16
}

fn is_modifier(key: u8) -> bool {
    matches!(
        key,
        0x10 | 0x11 | 0x12 | 0x5b | 0x5c | 0xa0 | 0xa1 | 0xa2 | 0xa3 | 0xa4 | 0xa5
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VirtualKeyMapping {
    pub windows_scan_code: u16,
    pub windows_extended: bool,
    pub linux_evdev_code: u16,
}

const fn key(
    windows_scan_code: u16,
    windows_extended: bool,
    linux_evdev_code: u16,
) -> VirtualKeyMapping {
    VirtualKeyMapping {
        windows_scan_code,
        windows_extended,
        linux_evdev_code,
    }
}

/// Return the fixed US physical-key mapping for normalized Moonlight virtual
/// keys. Keys without an unambiguous Set-1 and evdev representation are absent.
pub fn virtual_key_mapping(code: u8) -> Option<VirtualKeyMapping> {
    Some(match code {
        0x08 => key(0x0e, false, 14),
        0x09 => key(0x0f, false, 15),
        0x0d => key(0x1c, false, 28),
        0x10 | 0xa0 => key(0x2a, false, 42),
        0xa1 => key(0x36, false, 54),
        0x11 | 0xa2 => key(0x1d, false, 29),
        0xa3 => key(0x1d, true, 97),
        0x12 | 0xa4 => key(0x38, false, 56),
        0xa5 => key(0x38, true, 100),
        0x14 => key(0x3a, false, 58),
        0x1b => key(0x01, false, 1),
        0x20 => key(0x39, false, 57),
        0x21 => key(0x49, true, 104),
        0x22 => key(0x51, true, 109),
        0x23 => key(0x4f, true, 107),
        0x24 => key(0x47, true, 102),
        0x25 => key(0x4b, true, 105),
        0x26 => key(0x48, true, 103),
        0x27 => key(0x4d, true, 106),
        0x28 => key(0x50, true, 108),
        0x2c => key(0x37, true, 99),
        0x2d => key(0x52, true, 110),
        0x2e => key(0x53, true, 111),
        0x30 => key(0x0b, false, 11),
        0x31..=0x39 => key(
            u16::from(code - 0x31 + 0x02),
            false,
            u16::from(code - 0x31 + 2),
        ),
        0x41 => key(0x1e, false, 30),
        0x42 => key(0x30, false, 48),
        0x43 => key(0x2e, false, 46),
        0x44 => key(0x20, false, 32),
        0x45 => key(0x12, false, 18),
        0x46 => key(0x21, false, 33),
        0x47 => key(0x22, false, 34),
        0x48 => key(0x23, false, 35),
        0x49 => key(0x17, false, 23),
        0x4a => key(0x24, false, 36),
        0x4b => key(0x25, false, 37),
        0x4c => key(0x26, false, 38),
        0x4d => key(0x32, false, 50),
        0x4e => key(0x31, false, 49),
        0x4f => key(0x18, false, 24),
        0x50 => key(0x19, false, 25),
        0x51 => key(0x10, false, 16),
        0x52 => key(0x13, false, 19),
        0x53 => key(0x1f, false, 31),
        0x54 => key(0x14, false, 20),
        0x55 => key(0x16, false, 22),
        0x56 => key(0x2f, false, 47),
        0x57 => key(0x11, false, 17),
        0x58 => key(0x2d, false, 45),
        0x59 => key(0x15, false, 21),
        0x5a => key(0x2c, false, 44),
        0x5b => key(0x5b, true, 125),
        0x5c => key(0x5c, true, 126),
        0x5d => key(0x5d, true, 127),
        0x60 => key(0x52, false, 82),
        0x61 => key(0x4f, false, 79),
        0x62 => key(0x50, false, 80),
        0x63 => key(0x51, false, 81),
        0x64 => key(0x4b, false, 75),
        0x65 => key(0x4c, false, 76),
        0x66 => key(0x4d, false, 77),
        0x67 => key(0x47, false, 71),
        0x68 => key(0x48, false, 72),
        0x69 => key(0x49, false, 73),
        0x6a => key(0x37, false, 55),
        0x6b => key(0x4e, false, 78),
        0x6d => key(0x4a, false, 74),
        0x6e => key(0x53, false, 83),
        0x6f => key(0x35, true, 98),
        0x70..=0x79 => key(
            u16::from(code - 0x70 + 0x3b),
            false,
            u16::from(code - 0x70 + 59),
        ),
        0x7a => key(0x57, false, 87),
        0x7b => key(0x58, false, 88),
        0x90 => key(0x45, false, 69),
        0x91 => key(0x46, false, 70),
        0xba => key(0x27, false, 39),
        0xbb => key(0x0d, false, 13),
        0xbc => key(0x33, false, 51),
        0xbd => key(0x0c, false, 12),
        0xbe => key(0x34, false, 52),
        0xbf => key(0x35, false, 53),
        0xc0 => key(0x29, false, 41),
        0xdb => key(0x1a, false, 26),
        0xdc => key(0x2b, false, 43),
        0xdd => key(0x1b, false, 27),
        0xde => key(0x28, false, 40),
        0xe2 => key(0x56, false, 86),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyboard_tracks_held_keys_and_synthesizes_only_missing_modifiers() {
        let mut translator = InputTranslator::default();
        let batch = translator
            .translate(Input::Keyboard {
                pressed: true,
                key_code: 0x8041,
                modifiers: 3,
                flags: 0,
            })
            .unwrap();
        assert_eq!(
            batch.events(),
            &[
                InputEvent::Key {
                    virtual_key: 0x10,
                    normalized: true,
                    repeat_modifiers: 0,
                    temporary_modifier: true,
                    state: KeyState::Down
                },
                InputEvent::Key {
                    virtual_key: 0x11,
                    normalized: true,
                    repeat_modifiers: 0,
                    temporary_modifier: true,
                    state: KeyState::Down
                },
                InputEvent::Key {
                    virtual_key: 0x41,
                    normalized: true,
                    repeat_modifiers: 3,
                    temporary_modifier: false,
                    state: KeyState::Down
                },
                InputEvent::Key {
                    virtual_key: 0x11,
                    normalized: true,
                    repeat_modifiers: 0,
                    temporary_modifier: true,
                    state: KeyState::Up
                },
                InputEvent::Key {
                    virtual_key: 0x10,
                    normalized: true,
                    repeat_modifiers: 0,
                    temporary_modifier: true,
                    state: KeyState::Up
                },
            ]
        );
        assert!(
            translator
                .translate(Input::Keyboard {
                    pressed: true,
                    key_code: 0x8041,
                    modifiers: 0,
                    flags: 0,
                })
                .unwrap()
                .events()
                .is_empty()
        );
    }

    #[test]
    fn generic_and_left_modifier_packets_retain_distinct_source_ownership() {
        let mut translator = InputTranslator::default();
        let down = translator
            .translate(Input::Keyboard {
                pressed: true,
                key_code: 0x8010,
                modifiers: 1,
                flags: 0,
            })
            .unwrap();
        assert_eq!(down.events().len(), 1);
        assert_eq!(
            down.events()[0],
            InputEvent::Key {
                virtual_key: 0x10,
                normalized: true,
                repeat_modifiers: 0,
                temporary_modifier: false,
                state: KeyState::Down,
            }
        );
        assert_eq!(
            translator
                .translate(Input::Keyboard {
                    pressed: true,
                    key_code: 0x80a0,
                    modifiers: 0,
                    flags: 0,
                })
                .unwrap()
                .events(),
            &[InputEvent::Key {
                virtual_key: 0xa0,
                normalized: true,
                repeat_modifiers: 0,
                temporary_modifier: false,
                state: KeyState::Down,
            }]
        );
        assert_eq!(
            translator
                .translate(Input::Keyboard {
                    pressed: false,
                    key_code: 0x8010,
                    modifiers: 0,
                    flags: 0,
                })
                .unwrap()
                .events(),
            &[InputEvent::Key {
                virtual_key: 0x10,
                normalized: true,
                repeat_modifiers: 0,
                temporary_modifier: false,
                state: KeyState::Up,
            }]
        );
        assert_eq!(
            translator
                .translate(Input::Keyboard {
                    pressed: false,
                    key_code: 0x80a0,
                    modifiers: 0,
                    flags: 0
                })
                .unwrap()
                .events(),
            &[InputEvent::Key {
                virtual_key: 0xa0,
                normalized: true,
                repeat_modifiers: 0,
                temporary_modifier: false,
                state: KeyState::Up
            }]
        );
    }

    #[test]
    fn mouse_button_order_and_raw_scroll_passthrough_are_explicit() {
        let mut translator = InputTranslator::default();
        assert_eq!(
            translator
                .translate(Input::MouseButton {
                    pressed: true,
                    button: 2
                })
                .unwrap()
                .events(),
            &[InputEvent::Button {
                button: MouseButton::Middle,
                pressed: true
            }]
        );
        assert_eq!(
            translator
                .translate(Input::VerticalScroll { amount: 60 })
                .unwrap()
                .events(),
            &[InputEvent::ScrollUnits {
                axis: ScrollAxis::Vertical,
                amount: 60
            }]
        );
        assert_eq!(
            translator
                .translate(Input::HorizontalScroll { amount: -120 })
                .unwrap()
                .events(),
            &[InputEvent::ScrollUnits {
                axis: ScrollAxis::Horizontal,
                amount: -120
            }]
        );
        assert!(
            translator
                .translate(Input::VerticalScroll { amount: 0 })
                .unwrap()
                .events()
                .is_empty()
        );
        assert_eq!(
            translator.translate(Input::RelativeMouse { x: i16::MIN, y: 0 }),
            Err(TranslationError::InvalidPointer)
        );
    }

    #[test]
    fn absolute_requires_authoritative_viewport_and_checks_reference_geometry() {
        let input = Input::AbsoluteMouse {
            x: 999,
            y: 499,
            reference_width: 1000,
            reference_height: 500,
        };
        assert_eq!(
            InputTranslator::default().translate(input),
            Err(TranslationError::UnsupportedInput)
        );
        let viewport = AbsoluteViewport::new(1000, 2000, 5000, 6000).unwrap();
        let mut translator = InputTranslator::new(Some(viewport));
        assert_eq!(
            translator
                .translate(Input::AbsoluteMouse {
                    x: 500,
                    y: 250,
                    reference_width: 1000,
                    reference_height: 500,
                })
                .unwrap()
                .events(),
            &[InputEvent::Absolute { x: 3002, y: 4004 }]
        );
        assert_eq!(
            translator.translate(Input::AbsoluteMouse {
                x: 0,
                y: 0,
                reference_width: 1,
                reference_height: 500,
            }),
            Err(TranslationError::InvalidPointer)
        );
    }

    #[test]
    fn ambiguous_keyboard_encodings_and_unsupported_input_never_map_to_events() {
        let mut translator = InputTranslator::default();
        for (key_code, flags) in [(0x0041, 0), (0x8041, 2), (0x8041, 3)] {
            assert!(
                translator
                    .translate(Input::Keyboard {
                        pressed: true,
                        key_code,
                        modifiers: 0,
                        flags,
                    })
                    .is_err()
            );
        }
        // Native support depends on the resolved destination in the lease's
        // keybindings. Even reserved source bytes may map to a supported key.
        for byte in [0_u8, 255] {
            let translated = translator
                .translate(Input::Keyboard {
                    pressed: true,
                    key_code: 0x8000 | u16::from(byte),
                    modifiers: 0,
                    flags: 0,
                })
                .unwrap();
            assert_eq!(
                translated.events(),
                &[InputEvent::Key {
                    virtual_key: byte,
                    normalized: true,
                    repeat_modifiers: 0,
                    temporary_modifier: false,
                    state: KeyState::Down,
                }]
            );
        }
        assert_eq!(
            translator.translate(Input::Text("a")),
            Err(TranslationError::UnsupportedInput)
        );
    }

    #[test]
    fn non_normalized_packets_are_supported_and_keep_the_keydown_mode_for_repeat_and_up() {
        let mut translator = InputTranslator::default();
        let down = translator
            .translate(Input::Keyboard {
                pressed: true,
                key_code: 0x0041,
                modifiers: 0,
                flags: 1,
            })
            .unwrap();
        assert_eq!(
            down.events(),
            &[InputEvent::Key {
                virtual_key: 0x41,
                normalized: false,
                repeat_modifiers: 0,
                temporary_modifier: false,
                state: KeyState::Down,
            }]
        );

        let repeat_after_mode_change = translator
            .translate(Input::Keyboard {
                pressed: true,
                key_code: 0x8041,
                modifiers: 0,
                flags: 0,
            })
            .unwrap();
        assert!(repeat_after_mode_change.events().is_empty());

        let up_after_mode_change = translator
            .translate(Input::Keyboard {
                pressed: false,
                key_code: 0x8041,
                modifiers: 0,
                flags: 0,
            })
            .unwrap();
        assert_eq!(
            up_after_mode_change.events(),
            &[InputEvent::Key {
                virtual_key: 0x41,
                normalized: false,
                repeat_modifiers: 0,
                temporary_modifier: false,
                state: KeyState::Up,
            }]
        );
    }

    #[test]
    fn virtual_key_table_preserves_known_extended_and_physical_keys() {
        assert_eq!(virtual_key_mapping(0x41), Some(key(0x1e, false, 30)));
        assert_eq!(virtual_key_mapping(0xa3), Some(key(0x1d, true, 97)));
        assert_eq!(virtual_key_mapping(0x25), Some(key(0x4b, true, 105)));
        assert_eq!(virtual_key_mapping(0xff), None);
    }

    #[test]
    fn controller_translation_preserves_full_state_and_rejects_invalid_slot() {
        let state = ControllerState {
            number: 15,
            active_mask: 0xa55a,
            buttons: 0x89abcdef,
            left_trigger: 0,
            right_trigger: 255,
            left_x: i16::MIN,
            left_y: i16::MAX,
            right_x: -1,
            right_y: 1,
        };
        let mut translator = InputTranslator::default();

        assert_eq!(
            translator
                .translate(Input::Controller(state))
                .unwrap()
                .events(),
            &[InputEvent::Controller(state)]
        );
        assert_eq!(
            translator.translate(Input::Controller(ControllerState {
                number: 16,
                ..state
            })),
            Err(TranslationError::InvalidController)
        );
    }
}
