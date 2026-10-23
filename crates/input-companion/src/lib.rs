//! Host-owned permit model, authenticated Windows pipe service, and restricted
//! VHF adapter for a separately installed virtual-input companion. Native
//! modules remain trusted user code; this service never loads them.

mod model;
mod permit;
mod session;
mod vhf;

pub use model::{InputEvent, InputPacket, MouseButton, PROTOCOL_VERSION, PeerIdentity};
pub use permit::{PermitLedger, ProcessIdentity, WorkerLease};
pub use session::{DeviceSink, GrantRegistry, InputGrant, InputSession};
pub use vhf::{VhfDeviceSink, VhfTransport};

#[cfg(windows)]
mod windows_identity;
#[cfg(windows)]
mod windows_pipe;
#[cfg(windows)]
mod windows_service;
#[cfg(windows)]
mod windows_vhf;
#[cfg(windows)]
pub use windows_service::{WindowsInputClient, authorize_worker};
#[cfg(windows)]
pub use windows_service::{run as run_installed_service, wake_listener};
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
    Storage,
}

pub type InputResult<T> = Result<T, InputError>;

#[cfg(test)]
mod tests;
