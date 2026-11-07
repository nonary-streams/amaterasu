//! A safe CPU capture producer behind the public capture v1.10 native table.
//! Every frame is borrowed only during its callback; no provider allocation or
//! native capture handle crosses into the consumer contract.
use crate::abi::{
    AbiHeader, BorrowedBytes, NativeInterface, NativeInterfaceQuery, NativeServiceFactory,
    STATUS_INTERNAL_ERROR, STATUS_INVALID_ARGUMENT, STATUS_OK, Status,
};
use amaterasu_media::*;
use std::{
    collections::BTreeMap,
    ffi::c_void,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{Arc, Mutex},
};

#[derive(Clone, Copy)]
pub struct CpuSourceFormat {
    pub pixel_format: u32,
    pub color: FrameColorMetadata,
}
#[derive(Clone)]
pub struct CpuSourceDescription {
    pub generation: u64,
    pub width: u32,
    pub height: u32,
    pub formats: Vec<CpuSourceFormat>,
    pub hdr: Option<HdrMetadata>,
    pub authoritative_color: bool,
}
pub struct CpuSourcePresentation {
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub pixel_format: u32,
    pub color: FrameColorMetadata,
    pub bytes: Arc<Vec<u8>>,
    pub timestamp_us: u64,
    pub capture_pacing_timestamp_us: u64,
    pub sequence: u64,
    pub freshness: u32,
}
/// Implementations authenticate every operation against the caller and retire
/// native work at control close. These methods must never call foreign sinks.
pub trait CpuSourceProvider: Send + Sync {
    fn describe(&self, caller: &str, stream: u64) -> Result<CpuSourceDescription, Status>;
    fn read(
        &self,
        caller: &str,
        stream: u64,
        format: &CpuSourceFormat,
    ) -> Result<Option<CpuSourcePresentation>, Status>;
    /// Legacy callers do not negotiate color. Implementations may preserve an
    /// older eight-bit SDR path even when no resource description is available.
    /// A high-depth-only source must return UNSUPPORTED rather than clip HDR.
    fn read_legacy(
        &self,
        caller: &str,
        stream: u64,
    ) -> Result<Option<CpuSourcePresentation>, Status> {
        let description = validate_description(self.describe(caller, stream)?)?;
        let format = description
            .formats
            .iter()
            .find(|f| f.pixel_format == PIXEL_BGRA8)
            .ok_or(MEDIA_UNSUPPORTED)?;
        self.read(caller, stream, format)
    }
    /// Called after a foreign sink rejects a presentation, except BUSY, which
    /// leaves it retryable. Providers can invalidate a cadence cache here.
    fn delivery_failed(&self, _caller: &str, _stream: u64, _status: Status) {}
    fn geometry(&self, caller: &str, stream: u64) -> Result<CaptureGeometry, Status>;
}
pub struct CpuSourceFactory<P: CpuSourceProvider> {
    pub api: NativeServiceFactory,
    provider: Arc<P>,
}
impl<P: CpuSourceProvider + 'static> CpuSourceFactory<P> {
    pub fn new(provider: Arc<P>) -> Box<Self> {
        let mut factory = Box::new(Self {
            api: NativeServiceFactory {
                header: AbiHeader::of::<NativeServiceFactory>(),
                context: std::ptr::null_mut(),
                bind: Some(bind::<P>),
            },
            provider,
        });
        factory.api.context = (&mut *factory as *mut Self).cast();
        factory
    }
}
#[derive(Clone)]
struct Selection {
    description: CpuSourceDescription,
    index: usize,
}
struct Binding<P: CpuSourceProvider> {
    api: CaptureApiV1_10,
    provider: Arc<P>,
    caller: String,
    selections: Mutex<BTreeMap<u64, Selection>>,
}
fn guarded(f: impl FnOnce() -> Status) -> Status {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or(STATUS_INTERNAL_ERROR)
}
fn color_tuple(c: FrameColorMetadata) -> [u32; 6] {
    [
        c.version,
        c.size,
        c.primaries,
        c.transfer,
        c.matrix,
        c.range,
    ]
}
fn known_color(c: FrameColorMetadata) -> bool {
    c.version == 1
        && c.size == size_of::<FrameColorMetadata>() as u32
        && matches!(c.primaries, FRAME_PRIMARIES_BT709 | FRAME_PRIMARIES_BT2020)
        && matches!(
            c.transfer,
            FRAME_TRANSFER_SRGB | FRAME_TRANSFER_BT709 | FRAME_TRANSFER_PQ | FRAME_TRANSFER_HLG
        )
        && matches!(
            c.matrix,
            FRAME_MATRIX_RGB | FRAME_MATRIX_BT709 | FRAME_MATRIX_BT2020_NCL
        )
        && matches!(c.range, FRAME_RANGE_FULL | FRAME_RANGE_LIMITED)
}
fn same_format(a: CpuSourceFormat, b: CpuSourceFormat) -> bool {
    a.pixel_format == b.pixel_format && color_tuple(a.color) == color_tuple(b.color)
}
fn same_description(a: &CpuSourceDescription, b: &CpuSourceDescription) -> bool {
    a.generation == b.generation
        && a.width == b.width
        && a.height == b.height
        && a.authoritative_color == b.authoritative_color
        && a.hdr == b.hdr
        && a.formats.len() == b.formats.len()
        && a.formats
            .iter()
            .zip(&b.formats)
            .all(|(a, b)| same_format(*a, *b))
}
fn validate_description(d: CpuSourceDescription) -> Result<CpuSourceDescription, Status> {
    if d.generation == 0
        || d.width == 0
        || d.height == 0
        || d.width > 8192
        || d.height > 8192
        || d.formats.is_empty()
        || d.formats.len() > 8
        || d.hdr.is_some_and(|h| h.enabled > 1 || h.reserved != 0)
    {
        return Err(MEDIA_BACKEND_ERROR);
    }
    for (index, f) in d.formats.iter().enumerate() {
        let stride = d
            .width
            .checked_mul(if f.pixel_format == PIXEL_BGRA8 {
                4
            } else if f.pixel_format == PIXEL_P010 {
                2
            } else {
                1
            })
            .ok_or(MEDIA_BACKEND_ERROR)?;
        frame_size(d.width, d.height, f.pixel_format, stride).map_err(|_| MEDIA_BACKEND_ERROR)?;
        if color_tuple(f.color) != [0; 6] && !known_color(f.color)
            || d.formats[..index]
                .iter()
                .any(|other| same_format(*other, *f))
        {
            return Err(MEDIA_BACKEND_ERROR);
        }
    }
    Ok(d)
}
impl<P: CpuSourceProvider> Binding<P> {
    fn describe(&self, stream: u64) -> Result<CpuSourceDescription, Status> {
        validate_description(self.provider.describe(&self.caller, stream)?)
    }
    fn read(
        &self,
        stream: u64,
        d: &CpuSourceDescription,
        index: usize,
    ) -> Result<CpuSourcePresentation, Status> {
        let f = d.formats.get(index).ok_or(MEDIA_UNSUPPORTED)?;
        let p = self
            .provider
            .read(&self.caller, stream, f)?
            .ok_or(MEDIA_PENDING)?;
        let cpu = p.frame();
        cpu.validate().map_err(|_| MEDIA_BACKEND_ERROR)?;
        if p.width != d.width
            || p.height != d.height
            || p.pixel_format != f.pixel_format
            || color_tuple(p.color) != color_tuple(f.color)
            || !matches!(
                p.freshness,
                CAPTURE_FRAME_FRESH | CAPTURE_FRAME_CACHED_REUSE
            )
        {
            return Err(MEDIA_BACKEND_ERROR);
        }
        if !same_description(d, &self.describe(stream)?) {
            return Err(MEDIA_UNSUPPORTED);
        }
        Ok(p)
    }
    fn finish_delivery(&self, stream: u64, status: Status) -> Status {
        if status != STATUS_OK && status != MEDIA_BUSY {
            self.provider.delivery_failed(&self.caller, stream, status);
        }
        status
    }
    fn legacy(&self, stream: u64) -> Result<CpuSourcePresentation, Status> {
        let p = self
            .provider
            .read_legacy(&self.caller, stream)?
            .ok_or(MEDIA_PENDING)?;
        p.frame().validate().map_err(|_| MEDIA_BACKEND_ERROR)?;
        if p.pixel_format != PIXEL_BGRA8
            || !matches!(
                p.freshness,
                CAPTURE_FRAME_FRESH | CAPTURE_FRAME_CACHED_REUSE
            )
        {
            return Err(MEDIA_BACKEND_ERROR);
        }
        Ok(p)
    }
}
impl CpuSourcePresentation {
    fn frame(&self) -> CpuFrame {
        CpuFrame {
            width: self.width,
            height: self.height,
            format: self.pixel_format,
            stride: self.stride,
            data: self.bytes.as_ptr(),
            len: self.bytes.len(),
            timestamp_us: self.timestamp_us,
            sequence: self.sequence,
        }
    }
}
fn none() -> FrameDescriptor {
    FrameDescriptor {
        kind: FRAME_SYNC_NONE,
        version: 0,
        size: 0,
        reserved: 0,
        namespace: [0; 16],
        data: std::ptr::null(),
    }
}
unsafe extern "C" fn bind<P: CpuSourceProvider + 'static>(
    context: *mut c_void,
    caller: BorrowedBytes,
    query: *const NativeInterfaceQuery,
    output: *mut *const NativeInterface,
) -> Status {
    guarded(|| {
        if context.is_null() || query.is_null() || output.is_null() {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: the broker lends aligned query/output and caller storage for this
        // synchronous bind. The registration retains the boxed factory.
        let query = unsafe { &*query };
        unsafe { output.write(std::ptr::null()) };
        if !query.header.supports::<NativeInterfaceQuery>()
            || query.interface_major != INTERFACE_MAJOR
            || query.min_minor > 10
            || query.min_size as usize > size_of::<CaptureApiV1_10>()
        {
            return MEDIA_UNSUPPORTED;
        }
        let caller = match unsafe { caller.read() }
            .ok()
            .and_then(|b| std::str::from_utf8(b).ok())
        {
            Some(c) if !c.is_empty() && c.len() <= 128 && !c.chars().any(char::is_control) => {
                c.to_owned()
            }
            _ => return STATUS_INVALID_ARGUMENT,
        };
        let factory = unsafe { &*context.cast::<CpuSourceFactory<P>>() };
        let mut b = Box::new(Binding {
            api: table::<P>(),
            provider: factory.provider.clone(),
            caller,
            selections: Mutex::new(BTreeMap::new()),
        });
        let context = (&*b as *const Binding<P>).cast_mut().cast();
        b.api.base.base.base.base.context = context;
        let output_table = &b.api.base.base.base.base as *const NativeInterface;
        let _ = Box::into_raw(b);
        unsafe { output.write(output_table) };
        STATUS_OK
    })
}
fn table<P: CpuSourceProvider + 'static>() -> CaptureApiV1_10 {
    CaptureApiV1_10 {
        base: CaptureApiV1_9 {
            base: CaptureApiV1_7 {
                base: CaptureApiV1_6 {
                    base: NativeInterface {
                        header: AbiHeader {
                            size: size_of::<CaptureApiV1_10>() as u32,
                            major: INTERFACE_MAJOR,
                            minor: 10,
                        },
                        context: std::ptr::null_mut(),
                        release: Some(release::<P>),
                    },
                    read_frame: legacy::<P>,
                    geometry: geometry::<P>,
                    read_frame_with_freshness: fresh::<P>,
                    read_frame_with_capture_timestamp: timed::<P>,
                    read_dmabuf_frame: dmabuf,
                    latest_presentation: latest,
                    read_dmabuf_frame_leased: leased,
                },
                subscribe_events: subscribe,
                unsubscribe_events: unsubscribe,
                resolve_swap: resolve,
            },
            enumerate_resources: enumerate::<P>,
            select_resource: select::<P>,
            read_resource_frame: read_resource::<P>,
            hdr_metadata: hdr::<P>,
        },
        color_provenance: provenance::<P>,
    }
}
unsafe extern "C" fn release<P: CpuSourceProvider>(context: *mut c_void) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if !context.is_null() {
            // SAFETY: broker consumes the unique binding once after callbacks retire.
            unsafe { drop(Box::from_raw(context.cast::<Binding<P>>())) };
        }
    }));
}
unsafe fn binding<'a, P: CpuSourceProvider>(
    context: *mut c_void,
) -> Result<&'a Binding<P>, Status> {
    if context.is_null() {
        Err(STATUS_INVALID_ARGUMENT)
    } else {
        // SAFETY: each callback borrows a live broker-owned binding through return.
        Ok(unsafe { &*context.cast::<Binding<P>>() })
    }
}
unsafe extern "C" fn legacy<P: CpuSourceProvider>(
    c: *mut c_void,
    s: u64,
    sink: FrameSink,
) -> Status {
    guarded(|| {
        let b = match unsafe { binding::<P>(c) } {
            Ok(b) => b,
            Err(e) => return e,
        };
        let p = match b.legacy(s) {
            Ok(p) => p,
            Err(e) => return e,
        }; // SAFETY: owned presentation keeps bytes alive through the synchronous sink.
        b.finish_delivery(s, unsafe { sink.send(&p.frame()) })
    })
}
unsafe extern "C" fn fresh<P: CpuSourceProvider>(
    c: *mut c_void,
    s: u64,
    sink: FreshFrameSinkV1_2,
) -> Status {
    guarded(|| {
        let b = match unsafe { binding::<P>(c) } {
            Ok(b) => b,
            Err(e) => return e,
        };
        let p = match b.legacy(s) {
            Ok(p) => p,
            Err(e) => return e,
        }; // SAFETY: owned presentation keeps bytes alive through the synchronous sink.
        b.finish_delivery(s, unsafe { sink.send(&p.frame(), p.freshness) })
    })
}
unsafe extern "C" fn timed<P: CpuSourceProvider>(
    c: *mut c_void,
    s: u64,
    sink: FreshFrameSinkV1_3,
) -> Status {
    guarded(|| {
        let b = match unsafe { binding::<P>(c) } {
            Ok(b) => b,
            Err(e) => return e,
        };
        let p = match b.legacy(s) {
            Ok(p) => p,
            Err(e) => return e,
        }; // SAFETY: owned presentation keeps bytes alive through the synchronous sink.
        b.finish_delivery(s, unsafe {
            sink.send(
                &CaptureFrameV1_3 {
                    frame: p.frame(),
                    capture_pacing_timestamp_us: p.capture_pacing_timestamp_us,
                },
                p.freshness,
            )
        })
    })
}
unsafe extern "C" fn geometry<P: CpuSourceProvider>(
    c: *mut c_void,
    s: u64,
    out: *mut CaptureGeometry,
) -> Status {
    guarded(|| {
        if out.is_null() {
            return STATUS_INVALID_ARGUMENT;
        }
        let b = match unsafe { binding::<P>(c) } {
            Ok(b) => b,
            Err(e) => return e,
        };
        let g = match b.provider.geometry(&b.caller, s) {
            Ok(g) if g.validate().is_ok() => g,
            Ok(_) => return MEDIA_BACKEND_ERROR,
            Err(e) => return e,
        }; // SAFETY: caller supplies writable aligned output for this callback.
        unsafe { out.write(g) };
        STATUS_OK
    })
}
unsafe extern "C" fn enumerate<P: CpuSourceProvider>(
    c: *mut c_void,
    s: u64,
    sink: CaptureResourceOfferSink,
    out: *mut u64,
) -> Status {
    guarded(|| {
        if out.is_null() {
            return STATUS_INVALID_ARGUMENT;
        }
        let b = match unsafe { binding::<P>(c) } {
            Ok(b) => b,
            Err(e) => return e,
        };
        let d = match b.describe(s) {
            Ok(d) => d,
            Err(e) => return e,
        };
        for (index, f) in d.formats.iter().enumerate() {
            let base = CpuResourceCapabilities {
                format: f.pixel_format,
                min_width: d.width,
                max_width: d.width,
                min_height: d.height,
                max_height: d.height,
            };
            let caps = CpuResourceCapabilitiesV2 {
                base,
                color: f.color,
            };
            let known = known_color(f.color);
            let offer = CaptureResourceOffer {
                id: index as u64 + 1,
                ownership: FRAME_OWNERSHIP_BORROWED,
                accessibility: FRAME_ACCESS_CPU,
                reserved: 0,
                resource: FrameDescriptor {
                    kind: FRAME_RESOURCE_CPU,
                    version: if known { 2 } else { 1 },
                    size: if known {
                        size_of::<CpuResourceCapabilitiesV2>()
                    } else {
                        size_of::<CpuResourceCapabilities>()
                    } as u32,
                    reserved: 0,
                    namespace: [0; 16],
                    data: (&caps as *const CpuResourceCapabilitiesV2).cast(),
                },
                acquire_sync: none(),
                completion_sync: none(),
            };
            // SAFETY: offer and capabilities are borrowed until the callback returns.
            let status = unsafe { (sink.write)(sink.context, &offer) };
            if status != STATUS_OK {
                return status;
            }
        }
        let current = match b.describe(s) {
            Ok(current) => current,
            Err(status) => return status,
        };
        if !same_description(&d, &current) {
            return MEDIA_UNSUPPORTED;
        }
        // SAFETY: output is caller-owned, writable and aligned.
        unsafe { out.write(d.generation) };
        STATUS_OK
    })
}
unsafe extern "C" fn select<P: CpuSourceProvider>(
    c: *mut c_void,
    s: u64,
    g: u64,
    id: u64,
) -> Status {
    guarded(|| {
        let b = match unsafe { binding::<P>(c) } {
            Ok(b) => b,
            Err(e) => return e,
        };
        let d = match b.describe(s) {
            Ok(d) => d,
            Err(e) => return e,
        };
        if d.generation != g || id == 0 || id as usize > d.formats.len() {
            return MEDIA_UNSUPPORTED;
        }
        let mut selections = match b.selections.try_lock() {
            Ok(s) => s,
            Err(_) => return MEDIA_BUSY,
        };
        // Prune only independently closed streams; no native provider call runs
        // while a foreign sink is active, and no closed selection retains pixels.
        selections.retain(|stream, _| {
            !matches!(b.provider.describe(&b.caller, *stream), Err(MEDIA_CLOSED))
        });
        if !selections.contains_key(&s) && selections.len() >= 64 {
            return MEDIA_BUSY;
        }
        selections.insert(
            s,
            Selection {
                description: d,
                index: id as usize - 1,
            },
        );
        STATUS_OK
    })
}
unsafe extern "C" fn read_resource<P: CpuSourceProvider>(
    c: *mut c_void,
    s: u64,
    sink: CaptureResourceFrameSink,
) -> Status {
    guarded(|| {
        let b = match unsafe { binding::<P>(c) } {
            Ok(b) => b,
            Err(e) => return e,
        };
        let selection = match b.selections.try_lock() {
            Ok(map) => match map.get(&s) {
                Some(s) => s.clone(),
                None => return MEDIA_UNSUPPORTED,
            },
            Err(_) => return MEDIA_BUSY,
        };
        let d = match b.describe(s) {
            Ok(d) => d,
            Err(e) => return e,
        };
        if !same_description(&d, &selection.description) {
            return MEDIA_UNSUPPORTED;
        }
        let p = match b.read(s, &d, selection.index) {
            Ok(p) => p,
            Err(e) => return e,
        };
        let cpu = p.frame();
        let frame = CaptureResourceFrame {
            offer_generation: d.generation,
            offer_id: selection.index as u64 + 1,
            timestamp_us: p.timestamp_us,
            capture_pacing_timestamp_us: p.capture_pacing_timestamp_us,
            sequence: p.sequence,
            width: p.width,
            height: p.height,
            freshness: p.freshness,
            reserved: 0,
            color: p.color,
            clock: FrameClockMetadata::default(),
            pacing_clock: FrameClockMetadata::default(),
            resource: FrameDescriptor {
                kind: FRAME_RESOURCE_CPU,
                version: if known_color(p.color) { 2 } else { 1 },
                size: size_of::<CpuFrame>() as u32,
                reserved: 0,
                namespace: [0; 16],
                data: (&cpu as *const CpuFrame).cast(),
            },
            acquire_sync: none(),
            context: std::ptr::null_mut(),
            retain: retain_borrowed,
            release: release_borrowed,
        };
        // SAFETY: immutable Arc storage, frame and descriptor live until callback
        // returns. Borrowed-only retention is explicitly rejected.
        b.finish_delivery(s, unsafe { (sink.write)(sink.context, &frame) })
    })
}
unsafe extern "C" fn hdr<P: CpuSourceProvider>(
    c: *mut c_void,
    s: u64,
    out: *mut HdrMetadata,
) -> Status {
    guarded(|| {
        if out.is_null() {
            return STATUS_INVALID_ARGUMENT;
        }
        let b = match unsafe { binding::<P>(c) } {
            Ok(b) => b,
            Err(e) => return e,
        };
        let d = match b.describe(s) {
            Ok(d) => d,
            Err(e) => return e,
        };
        let Some(h) = d.hdr else {
            return MEDIA_UNSUPPORTED;
        }; // SAFETY: caller supplies writable aligned output.
        unsafe { out.write(h) };
        STATUS_OK
    })
}
unsafe extern "C" fn provenance<P: CpuSourceProvider>(
    c: *mut c_void,
    s: u64,
    out: *mut u32,
) -> Status {
    guarded(|| {
        if out.is_null() {
            return STATUS_INVALID_ARGUMENT;
        }
        let b = match unsafe { binding::<P>(c) } {
            Ok(b) => b,
            Err(e) => return e,
        };
        let d = match b.describe(s) {
            Ok(d) => d,
            Err(e) => return e,
        }; // SAFETY: caller supplies writable aligned output.
        unsafe {
            out.write(if d.authoritative_color {
                CAPTURE_COLOR_PER_PRESENTATION_AUTHORITATIVE
            } else {
                0
            })
        };
        STATUS_OK
    })
}
unsafe extern "C" fn retain_borrowed(_: *mut c_void) -> Status {
    MEDIA_UNSUPPORTED
}
unsafe extern "C" fn release_borrowed(_: *mut c_void, _: *const FrameDescriptor) {}
unsafe extern "C" fn dmabuf(_: *mut c_void, _: u64, _: DmaBufFrameSink) -> Status {
    MEDIA_UNSUPPORTED
}
unsafe extern "C" fn leased(_: *mut c_void, _: u64, _: DmaBufLeaseSink) -> Status {
    MEDIA_UNSUPPORTED
}
unsafe extern "C" fn latest(_: *mut c_void, _: u64, _: *mut PresentationStatus) -> Status {
    MEDIA_UNSUPPORTED
}
unsafe extern "C" fn subscribe(_: *mut c_void, _: u64, _: CaptureEventSink) -> Status {
    MEDIA_UNSUPPORTED
}
unsafe extern "C" fn unsubscribe(_: *mut c_void, _: u64) {}
unsafe extern "C" fn resolve(_: *mut c_void, _: u64, _: u64, _: u32, _: BorrowedBytes) -> Status {
    MEDIA_UNSUPPORTED
}
#[cfg(test)]
mod tests;
