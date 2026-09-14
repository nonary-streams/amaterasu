//! Encoded media admission to an owned network transport.
use crate::{EncodedAudioPacket, EncodedPacket};
use amaterasu_abi::{NativeInterface, Status};
use std::ffi::c_void;

/// The trusted control caller must authorize the destination and negotiate the
/// profile before opening a transport. The provider enforces stream ownership;
/// it does not authenticate peers. Accepted data is copied into bounded queues;
/// success means queued locally, never proof of peer delivery or decoding.
#[repr(C)]
pub struct MediaTransportApi {
    pub base: NativeInterface,
    /// Submit one complete H.264 Annex B access unit. Consumers must assemble any
    /// producer fragments first. STATUS_OK accepts all bytes; MEDIA_BUSY accepts
    /// nothing and consumes no packet indices or crypto nonces. EOS has no payload.
    pub submit_video: unsafe extern "C" fn(*mut c_void, u64, *const EncodedPacket) -> Status,
    /// Submit one complete Opus packet matching the negotiated duration and wire
    /// size. Input bytes remain borrowed only during the call. EOS has no payload.
    pub submit_audio: unsafe extern "C" fn(*mut c_void, u64, *const EncodedAudioPacket) -> Status,
    /// Stop accepting input and flush queued datagrams. A successful call starts
    /// draining; control status reports when the local queue has emptied. No EOS
    /// datagram or peer acknowledgement is invented by the transport.
    pub drain: unsafe extern "C" fn(*mut c_void, u64) -> Status,
}

#[cfg(all(test, target_pointer_width = "64"))]
mod tests {
    use super::*;

    #[test]
    fn transport_table_matches_public_c_layout() {
        assert_eq!(size_of::<MediaTransportApi>(), 56);
        assert_eq!(std::mem::offset_of!(MediaTransportApi, base), 0);
        assert_eq!(std::mem::offset_of!(MediaTransportApi, submit_video), 32);
        assert_eq!(std::mem::offset_of!(MediaTransportApi, submit_audio), 40);
        assert_eq!(std::mem::offset_of!(MediaTransportApi, drain), 48);
    }
}
