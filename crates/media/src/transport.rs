//! Encoded media admission to an owned network transport.
use crate::{EncodedAudioPacket, EncodedPacket, TimedEncodedPacket};
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

/// Additive media-transport v1.1 tail for an independent host-processing
/// timestamp. `processing_started_host_us == 0` means unavailable; providers
/// must not substitute a presentation timestamp for it. Admission and
/// borrowed-buffer guarantees match `submit_video`.
#[repr(C)]
pub struct MediaTransportApiV1_1 {
    pub base: MediaTransportApi,
    pub submit_video_timed:
        unsafe extern "C" fn(*mut c_void, u64, *const TimedEncodedPacket) -> Status,
}

#[cfg(all(test, target_pointer_width = "64"))]
mod tests {
    use super::*;
    use crate::{CODEC_HEVC, FLAG_END_OF_ACCESS_UNIT, TimedPacketSink};
    use amaterasu_abi::{STATUS_INVALID_ARGUMENT, STATUS_OK};
    use std::ffi::c_void;

    #[test]
    fn transport_table_matches_public_c_layout() {
        assert_eq!(size_of::<MediaTransportApi>(), 56);
        assert_eq!(std::mem::offset_of!(MediaTransportApi, base), 0);
        assert_eq!(std::mem::offset_of!(MediaTransportApi, submit_video), 32);
        assert_eq!(std::mem::offset_of!(MediaTransportApi, submit_audio), 40);
        assert_eq!(std::mem::offset_of!(MediaTransportApi, drain), 48);
        assert_eq!(
            std::mem::offset_of!(MediaTransportApiV1_1, submit_video_timed),
            size_of::<MediaTransportApi>()
        );
        assert_eq!(size_of::<MediaTransportApiV1_1>(), 64);
        assert_eq!(
            std::mem::offset_of!(TimedEncodedPacket, processing_started_host_us),
            size_of::<EncodedPacket>()
        );
    }

    #[test]
    fn timed_sink_keeps_presentation_and_processing_clocks_distinct() {
        unsafe extern "C" fn copy(
            context: *mut c_void,
            packet: *const TimedEncodedPacket,
        ) -> Status {
            // SAFETY: both pointers are live for this synchronous test callback.
            let observed = unsafe { &mut *context.cast::<Vec<(u64, u64)>>() };
            let packet = unsafe { &*packet };
            observed.push((packet.base.timestamp_us, packet.processing_started_host_us));
            STATUS_OK
        }
        let bytes = [1_u8];
        let mut observed: Vec<(u64, u64)> = Vec::new();
        let sink = TimedPacketSink {
            context: (&mut observed as *mut Vec<(u64, u64)>).cast(),
            write: copy,
        };
        let mut packet = TimedEncodedPacket {
            base: EncodedPacket {
                codec: CODEC_HEVC,
                flags: FLAG_END_OF_ACCESS_UNIT,
                data: bytes.as_ptr(),
                len: bytes.len(),
                timestamp_us: 5_000,
                sequence: 1,
            },
            processing_started_host_us: 6_000,
        };
        // SAFETY: sink, callback context and byte allocation remain live.
        assert_eq!(unsafe { sink.send(&packet) }, STATUS_OK);
        packet.base.data = std::ptr::null();
        // SAFETY: invalid base is validated before callback or dereference.
        assert_eq!(unsafe { sink.send(&packet) }, STATUS_INVALID_ARGUMENT);
        assert_eq!(observed, vec![(5_000, 6_000)]);
    }
}
