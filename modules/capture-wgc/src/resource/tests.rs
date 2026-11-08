use super::*;
use crate::{
    backend::CaptureSession,
    format::{CaptureFormat, DesktopColor, SourceInfo},
    model::{CaptureResult, Frame, Operation, Output},
};
use amaterasu_module_sdk::{
    DynamicRange,
    abi::*,
    capture::{CpuSourceFactory, SourceFormat},
};
use std::{
    ffi::c_void,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

struct State {
    hdr: AtomicBool,
    opens: AtomicUsize,
    reads: AtomicUsize,
    drops: AtomicUsize,
}
struct TestBackend<const PADDED: bool> {
    state: Arc<State>,
}
struct TestSession<const PADDED: bool> {
    state: Arc<State>,
    format: CaptureFormat,
    delivered: bool,
}
impl<const PADDED: bool> Backend for TestBackend<PADDED> {
    fn supported(&self) -> bool {
        true
    }
    fn enumerate(&self) -> CaptureResult<Vec<Output>> {
        Ok(vec![])
    }
    fn open(&self, output: &str) -> CaptureResult<Box<dyn CaptureSession>> {
        self.open_with_format(output, CaptureFormat::default())
    }
    fn verify_source_format(&self, _: &str, format: CaptureFormat) -> CaptureResult<()> {
        DesktopColor {
            color_space: if self.state.hdr.load(Ordering::SeqCst) {
                12
            } else {
                0
            },
            bits_per_color: 10,
        }
        .verify(format)
    }
    fn open_with_format(
        &self,
        output: &str,
        format: CaptureFormat,
    ) -> CaptureResult<Box<dyn CaptureSession>> {
        self.verify_source_format(output, format)?;
        self.state.opens.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(TestSession::<PADDED> {
            state: self.state.clone(),
            format,
            delivered: false,
        }))
    }
}
impl<const PADDED: bool> CaptureSession for TestSession<PADDED> {
    fn validate_source(&mut self) -> CaptureResult<()> {
        if self.format.ten_bit && self.state.hdr.load(Ordering::SeqCst) != self.format.hdr {
            Err(CaptureError::new("access_lost", "native range changed"))
        } else {
            Ok(())
        }
    }
    fn source_info(&mut self) -> CaptureResult<Option<SourceInfo>> {
        Ok(Some(SourceInfo {
            width: 2,
            height: 2,
            encoding: self.format.encoding(),
            desktop_hdr: self.state.hdr.load(Ordering::SeqCst),
        }))
    }
    fn read(&mut self, _: u32) -> CaptureResult<Option<Frame>> {
        self.state.reads.fetch_add(1, Ordering::SeqCst);
        if self.delivered {
            return Ok(None);
        }
        self.delivered = true;
        let encoding = self.format.encoding();
        let (stride, length) = crate::model::source_frame_layout(2, 2, encoding)?;
        let bytes = if self.format.ten_bit {
            // Exact FP16 values beyond BGRA8 precision; both producers expose
            // the same scRGB texture using independent row storage layouts.
            let pixel = [0x01, 0x38, 0x02, 0x38, 0x03, 0x38, 0x00, 0x3c];
            let row = pixel.repeat(2);
            if PADDED {
                let mut mapped = Vec::new();
                mapped.extend_from_slice(&row);
                mapped.extend_from_slice(&[0xcc; 8]);
                mapped.extend_from_slice(&row);
                // SAFETY: two initialized 16-byte pixel rows at 24-byte pitch.
                unsafe { crate::model::copy_source_rows(2, 2, 24, mapped.as_ptr(), encoding)? }
            } else {
                row.repeat(2)
            }
        } else {
            vec![42; length]
        };
        Ok(Some(Frame {
            encoding,
            width: 2,
            height: 2,
            stride,
            timestamp_us: 100,
            capture_pacing_timestamp_us: 90,
            sequence: 1,
            bytes: Arc::new(bytes),
        }))
    }
}
impl<const PADDED: bool> Drop for TestSession<PADDED> {
    fn drop(&mut self) {
        self.state.drops.fetch_add(1, Ordering::SeqCst);
    }
}
fn setup<const PADDED: bool>(
    hdr: bool,
) -> (Arc<Mutex<Controller<TestBackend<PADDED>>>>, Arc<State>) {
    let state = Arc::new(State {
        hdr: AtomicBool::new(hdr),
        opens: AtomicUsize::new(0),
        reads: AtomicUsize::new(0),
        drops: AtomicUsize::new(0),
    });
    (
        Arc::new(Mutex::new(Controller::new(
            TestBackend {
                state: state.clone(),
            },
            true,
        ))),
        state,
    )
}
fn open<const PADDED: bool>(
    controller: &Arc<Mutex<Controller<TestBackend<PADDED>>>>,
    depth: u8,
    hdr: bool,
) -> serde_json::Value {
    controller.lock().unwrap().handle(
        "video",
        Operation::Open {
            output: "exact-output".into(),
            operation_id: Some("format-open".into()),
            constant_rate: true,
            source_format: Some(SourceFormat {
                bit_depth: depth,
                dynamic_range: if hdr {
                    DynamicRange::Hdr
                } else {
                    DynamicRange::Sdr
                },
            }),
        },
        &mut || Ok(()),
    )
}
unsafe fn bind<P: CpuSourceProvider + 'static>(
    factory: &CpuSourceFactory<P>,
    caller: &str,
) -> *const CaptureApiV1_10 {
    let query = NativeInterfaceQuery {
        header: AbiHeader::of::<NativeInterfaceQuery>(),
        interface_major: 1,
        min_minor: 10,
        min_size: size_of::<CaptureApiV1_10>() as u32,
    };
    let mut table = std::ptr::null();
    // SAFETY: live factory/query/caller and writable synchronous output.
    assert_eq!(
        unsafe {
            factory.api.bind.unwrap()(
                factory.api.context,
                BorrowedBytes::new(caller.as_bytes()),
                &query,
                &mut table,
            )
        },
        STATUS_OK
    );
    table.cast()
}
unsafe extern "C" fn collect_offer(
    context: *mut c_void,
    offer: *const CaptureResourceOffer,
) -> Status {
    // SAFETY: factory lends a valid offer/capability, test owns output.
    let offer = unsafe { &*offer };
    let out = unsafe { &mut *context.cast::<Vec<(u64, u32, u32)>>() };
    assert_eq!(offer.resource.version, 2);
    assert_eq!(offer.ownership, FRAME_OWNERSHIP_RETAINABLE);
    let caps = unsafe { &*offer.resource.data.cast::<CpuResourceCapabilitiesV2>() };
    out.push((offer.id, caps.base.format, caps.color.transfer));
    STATUS_OK
}
unsafe extern "C" fn collect_frame(
    context: *mut c_void,
    frame: *const CaptureResourceFrame,
) -> Status {
    // SAFETY: callback receives immutable CPU pixels until return; copy them.
    let frame = unsafe { &*frame };
    let cpu = unsafe { &*frame.resource.data.cast::<CpuFrame>() };
    assert_eq!(frame.resource.version, 2);
    assert_eq!(cpu.format, PIXEL_P010);
    assert_eq!(cpu.stride, 4);
    assert_eq!(cpu.len, 12);
    assert_eq!(unsafe { (frame.retain)(frame.context) }, STATUS_OK);
    let none = FrameDescriptor {
        kind: FRAME_SYNC_NONE,
        version: 0,
        size: 0,
        reserved: 0,
        namespace: [0; 16],
        data: std::ptr::null(),
    };
    unsafe { (frame.release)(frame.context, &none) };
    let out = unsafe { &mut *context.cast::<Vec<u8>>() };
    out.extend_from_slice(unsafe { std::slice::from_raw_parts(cpu.data, cpu.len) });
    STATUS_OK
}
fn unchanged_native_consumer<const PADDED: bool>(hdr: bool) -> Vec<u8> {
    let (controller, state) = setup::<PADDED>(hdr);
    let reply = open(&controller, 10, hdr);
    let stream = reply["stream"].as_u64().unwrap();
    let factory = CpuSourceFactory::new(Arc::new(Provider::new(controller.clone())));
    // SAFETY: native table and factory live through callbacks and release once.
    unsafe {
        let table = bind(&factory, "video");
        let api = &*table;
        let context = api.base.base.base.base.context;
        let mut generation = 0;
        let mut offers = Vec::<(u64, u32, u32)>::new();
        assert_eq!(
            (api.base.enumerate_resources)(
                context,
                stream,
                CaptureResourceOfferSink {
                    context: (&mut offers as *mut Vec<_>).cast(),
                    write: collect_offer
                },
                &mut generation
            ),
            STATUS_OK
        );
        assert_eq!(offers.len(), 2);
        assert!(offers.iter().all(|offer| offer.1 == PIXEL_P010
            && offer.2
                == if hdr {
                    FRAME_TRANSFER_PQ
                } else {
                    FRAME_TRANSFER_BT709
                }));
        assert_eq!(generation, stream);
        assert_eq!(
            (api.base.select_resource)(context, stream, generation, offers[0].0),
            STATUS_OK
        );
        let mut bytes = Vec::<u8>::new();
        let sink = CaptureResourceFrameSink {
            context: (&mut bytes as *mut Vec<_>).cast(),
            write: collect_frame,
        };
        assert_eq!(
            (api.base.read_resource_frame)(context, stream, sink),
            STATUS_OK
        );
        assert_eq!(state.reads.load(Ordering::SeqCst), 1);
        let mut hdr_metadata = HdrMetadata::default();
        assert_eq!(
            (api.base.hdr_metadata)(context, stream, &mut hdr_metadata),
            STATUS_OK
        );
        assert_eq!(hdr_metadata.enabled, u32::from(hdr));
        assert_eq!(hdr_metadata.fields, [0; 12]);
        let mut provenance = 0;
        assert_eq!(
            (api.color_provenance)(context, stream, &mut provenance),
            STATUS_OK
        );
        assert_eq!(provenance, CAPTURE_COLOR_PER_PRESENTATION_AUTHORITATIVE);
        controller.lock().unwrap().close("video", stream).unwrap();
        assert_eq!(state.drops.load(Ordering::SeqCst), 1);
        assert_eq!(
            (api.base.read_resource_frame)(context, stream, sink),
            MEDIA_CLOSED
        );
        api.base.base.base.base.release.unwrap()(context);
        bytes
    }
}
#[test]
fn unchanged_public_consumer_reads_both_native_layouts_in_ten_bit_sdr_and_hdr() {
    for hdr in [false, true] {
        let packed = unchanged_native_consumer::<false>(hdr);
        let padded = unchanged_native_consumer::<true>(hdr);
        assert_eq!(packed, padded);
        assert_eq!(packed.len(), 12);
    }
}
#[test]
fn owner_format_replay_and_native_range_loss_keep_source_lifecycle_exact() {
    let (controller, state) = setup::<false>(true);
    let reply = open(&controller, 10, true);
    let stream = reply["stream"].as_u64().unwrap();
    assert_eq!(open(&controller, 10, true)["stream"], stream);
    assert_eq!(
        open(&controller, 10, false)["error"]["code"],
        "operation_conflict"
    );
    assert_eq!(state.opens.load(Ordering::SeqCst), 1);
    let provider = Provider::new(controller.clone());
    assert!(matches!(
        provider.describe("foreign", stream),
        Err(MEDIA_CLOSED)
    ));
    let bgra = CpuSourceFormat {
        pixel_format: PIXEL_BGRA8,
        color: FrameColorMetadata::default(),
    };
    assert!(matches!(
        provider.read("video", stream, &bgra),
        Err(MEDIA_UNSUPPORTED)
    ));
    assert_eq!(state.reads.load(Ordering::SeqCst), 0);
    let format = provider.describe("video", stream).unwrap().formats[0];
    let first = provider.read("video", stream, &format).unwrap().unwrap();
    let owned = first.bytes.clone();
    assert_eq!(first.freshness, CAPTURE_FRAME_FRESH);
    let repeated = provider.read("video", stream, &format).unwrap().unwrap();
    assert_eq!(repeated.freshness, CAPTURE_FRAME_CACHED_REUSE);
    assert_eq!(repeated.bytes.as_ref(), owned.as_ref());
    assert_eq!(repeated.capture_pacing_timestamp_us, 90);
    state.hdr.store(false, Ordering::SeqCst);
    assert!(matches!(
        provider.describe("video", stream),
        Err(MEDIA_CLOSED)
    ));
    assert_eq!(state.drops.load(Ordering::SeqCst), 1);
    assert!(matches!(
        provider.read("video", stream, &format),
        Err(MEDIA_CLOSED)
    ));
    assert_eq!(owned.len(), 12);
}
#[test]
fn explicit_eight_bit_source_advertises_verified_sdr_color() {
    let (controller, _) = setup::<false>(false);
    let stream = open(&controller, 8, false)["stream"].as_u64().unwrap();
    let provider = Provider::new(controller);
    let description = provider.describe("video", stream).unwrap();
    assert!(description.authoritative_color);
    assert_eq!(description.formats.len(), 1);
    assert_eq!(description.formats[0].pixel_format, PIXEL_BGRA8);
    assert_eq!(description.formats[0].color.version, 1);
    assert_eq!(description.formats[0].color.transfer, FRAME_TRANSFER_SRGB);
    assert_eq!(description.hdr.unwrap().enabled, 0);
}

#[test]
fn explicit_eight_bit_source_rejects_hdr_scanout_before_native_capture() {
    let (controller, state) = setup::<false>(true);
    let reply = open(&controller, 8, false);
    assert_eq!(reply["error"]["code"], "source_range_mismatch");
    assert_eq!(state.opens.load(Ordering::SeqCst), 0);
}

unsafe extern "C" fn reject_frame(
    _context: *mut c_void,
    _frame: *const CaptureResourceFrame,
) -> Status {
    STATUS_INVALID_ARGUMENT
}

#[test]
fn public_resource_sink_rejection_discards_cached_pixels_without_closing_owner() {
    let (controller, state) = setup::<false>(false);
    let stream = open(&controller, 10, false)["stream"].as_u64().unwrap();
    let factory = CpuSourceFactory::new(Arc::new(Provider::new(controller.clone())));
    // SAFETY: table/factory and stack callbacks stay live, release occurs once.
    unsafe {
        let table = bind(&factory, "video");
        let api = &*table;
        let c = api.base.base.base.base.context;
        let mut generation = 0;
        let mut offers = Vec::<(u64, u32, u32)>::new();
        assert_eq!(
            (api.base.enumerate_resources)(
                c,
                stream,
                CaptureResourceOfferSink {
                    context: (&mut offers as *mut Vec<_>).cast(),
                    write: collect_offer
                },
                &mut generation
            ),
            STATUS_OK
        );
        assert_eq!(
            (api.base.select_resource)(c, stream, generation, offers[0].0),
            STATUS_OK
        );
        let sink = CaptureResourceFrameSink {
            context: std::ptr::null_mut(),
            write: reject_frame,
        };
        assert_eq!(
            (api.base.read_resource_frame)(c, stream, sink),
            STATUS_INVALID_ARGUMENT
        );
        assert_eq!(
            (api.base.read_resource_frame)(c, stream, sink),
            MEDIA_PENDING
        );
        assert_eq!(state.drops.load(Ordering::SeqCst), 0);
        controller.lock().unwrap().close("video", stream).unwrap();
        assert_eq!(state.drops.load(Ordering::SeqCst), 1);
        api.base.base.base.base.release.unwrap()(c);
    }
}
struct ClosingSink {
    controller: Arc<Mutex<Controller<TestBackend<false>>>>,
    stream: u64,
    pixels: Vec<u8>,
}
unsafe extern "C" fn close_then_copy(
    context: *mut c_void,
    frame: *const CaptureResourceFrame,
) -> Status {
    // SAFETY: test lends its context for this synchronous callback; the source
    // factory owns all descriptor data and bytes through callback completion.
    let sink = unsafe { &mut *context.cast::<ClosingSink>() };
    sink.controller
        .try_lock()
        .expect("capture lock must be released before foreign sink")
        .close("video", sink.stream)
        .unwrap();
    let descriptor = unsafe { &*frame };
    let pixels =
        unsafe { amaterasu_module_sdk::media::BorrowedResourceFrame::from_descriptor(descriptor) }
            .copy_cpu()
            .unwrap();
    sink.pixels = pixels.pixels.pixels().to_vec();
    STATUS_OK
}
#[test]
fn source_close_inside_public_callback_preserves_borrowed_ten_bit_pixels() {
    let (controller, state) = setup::<false>(true);
    let stream = open(&controller, 10, true)["stream"].as_u64().unwrap();
    let factory = CpuSourceFactory::new(Arc::new(Provider::new(controller.clone())));
    // SAFETY: live native binding, borrowed sink storage, single final release.
    unsafe {
        let table = bind(&factory, "video");
        let api = &*table;
        let c = api.base.base.base.base.context;
        let mut generation = 0;
        let mut offers = Vec::<(u64, u32, u32)>::new();
        assert_eq!(
            (api.base.enumerate_resources)(
                c,
                stream,
                CaptureResourceOfferSink {
                    context: (&mut offers as *mut Vec<_>).cast(),
                    write: collect_offer
                },
                &mut generation
            ),
            STATUS_OK
        );
        assert_eq!(
            (api.base.select_resource)(c, stream, generation, offers[0].0),
            STATUS_OK
        );
        let mut sink = ClosingSink {
            controller,
            stream,
            pixels: vec![],
        };
        assert_eq!(
            (api.base.read_resource_frame)(
                c,
                stream,
                CaptureResourceFrameSink {
                    context: (&mut sink as *mut ClosingSink).cast(),
                    write: close_then_copy
                }
            ),
            STATUS_OK
        );
        assert_eq!(sink.pixels.len(), 12);
        assert_eq!(state.drops.load(Ordering::SeqCst), 1);
        api.base.base.base.base.release.unwrap()(c);
        assert_eq!(sink.pixels.len(), 12);
    }
}
