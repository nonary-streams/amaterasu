//! Exercise an unchanged public CPU resource consumer against DXGI's controller.
use super::*;
use crate::{
    backend::{Backend, CaptureSession},
    format::{CaptureFormat, SourceInfo},
    model::{CaptureResult, Frame, Output},
};
use amaterasu_media::*;
use amaterasu_module_sdk::{
    DynamicRange,
    abi::*,
    capture::{
        SourceFormat,
        high_depth::{RgbEncoding, RgbSurface},
    },
    media::{BorrowedResourceFrame, OwnedResourceCpuFrame},
};
use std::{
    ffi::c_void,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};
struct State {
    valid: AtomicBool,
    opens: AtomicUsize,
    drops: AtomicUsize,
    hdr: bool,
}
struct TestBackend(Arc<State>);
struct TestSession {
    state: Arc<State>,
    format: CaptureFormat,
}
impl Backend for TestBackend {
    fn supported(&self) -> bool {
        true
    }
    fn enumerate(&self) -> CaptureResult<Vec<Output>> {
        Ok(Vec::new())
    }
    fn open(&self, output: &str) -> CaptureResult<Box<dyn CaptureSession>> {
        self.open_with_format(output, CaptureFormat::default())
    }
    fn verify_source_format(&self, _: &str, format: CaptureFormat) -> CaptureResult<()> {
        if format.explicit && format.hdr != self.0.hdr {
            Err(CaptureError::new(
                "source_range_mismatch",
                "native range differs",
            ))
        } else {
            Ok(())
        }
    }
    fn open_with_format(
        &self,
        _: &str,
        format: CaptureFormat,
    ) -> CaptureResult<Box<dyn CaptureSession>> {
        self.0.opens.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(TestSession {
            state: self.0.clone(),
            format,
        }))
    }
}
impl CaptureSession for TestSession {
    fn source_info(&mut self) -> CaptureResult<Option<SourceInfo>> {
        if !self.state.valid.load(Ordering::SeqCst) {
            return Err(CaptureError::new(
                "access_lost",
                "exact native color changed",
            ));
        }
        Ok(Some(SourceInfo {
            width: 2,
            height: 2,
            format: self.format,
            encoding: RgbEncoding::Rgba16FloatScRgb,
        }))
    }
    fn read(&mut self, _: u32) -> CaptureResult<Option<Frame>> {
        let value: u16 = if self.format.hdr { 0x3c00 } else { 0x3800 };
        let mut bytes = Vec::new();
        for _ in 0..4 {
            for channel in [value, value, value, 0x3c00] {
                bytes.extend_from_slice(&channel.to_le_bytes());
            }
        }
        Ok(Some(Frame {
            width: 2,
            height: 2,
            stride: 16,
            timestamp_us: 20,
            sequence: 1,
            bytes: Vec::new(),
            surface: Some(RgbSurface {
                width: 2,
                height: 2,
                stride: 16,
                encoding: RgbEncoding::Rgba16FloatScRgb,
                bytes: Arc::new(bytes),
            }),
        }))
    }
}
impl Drop for TestSession {
    fn drop(&mut self) {
        self.state.drops.fetch_add(1, Ordering::SeqCst);
    }
}
struct Binding(*const CaptureApiV1_10);
impl Binding {
    fn new(factory: &native::Factory<TestBackend>) -> Self {
        let query = NativeInterfaceQuery {
            header: AbiHeader::of::<NativeInterfaceQuery>(),
            interface_major: 1,
            min_minor: 10,
            min_size: size_of::<CaptureApiV1_10>() as u32,
        };
        let mut output = std::ptr::null();
        // SAFETY: fixture factory/query/caller/output remain live for bind.
        assert_eq!(
            unsafe {
                factory.api.bind.unwrap()(
                    factory.api.context,
                    BorrowedBytes::new(b"video"),
                    &query,
                    &mut output,
                )
            },
            STATUS_OK
        );
        Self(output.cast())
    }
    fn api(&self) -> &CaptureApiV1_10 {
        // SAFETY: binding owns this table until Drop.
        unsafe { &*self.0 }
    }
    fn context(&self) -> *mut c_void {
        self.api().base.base.base.base.context
    }
}
impl Drop for Binding {
    fn drop(&mut self) {
        let api = self.api(); // SAFETY: the fixture retires all synchronous callbacks before its one release.
        unsafe { api.base.base.base.base.release.unwrap()(api.base.base.base.base.context) };
    }
}
unsafe extern "C" fn copy(context: *mut c_void, frame: *const CaptureResourceFrame) -> Status {
    // SAFETY: public CPU adapter validates and copies this borrowed frame. The
    // consumer retains independent pixels, without provider-private knowledge.
    let result = unsafe { BorrowedResourceFrame::from_descriptor(&*frame) }.copy_cpu();
    match result {
        Ok(frame) => {
            unsafe { *context.cast::<Option<OwnedResourceCpuFrame>>() = Some(frame) };
            STATUS_OK
        }
        Err(e) => e,
    }
}
unsafe extern "C" fn count_offer(
    context: *mut c_void,
    offer: *const CaptureResourceOffer,
) -> Status {
    // SAFETY: factory lends a complete V2 offer through this callback only.
    let offer = unsafe { &*offer };
    assert_eq!(offer.resource.version, 2);
    let caps = unsafe { &*offer.resource.data.cast::<CpuResourceCapabilitiesV2>() };
    assert_eq!(caps.base.format, PIXEL_P010);
    assert_eq!(caps.color.version, 1);
    unsafe { *context.cast::<usize>() += 1 };
    STATUS_OK
}
#[test]
fn sdr_and_hdr_keep_ten_bits_through_public_resource_negotiation_and_close() {
    for hdr in [false, true] {
        let state = Arc::new(State {
            valid: AtomicBool::new(true),
            opens: AtomicUsize::new(0),
            drops: AtomicUsize::new(0),
            hdr,
        });
        let controller = Arc::new(Mutex::new(Controller::new(
            TestBackend(state.clone()),
            true,
        )));
        let source = SourceFormat {
            bit_depth: 10,
            dynamic_range: if hdr {
                DynamicRange::Hdr
            } else {
                DynamicRange::Sdr
            },
        };
        let open = || Operation::Open {
            output: "exact-owned-target".into(),
            operation_id: Some("capture".into()),
            source_format: Some(source),
        };
        let reply = controller
            .lock()
            .unwrap()
            .handle("video", open(), &mut || Ok(()));
        assert_eq!(reply["ok"], true, "{reply}");
        let stream = reply["stream"].as_u64().unwrap();
        assert_eq!(
            controller
                .lock()
                .unwrap()
                .handle("video", open(), &mut || Ok(())),
            reply
        );
        assert_eq!(state.opens.load(Ordering::SeqCst), 1);
        let changed = Operation::Open {
            output: "exact-owned-target".into(),
            operation_id: Some("capture".into()),
            source_format: None,
        };
        assert_eq!(
            controller
                .lock()
                .unwrap()
                .handle("video", changed, &mut || Ok(()))["error"]["code"],
            "operation_conflict"
        );
        let factory = native::Factory::new(controller.clone());
        let binding = Binding::new(&factory);
        let api = binding.api();
        let c = binding.context();
        let mut generation = 0;
        let mut count = 0usize;
        // SAFETY: tables and all borrowed output contexts live through callbacks.
        unsafe {
            assert_eq!(
                (api.base.enumerate_resources)(
                    c,
                    stream,
                    CaptureResourceOfferSink {
                        context: (&mut count as *mut usize).cast(),
                        write: count_offer
                    },
                    &mut generation
                ),
                STATUS_OK
            );
            assert_eq!(count, 2);
            let mut provenance = 0;
            assert_eq!(
                (api.color_provenance)(c, stream, &mut provenance),
                STATUS_OK
            );
            assert_eq!(provenance, CAPTURE_COLOR_PER_PRESENTATION_AUTHORITATIVE);
            let mut owned = None::<OwnedResourceCpuFrame>;
            let sink = CaptureResourceFrameSink {
                context: (&mut owned as *mut Option<OwnedResourceCpuFrame>).cast(),
                write: copy,
            };
            for (id, range) in [(1, FRAME_RANGE_LIMITED), (2, FRAME_RANGE_FULL)] {
                assert_eq!(
                    (api.base.select_resource)(c, stream, generation, id),
                    STATUS_OK
                );
                assert_eq!((api.base.read_resource_frame)(c, stream, sink), STATUS_OK);
                let frame = owned.as_ref().unwrap();
                assert_eq!(frame.color.range, range);
                assert_eq!(
                    frame.color.transfer,
                    if hdr {
                        FRAME_TRANSFER_PQ
                    } else {
                        FRAME_TRANSFER_BT709
                    }
                );
                let sample = u16::from_le_bytes(frame.pixels.pixels()[..2].try_into().unwrap());
                assert_eq!(sample & 63, 0);
                assert_ne!(
                    (sample >> 6) % 4,
                    0,
                    "source precision must survive beyond eight-bit steps"
                );
            }
            let mut legacy = Vec::<u8>::new();
            unsafe extern "C" fn old_sink(_: *mut c_void, _: *const CpuFrame) -> Status {
                panic!("high-depth source must not silently clip into a legacy BGRA callback")
            }
            assert_eq!(
                (api.base.base.base.read_frame)(
                    c,
                    stream,
                    FrameSink {
                        context: (&mut legacy as *mut Vec<u8>).cast(),
                        write: old_sink
                    }
                ),
                MEDIA_UNSUPPORTED
            );
            controller.lock().unwrap().close("video", stream).unwrap();
            assert_eq!(state.drops.load(Ordering::SeqCst), 1);
            assert_eq!(
                (api.base.read_resource_frame)(c, stream, sink),
                MEDIA_CLOSED
            );
            assert_eq!(owned.unwrap().pixels.pixels().len(), 12);
        }
    }
}
#[test]
fn explicit_range_mismatch_precedes_effects_and_native_color_loss_retires_source() {
    let state = Arc::new(State {
        valid: AtomicBool::new(true),
        opens: AtomicUsize::new(0),
        drops: AtomicUsize::new(0),
        hdr: true,
    });
    let controller = Arc::new(Mutex::new(Controller::new(
        TestBackend(state.clone()),
        true,
    )));
    let request = |hdr| Operation::Open {
        output: "owned".into(),
        operation_id: None,
        source_format: Some(SourceFormat {
            bit_depth: if hdr { 10 } else { 8 },
            dynamic_range: if hdr {
                DynamicRange::Hdr
            } else {
                DynamicRange::Sdr
            },
        }),
    };
    let mut guarded = 0;
    let reply = controller
        .lock()
        .unwrap()
        .handle("video", request(false), &mut || {
            guarded += 1;
            Ok(())
        });
    assert_eq!(reply["error"]["code"], "source_range_mismatch");
    assert_eq!(guarded, 0);
    assert_eq!(state.opens.load(Ordering::SeqCst), 0);
    let reply = controller
        .lock()
        .unwrap()
        .handle("video", request(true), &mut || Ok(()));
    let stream = reply["stream"].as_u64().unwrap();
    let factory = native::Factory::new(controller.clone());
    let binding = Binding::new(&factory);
    state.valid.store(false, Ordering::SeqCst);
    let mut flags = 0;
    // SAFETY: binding and flags are retained through the synchronous query.
    assert_eq!(
        unsafe { (binding.api().color_provenance)(binding.context(), stream, &mut flags) },
        MEDIA_CLOSED
    );
    assert_eq!(state.drops.load(Ordering::SeqCst), 1);
}
