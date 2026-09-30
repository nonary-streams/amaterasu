//! Shared address selection, port mapping, and UDP binding for public services.

mod address;
mod endpoint;
mod ports;
mod udp;

pub use address::{
    AddressFamily, ListenPolicy, invalid_configured_bind, valid_peer, validate_local_bind,
};
pub use endpoint::validate_bound_endpoint;
pub use ports::{
    AUDIO_OFFSET, CONTROL_OFFSET, DEFAULT_BASE_PORT, GameStreamPorts, HTTPS_OFFSET, MAX_BASE_PORT,
    MIN_BASE_PORT, RTSP_OFFSET, VIDEO_OFFSET,
};
pub use udp::bind_udp;
