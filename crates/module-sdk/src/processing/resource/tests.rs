use super::*;
use crate::{DynamicRange, abi::*, capture::*, media::OwnedCpuFrame};
use amaterasu_media::*;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

#[derive(Default)]
struct Counts {
    reads: AtomicUsize,
    processes: AtomicUsize,
    cancels: AtomicUsize,
    shutdowns: AtomicUsize,
}
fn color(hdr: bool, rgb: bool) -> FrameColorMetadata {
    FrameColorMetadata {
        version: 1,
        size: size_of::<FrameColorMetadata>() as u32,
        primaries: if hdr {
            FRAME_PRIMARIES_BT2020
        } else {
            FRAME_PRIMARIES_BT709
        },
        transfer: if hdr {
            FRAME_TRANSFER_PQ
        } else if rgb {
            FRAME_TRANSFER_SRGB
        } else {
            FRAME_TRANSFER_BT709
        },
        matrix: if hdr {
            FRAME_MATRIX_BT2020_NCL
        } else if rgb {
            FRAME_MATRIX_RGB
        } else {
            FRAME_MATRIX_BT709
        },
        range: if rgb {
            FRAME_RANGE_FULL
        } else {
            FRAME_RANGE_LIMITED
        },
    }
}
fn offer(format: u32, hdr: bool) -> CpuProcessingOffer {
    CpuProcessingOffer {
        id: 1,
        capabilities: CpuResourceCapabilitiesV2 {
            base: CpuResourceCapabilities {
                format,
                min_width: 16,
                max_width: 16,
                min_height: 16,
                max_height: 16,
            },
            color: color(hdr, format == PIXEL_BGRA8),
        },
    }
}
struct Source(Arc<Counts>);
impl CpuSourceProvider for Source {
    fn describe(&self, _: &str, _: u64) -> Result<CpuSourceDescription, Status> {
        Ok(CpuSourceDescription {
            generation: 1,
            width: 16,
            height: 16,
            formats: vec![CpuSourceFormat {
                pixel_format: PIXEL_P010,
                color: color(true, false),
            }],
            hdr: None,
            authoritative_color: true,
        })
    }
    fn read(
        &self,
        _: &str,
        _: u64,
        format: &CpuSourceFormat,
    ) -> Result<Option<CpuSourcePresentation>, Status> {
        self.0.reads.fetch_add(1, Ordering::SeqCst);
        assert_eq!(format.pixel_format, PIXEL_P010);
        let bytes = (0..16 * 16 * 3 / 2)
            .flat_map(|i| if i < 16 * 16 { 64u16 << 6 } else { 512u16 << 6 }.to_le_bytes())
            .collect();
        Ok(Some(CpuSourcePresentation {
            width: 16,
            height: 16,
            stride: 32,
            pixel_format: PIXEL_P010,
            color: color(true, false),
            bytes: Arc::new(bytes),
            timestamp_us: 3,
            capture_pacing_timestamp_us: 2,
            sequence: 1,
            freshness: CAPTURE_FRAME_FRESH,
        }))
    }
    fn geometry(&self, _: &str, _: u64) -> Result<CaptureGeometry, Status> {
        CaptureGeometry::new(
            DesktopRect::new(0, 0, 16, 16)?,
            DesktopRect::new(0, 0, 16, 16)?,
        )
    }
}
struct Planar {
    counts: Arc<Counts>,
    unsupported: bool,
    wrong_sequence: bool,
}
impl CpuResourceProcessor for Planar {
    fn describe(&self, _: u64) -> Result<CpuProcessingDescription, Status> {
        Ok(CpuProcessingDescription {
            generation: 1,
            inputs: vec![offer(PIXEL_P010, !self.unsupported)],
            outputs: vec![offer(PIXEL_NV12, false)],
        })
    }
    fn configure(
        &mut self,
        _: u64,
        input: CpuResourceCapabilitiesV2,
        output: CpuResourceCapabilitiesV2,
    ) -> Result<(), Status> {
        assert_eq!(input.color.transfer, FRAME_TRANSFER_PQ);
        assert_eq!(output.color.transfer, FRAME_TRANSFER_BT709);
        Ok(())
    }
    fn process(
        &mut self,
        _: u64,
        input: CpuResourceInput<'_>,
        _: CpuResourceCapabilitiesV2,
    ) -> Result<OwnedCpuFrame, Status> {
        self.counts.processes.fetch_add(1, Ordering::SeqCst);
        // Contract fixture: neutral black maps exactly between these spaces;
        // this is not a general HDR tone-mapping implementation.
        let bytes = input
            .pixels
            .pixels()
            .as_chunks::<2>()
            .0
            .iter()
            .map(|sample| (u16::from_le_bytes(*sample) >> 8) as u8)
            .collect();
        let frame = input.pixels.descriptor();
        OwnedCpuFrame::new(
            16,
            16,
            PIXEL_NV12,
            16,
            bytes,
            frame.timestamp_us,
            frame.sequence + u64::from(self.wrong_sequence),
        )
    }
    fn cancel(&mut self, _: u64) {
        self.counts.cancels.fetch_add(1, Ordering::SeqCst);
    }
    fn shutdown(&mut self) {
        self.counts.shutdowns.fetch_add(1, Ordering::SeqCst);
    }
}
struct Rgb(Arc<Counts>);
impl CpuResourceProcessor for Rgb {
    fn describe(&self, _: u64) -> Result<CpuProcessingDescription, Status> {
        Ok(CpuProcessingDescription {
            generation: 7,
            inputs: vec![offer(PIXEL_P010, true)],
            outputs: vec![offer(PIXEL_BGRA8, false)],
        })
    }
    fn configure(
        &mut self,
        _: u64,
        _: CpuResourceCapabilitiesV2,
        _: CpuResourceCapabilitiesV2,
    ) -> Result<(), Status> {
        Ok(())
    }
    fn process(
        &mut self,
        _: u64,
        input: CpuResourceInput<'_>,
        _: CpuResourceCapabilitiesV2,
    ) -> Result<OwnedCpuFrame, Status> {
        self.0.processes.fetch_add(1, Ordering::SeqCst);
        let mut bytes = vec![0u8; 16 * 16 * 4];
        for (pixel, y) in bytes
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip(input.pixels.pixels()[..16 * 16 * 2].as_chunks::<2>().0)
        {
            let code = (u16::from_le_bytes(*y) >> 6).saturating_sub(64);
            let level = (u32::from(code) * 255 / 876).min(255) as u8;
            *pixel = [level, level, level, 255];
        }
        let frame = input.pixels.descriptor();
        OwnedCpuFrame::new(
            16,
            16,
            PIXEL_BGRA8,
            64,
            bytes,
            frame.timestamp_us,
            frame.sequence,
        )
    }
    fn cancel(&mut self, _: u64) {
        self.0.cancels.fetch_add(1, Ordering::SeqCst);
    }
    fn shutdown(&mut self) {
        self.0.shutdowns.fetch_add(1, Ordering::SeqCst);
    }
}
struct Native(*const NativeInterface);
impl Native {
    fn capture(factory: &NativeServiceFactory) -> Self {
        let query = NativeInterfaceQuery {
            header: AbiHeader::of::<NativeInterfaceQuery>(),
            interface_major: 1,
            min_minor: 8,
            min_size: CAPTURE_API_V1_8_SIZE,
        };
        let mut out = std::ptr::null();
        assert_eq!(
            unsafe {
                factory.bind.unwrap()(
                    factory.context,
                    BorrowedBytes::new(b"video"),
                    &query,
                    &mut out,
                )
            },
            STATUS_OK
        );
        Self(out)
    }
    unsafe fn table<T>(&self) -> &T {
        unsafe { &*self.0.cast::<T>() }
    }
}
impl Drop for Native {
    fn drop(&mut self) {
        let api = unsafe { &*self.0 };
        if let Some(release) = api.release {
            unsafe { release(api.context) }
        }
    }
}
fn target() -> CpuProcessingTarget {
    CpuProcessingTarget {
        width: 16,
        height: 16,
        formats: vec![
            CpuSourceFormat {
                pixel_format: PIXEL_NV12,
                color: color(false, false),
            },
            CpuSourceFormat {
                pixel_format: PIXEL_BGRA8,
                color: color(false, true),
            },
        ],
    }
}
fn unchanged_consumer<P: CpuResourceProcessor>(provider: P, counts: &Arc<Counts>, expected: u32) {
    let capture = CpuSourceFactory::new(Arc::new(Source(counts.clone())));
    let capture = Native::capture(&capture.api);
    let processor = Native(into_resource_processor_binding(provider).cast());
    let mut session = unsafe {
        CpuResourceProcessingSession::new(
            processor.table(),
            1,
            2,
            SourceFormat {
                bit_depth: 10,
                dynamic_range: DynamicRange::Hdr,
            },
            target(),
        )
    }
    .unwrap();
    let mut delivered = 0;
    let status = unsafe {
        session.forward(capture.table(), |frame| {
            delivered += 1;
            assert_eq!(
                (
                    frame.sequence,
                    frame.timestamp_us,
                    frame.capture_pacing_timestamp_us
                ),
                (1, 3, 2)
            );
            assert_eq!(
                frame.color.transfer,
                if expected == PIXEL_BGRA8 {
                    FRAME_TRANSFER_SRGB
                } else {
                    FRAME_TRANSFER_BT709
                }
            );
            let cpu = &*frame.resource.data.cast::<CpuFrame>();
            assert_eq!(cpu.format, expected);
            let bytes = cpu.bytes().unwrap();
            if expected == PIXEL_NV12 {
                assert!(bytes[..256].iter().all(|value| *value == 16));
                assert!(bytes[256..].iter().all(|value| *value == 128));
            } else {
                assert!(
                    bytes
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .all(|pixel| *pixel == [0, 0, 0, 255])
                );
            }
            STATUS_OK
        })
    }
    .unwrap();
    assert_eq!(status, STATUS_OK);
    assert_eq!(delivered, 1);
    drop(session);
    assert_eq!(counts.cancels.load(Ordering::SeqCst), 1);
    drop(processor);
    assert_eq!(counts.shutdowns.load(Ordering::SeqCst), 1);
}
#[test]
fn unchanged_resource_consumer_accepts_two_independent_processors_and_real_color_outputs() {
    let planar = Arc::new(Counts::default());
    unchanged_consumer(
        Planar {
            counts: planar.clone(),
            unsupported: false,
            wrong_sequence: false,
        },
        &planar,
        PIXEL_NV12,
    );
    let rgb = Arc::new(Counts::default());
    unchanged_consumer(Rgb(rgb.clone()), &rgb, PIXEL_BGRA8);
}
#[test]
fn unsupported_source_color_is_rejected_before_capture_and_output_identity_cannot_change() {
    for (unsupported, wrong_sequence) in [(true, false), (false, true)] {
        let counts = Arc::new(Counts::default());
        let capture = CpuSourceFactory::new(Arc::new(Source(counts.clone())));
        let capture = Native::capture(&capture.api);
        let processor = Native(
            into_resource_processor_binding(Planar {
                counts: counts.clone(),
                unsupported,
                wrong_sequence,
            })
            .cast(),
        );
        let mut session = unsafe {
            CpuResourceProcessingSession::new(
                processor.table(),
                1,
                2,
                SourceFormat {
                    bit_depth: 10,
                    dynamic_range: DynamicRange::Hdr,
                },
                target(),
            )
        }
        .unwrap();
        assert!(
            unsafe {
                session.forward(capture.table(), |_| {
                    panic!("invalid output must not be delivered")
                })
            }
            .is_err()
        );
        assert_eq!(
            counts.reads.load(Ordering::SeqCst),
            usize::from(!unsupported)
        );
        assert!(session.is_retired());
        drop(session);
        assert_eq!(counts.cancels.load(Ordering::SeqCst), 1);
    }
}

struct Backpressure(Planar, bool);
impl CpuResourceProcessor for Backpressure {
    fn describe(&self, stream: u64) -> Result<CpuProcessingDescription, Status> {
        self.0.describe(stream)
    }
    fn configure(
        &mut self,
        stream: u64,
        input: CpuResourceCapabilitiesV2,
        output: CpuResourceCapabilitiesV2,
    ) -> Result<(), Status> {
        self.0.configure(stream, input, output)
    }
    fn process(
        &mut self,
        stream: u64,
        input: CpuResourceInput<'_>,
        output: CpuResourceCapabilitiesV2,
    ) -> Result<OwnedCpuFrame, Status> {
        if !self.1 {
            self.1 = true;
            Err(MEDIA_BUSY)
        } else {
            self.0.process(stream, input, output)
        }
    }
    fn cancel(&mut self, stream: u64) {
        self.0.cancel(stream);
    }
    fn shutdown(&mut self) {
        self.0.shutdown();
    }
}
#[test]
fn processor_backpressure_keeps_one_borrowed_presentation_and_cancels_once() {
    let counts = Arc::new(Counts::default());
    let capture = CpuSourceFactory::new(Arc::new(Source(counts.clone())));
    let capture = Native::capture(&capture.api);
    let processor = Native(
        into_resource_processor_binding(Backpressure(
            Planar {
                counts: counts.clone(),
                unsupported: false,
                wrong_sequence: false,
            },
            false,
        ))
        .cast(),
    );
    let mut session = unsafe {
        CpuResourceProcessingSession::new(
            processor.table(),
            1,
            2,
            SourceFormat {
                bit_depth: 10,
                dynamic_range: DynamicRange::Hdr,
            },
            target(),
        )
    }
    .unwrap();
    assert_eq!(
        unsafe { session.forward(capture.table(), |_| panic!("busy processor has no output")) }
            .unwrap(),
        MEDIA_BUSY
    );
    assert!(!session.is_retired());
    assert_eq!(counts.cancels.load(Ordering::SeqCst), 0);
    assert_eq!(
        unsafe {
            session.forward(capture.table(), |frame| {
                assert_eq!((frame.sequence, frame.timestamp_us), (1, 3));
                STATUS_OK
            })
        }
        .unwrap(),
        STATUS_OK
    );
    session.cancel();
    drop(session);
    assert_eq!(counts.cancels.load(Ordering::SeqCst), 1);
}

#[test]
fn public_processor_factory_checks_caller_query_and_bounded_stream_admission() {
    let counts = Arc::new(Counts::default());
    let created = Arc::new(AtomicUsize::new(0));
    let factory = ResourceProcessorFactory::new({
        let counts = counts.clone();
        let created = created.clone();
        move |caller: &str| {
            assert_eq!(caller, "video");
            created.fetch_add(1, Ordering::SeqCst);
            Ok(Planar {
                counts: counts.clone(),
                unsupported: false,
                wrong_sequence: false,
            })
        }
    });
    let mut query = NativeInterfaceQuery {
        header: AbiHeader::of::<NativeInterfaceQuery>(),
        interface_major: 1,
        min_minor: 0,
        min_size: FRAME_RESOURCE_PROCESSOR_API_V1_0_SIZE,
    };
    for bad in [1, 2] {
        query.min_minor = bad;
        let mut pointer = std::ptr::dangling();
        assert_eq!(
            unsafe {
                factory.api.bind.unwrap()(
                    factory.api.context,
                    BorrowedBytes::new(b"video"),
                    &query,
                    &mut pointer,
                )
            },
            MEDIA_UNSUPPORTED
        );
        assert!(pointer.is_null());
    }
    assert_eq!(created.load(Ordering::SeqCst), 0);
    query.min_minor = 0;
    let mut pointer = std::ptr::null();
    assert_eq!(
        unsafe {
            factory.api.bind.unwrap()(
                factory.api.context,
                BorrowedBytes::new(b"video"),
                &query,
                &mut pointer,
            )
        },
        STATUS_OK
    );
    let binding = Native(pointer);
    let api: &FrameResourceProcessorApi = unsafe { binding.table() };
    for stream in 1..=MAX_PROCESSING_STREAMS as u64 {
        assert_eq!(
            unsafe { (api.select_output)(api.base.base.context, stream, 1, 1) },
            STATUS_OK
        );
    }
    assert_eq!(
        unsafe {
            (api.select_output)(
                api.base.base.context,
                MAX_PROCESSING_STREAMS as u64 + 1,
                1,
                1,
            )
        },
        MEDIA_BUSY
    );
    for stream in 1..=MAX_PROCESSING_STREAMS as u64 {
        unsafe { (api.base.cancel)(api.base.base.context, stream) };
    }
    assert_eq!(
        counts.cancels.load(Ordering::SeqCst),
        MAX_PROCESSING_STREAMS
    );
    drop(binding);
    assert_eq!(counts.shutdowns.load(Ordering::SeqCst), 1);
}

#[test]
fn panicking_output_consumer_is_contained_and_retires_the_processor() {
    let counts = Arc::new(Counts::default());
    let source = CpuSourceFactory::new(Arc::new(Source(counts.clone())));
    let source = Native::capture(&source.api);
    let processor = Native(
        into_resource_processor_binding(Planar {
            counts: counts.clone(),
            unsupported: false,
            wrong_sequence: false,
        })
        .cast(),
    );
    let mut session = unsafe {
        CpuResourceProcessingSession::new(
            processor.table(),
            1,
            2,
            SourceFormat {
                bit_depth: 10,
                dynamic_range: DynamicRange::Hdr,
            },
            target(),
        )
    }
    .unwrap();
    assert!(unsafe { session.forward(source.table(), |_| panic!("consumer failure")) }.is_err());
    assert!(session.is_retired());
    drop(session);
    assert_eq!(counts.cancels.load(Ordering::SeqCst), 1);
}
