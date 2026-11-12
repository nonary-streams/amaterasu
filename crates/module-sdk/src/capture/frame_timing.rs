//! Provider-reported host timing of individual capture presentations.
use crate::abi::{STATUS_OK, Status};
use amaterasu_media::{
    CaptureApiPrefix, CaptureApiV1_12, CaptureFrameTimingV1, HOST_PROCESSING_LATENCY_WINDOW_US,
    MEDIA_BACKEND_ERROR, MEDIA_PENDING, MEDIA_UNSUPPORTED, capture_frame_timing_available,
};

/// Asks a v1.12 capture provider when it began handling the presentation with
/// stream `sequence`, on the host monotonic clock in microseconds.
///
/// `Ok(None)` means the provider supports the query but has no known start for
/// that presentation. `Err(MEDIA_UNSUPPORTED)` means the table predates v1.12 or
/// the stream supplies no timing. A record for a different sequence is a
/// provider protocol error. Every outcome other than `Ok(Some(_))` should leave
/// the consumer on its own measurement; none of them should fail a stream.
///
/// # Safety
/// `api` must be the prefix of a live capture binding of the advertised capture
/// interface family, with its complete advertised table allocated. The binding
/// and stream must remain live through the synchronous callback.
pub unsafe fn frame_processing_start(
    api: &CaptureApiPrefix,
    stream: u64,
    sequence: u64,
) -> Result<Option<u64>, Status> {
    if !capture_frame_timing_available(api.base.header) {
        return Err(MEDIA_UNSUPPORTED);
    }
    // SAFETY: the caller promises an authentic live capture table; major,
    // minor and size cover the v1.12 tail for this synchronous call.
    let table = unsafe { &*std::ptr::from_ref(api).cast::<CaptureApiV1_12>() };
    let mut timing = CaptureFrameTimingV1::default();
    // SAFETY: provider receives its bound context and a live, writable output.
    let status = unsafe { (table.frame_timing)(api.base.context, stream, sequence, &mut timing) };
    match status {
        STATUS_OK if timing.sequence != sequence => Err(MEDIA_BACKEND_ERROR),
        STATUS_OK if timing.processing_started_host_us == 0 => Ok(None),
        STATUS_OK => Ok(Some(timing.processing_started_host_us)),
        MEDIA_PENDING => Ok(None),
        status => Err(status),
    }
}

/// Chooses the processing start a consumer reports for one frame. A provider
/// start replaces the consumer's own `local_us` only when it is known, not
/// later than `local_us` and within the host latency sanity window; otherwise
/// the consumer keeps its own measurement. Zero stays "unknown".
pub fn select_processing_start(provider_us: Option<u64>, local_us: u64) -> u64 {
    match provider_us {
        Some(provider)
            if provider != 0
                && local_us != 0
                && provider <= local_us
                && local_us - provider <= HOST_PROCESSING_LATENCY_WINDOW_US =>
        {
            provider
        }
        _ => local_us,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::abi::{AbiHeader, BorrowedBytes, NativeInterface};
    use amaterasu_media::{
        CAPTURE_API_V1_12_SIZE, CaptureApiV1_6, CaptureApiV1_7, CaptureApiV1_9, CaptureApiV1_10,
        CaptureApiV1_11, CaptureEventSink, CaptureGeometry, CaptureResourceFrameSink,
        CaptureResourceOfferSink, DmaBufFrameSink, DmaBufLeaseSink, FrameSink, FreshFrameSinkV1_2,
        FreshFrameSinkV1_3, HdrMetadata, INTERFACE_MAJOR, PresentationStatus,
    };
    use std::{cell::Cell, ffi::c_void};

    // Function values stand in for unused callbacks: zeroed function pointers
    // would violate Rust's validity rules.
    unsafe extern "C" fn frame(_: *mut c_void, _: u64, _: FrameSink) -> Status {
        MEDIA_UNSUPPORTED
    }
    unsafe extern "C" fn geometry(_: *mut c_void, _: u64, _: *mut CaptureGeometry) -> Status {
        MEDIA_UNSUPPORTED
    }
    unsafe extern "C" fn fresh(_: *mut c_void, _: u64, _: FreshFrameSinkV1_2) -> Status {
        MEDIA_UNSUPPORTED
    }
    unsafe extern "C" fn timed(_: *mut c_void, _: u64, _: FreshFrameSinkV1_3) -> Status {
        MEDIA_UNSUPPORTED
    }
    unsafe extern "C" fn dmabuf(_: *mut c_void, _: u64, _: DmaBufFrameSink) -> Status {
        MEDIA_UNSUPPORTED
    }
    unsafe extern "C" fn presentation(
        _: *mut c_void,
        _: u64,
        _: *mut PresentationStatus,
    ) -> Status {
        MEDIA_UNSUPPORTED
    }
    unsafe extern "C" fn leased(_: *mut c_void, _: u64, _: DmaBufLeaseSink) -> Status {
        MEDIA_UNSUPPORTED
    }
    unsafe extern "C" fn subscribe(_: *mut c_void, _: u64, _: CaptureEventSink) -> Status {
        MEDIA_UNSUPPORTED
    }
    unsafe extern "C" fn unsubscribe(_: *mut c_void, _: u64) {}
    unsafe extern "C" fn swap(_: *mut c_void, _: u64, _: u64, _: u32, _: BorrowedBytes) -> Status {
        MEDIA_UNSUPPORTED
    }
    unsafe extern "C" fn enumerate(
        _: *mut c_void,
        _: u64,
        _: CaptureResourceOfferSink,
        _: *mut u64,
    ) -> Status {
        MEDIA_UNSUPPORTED
    }
    unsafe extern "C" fn select(_: *mut c_void, _: u64, _: u64, _: u64) -> Status {
        MEDIA_UNSUPPORTED
    }
    unsafe extern "C" fn resource(_: *mut c_void, _: u64, _: CaptureResourceFrameSink) -> Status {
        MEDIA_UNSUPPORTED
    }
    unsafe extern "C" fn metadata(_: *mut c_void, _: u64, _: *mut HdrMetadata) -> Status {
        MEDIA_UNSUPPORTED
    }
    unsafe extern "C" fn provenance(_: *mut c_void, _: u64, _: *mut u32) -> Status {
        MEDIA_UNSUPPORTED
    }

    /// Scripted provider answer: status and the record it writes on OK.
    type Answer = Cell<(Status, CaptureFrameTimingV1)>;

    unsafe extern "C" fn timing(
        context: *mut c_void,
        _: u64,
        sequence: u64,
        output: *mut CaptureFrameTimingV1,
    ) -> Status {
        // SAFETY: the fixture keeps the answer alive for the table lifetime.
        let (status, mut record) = unsafe { &*context.cast::<Answer>() }.get();
        if record.sequence == u64::MAX {
            record.sequence = sequence;
        }
        // SAFETY: the adapter passes a live, writable output record.
        unsafe { output.write(record) };
        status
    }

    fn v1_10(context: *mut c_void, minor: u32, size: u32) -> CaptureApiV1_10 {
        CaptureApiV1_10 {
            base: CaptureApiV1_9 {
                base: CaptureApiV1_7 {
                    base: CaptureApiV1_6 {
                        base: NativeInterface {
                            header: AbiHeader {
                                size,
                                major: INTERFACE_MAJOR,
                                minor,
                            },
                            context,
                            release: None,
                        },
                        read_frame: frame,
                        geometry,
                        read_frame_with_freshness: fresh,
                        read_frame_with_capture_timestamp: timed,
                        read_dmabuf_frame: dmabuf,
                        latest_presentation: presentation,
                        read_dmabuf_frame_leased: leased,
                    },
                    subscribe_events: subscribe,
                    unsubscribe_events: unsubscribe,
                    resolve_swap: swap,
                },
                enumerate_resources: enumerate,
                select_resource: select,
                read_resource_frame: resource,
                hdr_metadata: metadata,
            },
            color_provenance: provenance,
        }
    }

    unsafe extern "C" fn preflight(
        _: *mut c_void,
        _: BorrowedBytes,
        _: crate::abi::ReplySink,
    ) -> Status {
        MEDIA_UNSUPPORTED
    }

    fn v1_11(context: *mut c_void, minor: u32, size: u32) -> CaptureApiV1_11 {
        CaptureApiV1_11 {
            base: v1_10(context, minor, size),
            source_preflight: preflight,
        }
    }

    fn prefix<T>(table: &T) -> &CaptureApiPrefix {
        // SAFETY: every capture table begins with the v1.0 prefix.
        unsafe { &*std::ptr::from_ref(table).cast::<CaptureApiPrefix>() }
    }

    #[test]
    fn a_v1_10_provider_reports_no_timing_and_the_consumer_keeps_its_own_start() {
        // Replacement: an unchanged consumer bound to a provider that predates
        // v1.12 must not read past its table and keeps its local measurement.
        let table = v1_10(
            std::ptr::null_mut(),
            10,
            std::mem::size_of::<CaptureApiV1_10>() as u32,
        );
        // SAFETY: the complete v1.10 table is live for this synchronous query.
        let provider = unsafe { frame_processing_start(prefix(&table), 7, 3) };
        assert_eq!(provider, Err(MEDIA_UNSUPPORTED));
        assert_eq!(
            select_processing_start(provider.ok().flatten(), 9_000),
            9_000
        );
    }

    #[test]
    fn a_v1_12_provider_supplies_the_start_of_the_requested_presentation() {
        let answer: Answer = Cell::new((
            STATUS_OK,
            CaptureFrameTimingV1 {
                sequence: u64::MAX,
                processing_started_host_us: 4_000,
            },
        ));
        let mut table = CaptureApiV1_12 {
            base: v1_11(
                std::ptr::from_ref(&answer).cast_mut().cast(),
                12,
                CAPTURE_API_V1_12_SIZE,
            ),
            frame_timing: timing,
        };
        let query = |table: &CaptureApiV1_12| {
            // SAFETY: the complete advertised table is live for the query.
            unsafe { frame_processing_start(prefix(table), 7, 3) }
        };
        assert_eq!(query(&table), Ok(Some(4_000)));
        answer.set((
            STATUS_OK,
            CaptureFrameTimingV1 {
                sequence: u64::MAX,
                processing_started_host_us: 0,
            },
        ));
        assert_eq!(query(&table), Ok(None));
        answer.set((MEDIA_PENDING, CaptureFrameTimingV1::default()));
        assert_eq!(query(&table), Ok(None));
        answer.set((
            STATUS_OK,
            CaptureFrameTimingV1 {
                sequence: 2,
                processing_started_host_us: 4_000,
            },
        ));
        assert_eq!(query(&table), Err(MEDIA_BACKEND_ERROR));
        answer.set((MEDIA_UNSUPPORTED, CaptureFrameTimingV1::default()));
        assert_eq!(query(&table), Err(MEDIA_UNSUPPORTED));
        // An advertised minor without the matching size is never read.
        table.base.base.base.base.base.base.header.size =
            std::mem::size_of::<CaptureApiV1_10>() as u32;
        assert_eq!(query(&table), Err(MEDIA_UNSUPPORTED));
    }

    #[test]
    fn provider_starts_apply_only_inside_the_sanity_window() {
        assert_eq!(select_processing_start(Some(4_000), 9_000), 4_000);
        assert_eq!(select_processing_start(Some(9_000), 9_000), 9_000);
        assert_eq!(select_processing_start(None, 9_000), 9_000);
        assert_eq!(select_processing_start(Some(0), 9_000), 9_000);
        // A start from the future did not come from this clock.
        assert_eq!(select_processing_start(Some(9_001), 9_000), 9_000);
        let local = 10 * HOST_PROCESSING_LATENCY_WINDOW_US;
        let oldest = local - HOST_PROCESSING_LATENCY_WINDOW_US;
        assert_eq!(select_processing_start(Some(oldest), local), oldest);
        assert_eq!(select_processing_start(Some(oldest - 1), local), local);
        // Zero keeps meaning unknown.
        assert_eq!(select_processing_start(Some(4_000), 0), 0);
    }
}
