/// HDR10 static metadata of the current captured presentation. Fields are RGB
/// primaries xy, white xy (1/50000), max nits, min 1/10000 nit, MaxCLL, MaxFALL.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HdrMetadata {
    pub enabled: u32,
    pub fields: [u16; 12],
    pub reserved: u32,
}

use crate::*;
use std::ffi::c_void;
/// Additive capture v1.9 prefix; output is initialized only on STATUS_OK.
#[repr(C)]
pub struct CaptureApiV1_9 {
    pub base: CaptureApiV1_7,
    pub enumerate_resources:
        unsafe extern "C" fn(*mut c_void, u64, CaptureResourceOfferSink, *mut u64) -> Status,
    pub select_resource: unsafe extern "C" fn(*mut c_void, u64, u64, u64) -> Status,
    pub read_resource_frame:
        unsafe extern "C" fn(*mut c_void, u64, CaptureResourceFrameSink) -> Status,
    pub hdr_metadata: unsafe extern "C" fn(*mut c_void, u64, *mut HdrMetadata) -> Status,
}
/// A capture provider guarantees that each delivered frame's color value is
/// derived from that presentation, rather than a default or inferred value.
/// Unknown color remains unknown and must be rejected when a known range is
/// required. This is a native provider contract, not a native-code sandbox.
pub const CAPTURE_COLOR_PER_PRESENTATION_AUTHORITATIVE: u32 = 1;
pub const CAPTURE_COLOR_PROVENANCE_INTERFACE_MINOR: u32 = 10;

/// Additive capture v1.10 tail. `color_provenance` authenticates the binding's
/// stream owner and writes flags only on STATUS_OK. Before the source has
/// established the property it returns MEDIA_PENDING; unavailable provenance
/// returns MEDIA_UNSUPPORTED or zero flags. The guarantee applies to every
/// delivered frame from this stream, including after source changes; a provider
/// must stop delivery or revoke the guarantee before it can no longer honor it.
/// Consumers still validate each frame's recognized color against their need.
#[repr(C)]
pub struct CaptureApiV1_10 {
    pub base: CaptureApiV1_9,
    pub color_provenance: unsafe extern "C" fn(*mut c_void, u64, *mut u32) -> Status,
}

/// Additive video v1.2 prefix; metadata belongs to the caller-owned stream.
#[repr(C)]
pub struct VideoApiV1_2 {
    pub base: amaterasu_abi::NativeInterface,
    pub receive: unsafe extern "C" fn(*mut c_void, u64, PacketSink) -> Status,
    pub geometry: unsafe extern "C" fn(*mut c_void, u64, *mut CaptureGeometry) -> Status,
    pub hdr_metadata: unsafe extern "C" fn(*mut c_void, u64, *mut HdrMetadata) -> Status,
}
/// Additive video v1.3 tail. Legacy receive retains its existing semantics.
/// Timed output carries an independent processing start, or zero when unknown.
#[repr(C)]
pub struct VideoApiV1_3 {
    pub base: VideoApiV1_2,
    pub receive_timed: unsafe extern "C" fn(*mut c_void, u64, TimedPacketSink) -> Status,
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::{offset_of, size_of};
    #[test]
    fn hdr_tails_preserve_existing_prefixes() {
        assert_eq!(size_of::<HdrMetadata>(), 32);
        assert_eq!(
            offset_of!(CaptureApiV1_9, hdr_metadata),
            size_of::<CaptureApiV1_8>()
        );
        assert_eq!(
            offset_of!(CaptureApiV1_10, color_provenance),
            size_of::<CaptureApiV1_9>()
        );
        assert_eq!(
            offset_of!(VideoApiV1_2, hdr_metadata),
            size_of::<VideoApiV1_1>()
        );
        assert_eq!(
            offset_of!(VideoApiV1_3, receive_timed),
            size_of::<VideoApiV1_2>()
        );
    }
}
