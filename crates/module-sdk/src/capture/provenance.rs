//! Per-stream capture color provenance for consumers of retained frames.
use crate::abi::{STATUS_OK, Status};
use amaterasu_media::{
    CAPTURE_COLOR_PER_PRESENTATION_AUTHORITATIVE, CAPTURE_COLOR_PROVENANCE_INTERFACE_MINOR,
    CaptureApiPrefix, CaptureApiV1_10, INTERFACE_MAJOR, MEDIA_BACKEND_ERROR, MEDIA_UNSUPPORTED,
};

/// Query whether each frame color is derived from its own source presentation.
/// A false result does not establish the frame color. Callers must also inspect
/// the color of every delivered frame and reject unknown or unsuitable values.
/// Errors including PENDING and BUSY are preserved for the caller to retry.
///
/// # Safety
/// `api` must be the prefix of a live capture binding of the advertised capture
/// interface family, with its complete advertised table allocated. The binding
/// and stream must remain live through the synchronous callback.
pub unsafe fn color_provenance(api: &CaptureApiPrefix, stream: u64) -> Result<bool, Status> {
    let header = api.base.header;
    if header.major != INTERFACE_MAJOR
        || header.minor < CAPTURE_COLOR_PROVENANCE_INTERFACE_MINOR
        || header.size < size_of::<CaptureApiV1_10>() as u32
    {
        return Err(MEDIA_UNSUPPORTED);
    }
    // SAFETY: the caller promises an authentic live capture table; both minor
    // and size cover the v1.10 tail for this synchronous call.
    let table = unsafe { &*std::ptr::from_ref(api).cast::<CaptureApiV1_10>() };
    let mut flags = 0;
    // SAFETY: provider receives its bound context and a live output pointer.
    let status = unsafe { (table.color_provenance)(api.base.context, stream, &mut flags) };
    if status != STATUS_OK {
        return Err(status);
    }
    match flags {
        0 => Ok(false),
        CAPTURE_COLOR_PER_PRESENTATION_AUTHORITATIVE => Ok(true),
        _ => Err(MEDIA_BACKEND_ERROR),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::abi::{AbiHeader, NativeInterface};
    use amaterasu_media::{CaptureApiV1_7, CaptureApiV1_9, MEDIA_PENDING};
    use std::ffi::c_void;

    unsafe extern "C" fn provenance(context: *mut c_void, _: u64, out: *mut u32) -> Status {
        // SAFETY: the fixture supplies stable flag storage and a live output pointer.
        let value = unsafe { &*context.cast::<std::cell::Cell<u32>>() }.get();
        if value == 99 {
            return MEDIA_PENDING;
        }
        unsafe { *out = value };
        STATUS_OK
    }

    #[test]
    fn recognizes_only_explicit_provenance_and_complete_v1_10_tables() {
        // The unused callbacks are represented with function values rather than
        // zeroed function pointers, which would violate Rust's validity rules.
        unsafe extern "C" fn unused_frame(
            _: *mut c_void,
            _: u64,
            _: amaterasu_media::FrameSink,
        ) -> Status {
            MEDIA_UNSUPPORTED
        }
        unsafe extern "C" fn geometry(
            _: *mut c_void,
            _: u64,
            _: *mut amaterasu_media::CaptureGeometry,
        ) -> Status {
            MEDIA_UNSUPPORTED
        }
        unsafe extern "C" fn fresh(
            _: *mut c_void,
            _: u64,
            _: amaterasu_media::FreshFrameSinkV1_2,
        ) -> Status {
            MEDIA_UNSUPPORTED
        }
        unsafe extern "C" fn timestamp(
            _: *mut c_void,
            _: u64,
            _: amaterasu_media::FreshFrameSinkV1_3,
        ) -> Status {
            MEDIA_UNSUPPORTED
        }
        unsafe extern "C" fn dmabuf(
            _: *mut c_void,
            _: u64,
            _: amaterasu_media::DmaBufFrameSink,
        ) -> Status {
            MEDIA_UNSUPPORTED
        }
        unsafe extern "C" fn presentation(
            _: *mut c_void,
            _: u64,
            _: *mut amaterasu_media::PresentationStatus,
        ) -> Status {
            MEDIA_UNSUPPORTED
        }
        unsafe extern "C" fn lease(
            _: *mut c_void,
            _: u64,
            _: amaterasu_media::DmaBufLeaseSink,
        ) -> Status {
            MEDIA_UNSUPPORTED
        }
        unsafe extern "C" fn subscribe(
            _: *mut c_void,
            _: u64,
            _: amaterasu_media::CaptureEventSink,
        ) -> Status {
            MEDIA_UNSUPPORTED
        }
        unsafe extern "C" fn unsubscribe(_: *mut c_void, _: u64) {}
        unsafe extern "C" fn swap(
            _: *mut c_void,
            _: u64,
            _: u64,
            _: u32,
            _: crate::abi::BorrowedBytes,
        ) -> Status {
            MEDIA_UNSUPPORTED
        }
        unsafe extern "C" fn enumerate(
            _: *mut c_void,
            _: u64,
            _: amaterasu_media::CaptureResourceOfferSink,
            _: *mut u64,
        ) -> Status {
            MEDIA_UNSUPPORTED
        }
        unsafe extern "C" fn select(_: *mut c_void, _: u64, _: u64, _: u64) -> Status {
            MEDIA_UNSUPPORTED
        }
        unsafe extern "C" fn resource(
            _: *mut c_void,
            _: u64,
            _: amaterasu_media::CaptureResourceFrameSink,
        ) -> Status {
            MEDIA_UNSUPPORTED
        }
        unsafe extern "C" fn metadata(
            _: *mut c_void,
            _: u64,
            _: *mut amaterasu_media::HdrMetadata,
        ) -> Status {
            MEDIA_UNSUPPORTED
        }
        let flags = std::cell::Cell::new(0u32);
        let mut table = CaptureApiV1_10 {
            base: CaptureApiV1_9 {
                base: CaptureApiV1_7 {
                    base: amaterasu_media::CaptureApiV1_6 {
                        base: NativeInterface {
                            header: AbiHeader {
                                size: size_of::<CaptureApiV1_10>() as u32,
                                major: INTERFACE_MAJOR,
                                minor: 10,
                            },
                            context: std::ptr::from_ref(&flags).cast_mut().cast(),
                            release: None,
                        },
                        read_frame: unused_frame,
                        geometry,
                        read_frame_with_freshness: fresh,
                        read_frame_with_capture_timestamp: timestamp,
                        read_dmabuf_frame: dmabuf,
                        latest_presentation: presentation,
                        read_dmabuf_frame_leased: lease,
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
        };
        let query = |table: &CaptureApiV1_10| unsafe {
            color_provenance(&*std::ptr::from_ref(table).cast::<CaptureApiPrefix>(), 4)
        };
        assert_eq!(query(&table), Ok(false));
        flags.set(CAPTURE_COLOR_PER_PRESENTATION_AUTHORITATIVE);
        assert_eq!(query(&table), Ok(true));
        flags.set(99);
        assert_eq!(query(&table), Err(MEDIA_PENDING));
        flags.set(2);
        assert_eq!(query(&table), Err(MEDIA_BACKEND_ERROR));
        table.base.base.base.base.header.minor = 9;
        assert_eq!(query(&table), Err(MEDIA_UNSUPPORTED));
        table.base.base.base.base.header.minor = 10;
        table.base.base.base.base.header.size = size_of::<CaptureApiV1_9>() as u32;
        assert_eq!(query(&table), Err(MEDIA_UNSUPPORTED));
    }
}
