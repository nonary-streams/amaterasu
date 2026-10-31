//! Shared address selection and port mapping policy for public services.
//! Production socket operations belong to the selected network provider.

mod address;
mod endpoint;
mod ports;
#[cfg(any(test, feature = "test-support"))]
mod udp;

pub use address::{
    AddressFamily, ListenPolicy, invalid_configured_bind, valid_peer, validate_local_bind,
};
pub use endpoint::validate_bound_endpoint;
pub use ports::{
    AUDIO_OFFSET, CONTROL_OFFSET, DEFAULT_BASE_PORT, GameStreamPorts, HTTPS_OFFSET, MAX_BASE_PORT,
    MIN_BASE_PORT, RTSP_OFFSET, VIDEO_OFFSET,
};
#[cfg(any(test, feature = "test-support"))]
pub use udp::bind_udp;
