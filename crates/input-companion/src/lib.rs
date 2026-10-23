//! Host-owned policy model and restricted VHF device adapter for a future
//! installed virtual-input companion.
//!
//! This crate does not authenticate OS connections. Only a
//! separately installed companion may use it, after binding `PeerIdentity` to
//! the accepted OS connection and issuing `InputGrant` from trusted host policy.
//! The Windows device adapter is present but no production service/IPC grants
//! are wired to it yet.

mod model;
mod session;
mod vhf;

pub use model::{InputEvent, InputPacket, MouseButton, PROTOCOL_VERSION, PeerIdentity};
pub use session::{DeviceSink, GrantRegistry, InputGrant, InputSession};
pub use vhf::{VhfDeviceSink, VhfTransport};

#[cfg(windows)]
mod windows_vhf;
#[cfg(windows)]
pub use windows_vhf::WindowsVhfTransport;

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
