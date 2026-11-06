use super::*;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
struct Provider {
    open: AtomicBool,
    generation: AtomicU64,
    reads: AtomicU64,
}
impl CpuSourceProvider for Provider {
    fn describe(&self, caller: &str, stream: u64) -> Result<CpuSourceDescription, Status> {
        if caller != "video" || stream != 7 || !self.open.load(Ordering::SeqCst) {
            return Err(MEDIA_CLOSED);
        }
        Ok(CpuSourceDescription {
            generation: self.generation.load(Ordering::SeqCst),
            width: 2,
            height: 2,
            formats: vec![CpuSourceFormat {
                pixel_format: PIXEL_P010,
                color: crate::capture::high_depth::p010_color(
                    true,
                    crate::capture::high_depth::YuvRange::Limited,
                ),
            }],
            hdr: Some(HdrMetadata {
                enabled: 1,
                ..HdrMetadata::default()
            }),
            authoritative_color: true,
        })
    }
    fn read(
        &self,
        caller: &str,
        stream: u64,
        format: &CpuSourceFormat,
    ) -> Result<Option<CpuSourcePresentation>, Status> {
        self.describe(caller, stream)?;
        self.reads.fetch_add(1, Ordering::SeqCst);
        Ok(Some(CpuSourcePresentation {
            width: 2,
            height: 2,
            stride: 4,
            pixel_format: format.pixel_format,
            color: format.color,
            bytes: Arc::new(vec![0; 12]),
            timestamp_us: 123,
            capture_pacing_timestamp_us: 120,
            sequence: 8,
            freshness: CAPTURE_FRAME_FRESH,
        }))
    }
    fn geometry(&self, _: &str, _: u64) -> Result<CaptureGeometry, Status> {
        Err(MEDIA_UNSUPPORTED)
    }
}
fn provider() -> Arc<Provider> {
    Arc::new(Provider {
        open: AtomicBool::new(true),
        generation: AtomicU64::new(1),
        reads: AtomicU64::new(0),
    })
}
unsafe fn bind_table(factory: &CpuSourceFactory<Provider>, caller: &str) -> *const CaptureApiV1_10 {
    let query = NativeInterfaceQuery {
        header: AbiHeader::of::<NativeInterfaceQuery>(),
        interface_major: 1,
        min_minor: 10,
        min_size: size_of::<CaptureApiV1_10>() as u32,
    };
    let mut output = std::ptr::null();
    // SAFETY: factory, caller/query and output live through this synchronous call.
    assert_eq!(
        unsafe {
            factory.api.bind.unwrap()(
                factory.api.context,
                BorrowedBytes::new(caller.as_bytes()),
                &query,
                &mut output,
            )
        },
        STATUS_OK
    );
    output.cast()
}
unsafe extern "C" fn offer_sink(
    context: *mut c_void,
    offer: *const CaptureResourceOffer,
) -> Status {
    // SAFETY: test owns context, and factory lends descriptor and payload.
    let out = unsafe { &mut *context.cast::<Vec<(u64, u32, [u32; 6])>>() };
    let offer = unsafe { &*offer };
    assert_eq!(offer.ownership, FRAME_OWNERSHIP_BORROWED);
    assert_eq!(offer.resource.version, 2);
    let caps = unsafe { &*offer.resource.data.cast::<CpuResourceCapabilitiesV2>() };
    out.push((offer.id, caps.base.format, color_tuple(caps.color)));
    STATUS_OK
}
unsafe extern "C" fn frame_sink(
    context: *mut c_void,
    frame: *const CaptureResourceFrame,
) -> Status {
    // SAFETY: callback receives borrowed descriptors; copy only the CPU bytes.
    let frame = unsafe { &*frame };
    let cpu = unsafe { &*frame.resource.data.cast::<CpuFrame>() };
    assert_eq!(frame.resource.version, 2);
    assert_eq!(frame.offer_generation, 1);
    assert_eq!(frame.offer_id, 1);
    assert_eq!(frame.color.transfer, FRAME_TRANSFER_PQ);
    assert_eq!(unsafe { (frame.retain)(frame.context) }, MEDIA_UNSUPPORTED);
    let out = unsafe { &mut *context.cast::<Vec<u8>>() };
    out.extend_from_slice(unsafe { std::slice::from_raw_parts(cpu.data, cpu.len) });
    MEDIA_BUSY
}
#[test]
fn negotiated_color_lifetime_sink_backpressure_and_generation_are_preserved() {
    let p = provider();
    let factory = CpuSourceFactory::new(p.clone());
    // SAFETY: table remains retained until explicit release after all callbacks.
    unsafe {
        let table = bind_table(&factory, "video");
        let api = &*table;
        let c = api.base.base.base.base.context;
        let mut generation = 0;
        let mut offers = Vec::<(u64, u32, [u32; 6])>::new();
        assert_eq!(
            (api.base.enumerate_resources)(
                c,
                7,
                CaptureResourceOfferSink {
                    context: (&mut offers as *mut Vec<_>).cast(),
                    write: offer_sink
                },
                &mut generation
            ),
            STATUS_OK
        );
        assert_eq!(generation, 1);
        assert_eq!(offers.len(), 1);
        assert_eq!(offers[0].1, PIXEL_P010);
        assert_eq!((api.base.select_resource)(c, 7, 1, 1), STATUS_OK);
        let mut copied = Vec::<u8>::new();
        let sink = CaptureResourceFrameSink {
            context: (&mut copied as *mut Vec<u8>).cast(),
            write: frame_sink,
        };
        assert_eq!((api.base.read_resource_frame)(c, 7, sink), MEDIA_BUSY);
        assert_eq!(copied, vec![0; 12]);
        p.generation.store(2, Ordering::SeqCst);
        assert_eq!(
            (api.base.read_resource_frame)(c, 7, sink),
            MEDIA_UNSUPPORTED
        );
        assert_eq!(p.reads.load(Ordering::SeqCst), 1);
        assert_eq!((api.base.select_resource)(c, 7, 1, 1), MEDIA_UNSUPPORTED);
        p.open.store(false, Ordering::SeqCst);
        assert_eq!((api.base.read_resource_frame)(c, 7, sink), MEDIA_CLOSED);
        api.base.base.base.base.release.unwrap()(c);
        // Consumer copy survives native source and binding retirement.
        assert_eq!(copied.len(), 12);
    }
}
#[test]
fn bindings_use_authenticated_owner_and_do_not_invent_legacy_eight_bit_hdr() {
    let p = provider();
    let factory = CpuSourceFactory::new(p);
    // SAFETY: live test tables are released exactly once.
    unsafe {
        let table = bind_table(&factory, "other");
        let api = &*table;
        let c = api.base.base.base.base.context;
        let mut generation = 0;
        assert_eq!(
            (api.base.enumerate_resources)(
                c,
                7,
                CaptureResourceOfferSink {
                    context: std::ptr::null_mut(),
                    write: offer_sink
                },
                &mut generation
            ),
            MEDIA_CLOSED
        );
        api.base.base.base.base.release.unwrap()(c);
        let table = bind_table(&factory, "video");
        let api = &*table;
        let c = api.base.base.base.base.context;
        assert!(matches!(
            (&*c.cast::<Binding<Provider>>()).legacy(7),
            Err(MEDIA_UNSUPPORTED)
        ));
        api.base.base.base.base.release.unwrap()(c);
    }
}
