//! Host-owned policy model for a future virtual-input companion.
//!
//! This crate does not authenticate OS connections or open a VHF device. Only a
//! separately installed companion may use it, after binding `PeerIdentity` to
//! the accepted OS connection and issuing `InputGrant` from trusted host policy.
//! Production transport and driver adapters are deliberately absent.

mod model;
mod session;

pub use model::{InputEvent, InputPacket, MouseButton, PROTOCOL_VERSION, PeerIdentity};
pub use session::{DeviceSink, GrantRegistry, InputGrant, InputSession};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InputError {
    InvalidPeer,
    InvalidGrant,
    PermissionDenied,
    InvalidEvent,
    InvalidSequence,
    ResourceExhausted,
    Closed,
    Expired,
    Device,
}

pub type InputResult<T> = Result<T, InputError>;

#[cfg(test)]
mod tests;
