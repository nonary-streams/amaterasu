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
            offset_of!(VideoApiV1_2, hdr_metadata),
            size_of::<VideoApiV1_1>()
        );
        assert_eq!(
            offset_of!(VideoApiV1_3, receive_timed),
            size_of::<VideoApiV1_2>()
        );
    }
}
