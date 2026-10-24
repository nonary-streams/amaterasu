use crate::{InputError, InputResult};
use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u16 = 1;

/// Supplied by a trusted OS transport adapter. It is never read from a packet.
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PeerIdentity {
    principal: String,
    interactive_session: u32,
    component_set_digest: String,
}

impl PeerIdentity {
    pub(crate) fn valid(&self) -> bool {
        !self.principal.is_empty()
            && self.principal.len() <= 256
            && self.interactive_session != u32::MAX
            && self.component_set_digest.len() == 64
            && self
                .component_set_digest
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    }

    /// The caller must derive these values from the OS connection and the
    /// verified active component set, not from worker-provided JSON.
    pub fn from_verified_connection(
        principal: String,
        interactive_session: u32,
        component_set_digest: String,
    ) -> InputResult<Self> {
        let identity = Self {
            principal,
            interactive_session,
            component_set_digest,
        };
        if !identity.valid() {
            return Err(InputError::InvalidPeer);
        }
        Ok(identity)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    Back,
    Forward,
}

/// A bounded local rumble notification emitted by a granted virtual gamepad.
/// This is a companion API only; it is not a Moonlight wire message.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GamepadFeedback {
    pub slot: u16,
    pub strong_motor: u16,
    pub weak_motor: u16,
    pub duration_ms: u16,
}

/// Normalized intent, never a raw HID report or arbitrary device descriptor.
/// The companion and driver must independently map and validate reports.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum InputEvent {
    Key {
        scan_code: u8,
        extended: bool,
        pressed: bool,
    },
    EvdevKey {
        code: u16,
        pressed: bool,
    },
    /// Emits an evdev repeat value for a key already held by this session.
    /// It never changes the companion's held-input ledger.
    EvdevKeyRepeat {
        code: u16,
    },
    /// Normalized desktop pointer coordinates in the inclusive 0..=65535 range.
    /// This is stateless and does not add an entry to the held-input ledger.
    Absolute {
        x: u16,
        y: u16,
    },
    MouseButton {
        button: MouseButton,
        pressed: bool,
    },
    Relative {
        dx: i16,
        dy: i16,
    },
    Scroll {
        vertical: i16,
        horizontal: i16,
    },
    /// Complete normalized state for one controller slot. The companion maps
    /// only the documented button bits and axes to its session-owned device.
    Gamepad {
        number: u16,
        active_mask: u16,
        buttons: u32,
        left_trigger: u8,
        right_trigger: u8,
        left_x: i16,
        left_y: i16,
        right_x: i16,
        right_y: i16,
    },
}

impl InputEvent {
    pub(crate) fn validate(&self) -> InputResult<()> {
        match self {
            Self::Key { scan_code, .. } if !(1..=127).contains(scan_code) => {
                Err(InputError::InvalidEvent)
            }
            Self::EvdevKey { code, .. } if !(1..=248).contains(code) || *code == 240 => {
                Err(InputError::InvalidEvent)
            }
            Self::EvdevKeyRepeat { code } if !(1..=248).contains(code) || *code == 240 => {
                Err(InputError::InvalidEvent)
            }
            Self::Relative { dx: 0, dy: 0 }
            | Self::Scroll {
                vertical: 0,
                horizontal: 0,
            } => Err(InputError::InvalidEvent),
            Self::Scroll {
                vertical,
                horizontal,
            } if vertical.abs_diff(0) > 12_000 || horizontal.abs_diff(0) > 12_000 => {
                Err(InputError::InvalidEvent)
            }
            Self::Gamepad {
                number, buttons, ..
            } if *number >= 16 || *buttons & !GAMEPAD_BUTTON_MASK != 0 => {
                Err(InputError::InvalidEvent)
            }
            _ => Ok(()),
        }
    }

    pub(crate) fn keyboard(&self) -> bool {
        matches!(
            self,
            Self::Key { .. } | Self::EvdevKey { .. } | Self::EvdevKeyRepeat { .. }
        )
    }

    pub(crate) fn gamepad(&self) -> bool {
        matches!(self, Self::Gamepad { .. })
    }

    pub(crate) fn transition(&self) -> Option<(HeldInput, bool)> {
        match self {
            Self::Key {
                scan_code,
                extended,
                pressed,
            } => Some((HeldInput::Key(*scan_code, *extended), *pressed)),
            Self::EvdevKey { code, pressed } => Some((HeldInput::EvdevKey(*code), *pressed)),
            Self::MouseButton { button, pressed } => {
                Some((HeldInput::MouseButton(*button), *pressed))
            }
            _ => None,
        }
    }
}

/// Button bits consumed by the normalized Moonlight gamepad event. Reserved
/// bits are rejected instead of being forwarded or silently reinterpreted.
pub(crate) const GAMEPAD_BUTTON_MASK: u32 = ((1 << 11) - 1) | ((1 << 22) - (1 << 12));

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum HeldInput {
    Key(u8, bool),
    EvdevKey(u16),
    MouseButton(MouseButton),
}

impl HeldInput {
    pub(crate) fn release(self) -> InputEvent {
        match self {
            Self::Key(scan_code, extended) => InputEvent::Key {
                scan_code,
                extended,
                pressed: false,
            },
            Self::EvdevKey(code) => InputEvent::EvdevKey {
                code,
                pressed: false,
            },
            Self::MouseButton(button) => InputEvent::MouseButton {
                button,
                pressed: false,
            },
        }
    }
}

/// A versioned request from an ordinary worker. It carries no claimed caller.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct InputPacket {
    pub version: u16,
    pub generation: u64,
    pub sequence: u64,
    pub lease_token: String,
    pub event: InputEvent,
}
