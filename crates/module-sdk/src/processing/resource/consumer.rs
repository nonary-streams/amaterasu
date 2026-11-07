use super::{format::*, *};
use crate::{
    DynamicRange,
    abi::*,
    capture::{ConnectionError, CpuSourceFormat, SourceFormat},
};
use amaterasu_media::*;
use std::{ffi::c_void, marker::PhantomData, rc::Rc};

#[derive(Clone)]
pub struct CpuProcessingTarget {
    pub width: u32,
    pub height: u32,
    pub formats: Vec<CpuSourceFormat>,
}
#[derive(Clone, Copy)]
struct Selection {
    source_generation: u64,
    source: CpuProcessingOffer,
    output_generation: u64,
    output: CpuProcessingOffer,
}
struct Collected {
    ids: std::collections::BTreeSet<u64>,
    offers: Vec<CpuProcessingOffer>,
    invalid: bool,
}
unsafe extern "C" fn collect(context: *mut c_void, offer: *const CaptureResourceOffer) -> Status {
    if context.is_null() {
        return STATUS_INVALID_ARGUMENT;
    }
    let state = unsafe { &mut *context.cast::<Collected>() };
    if offer.is_null() || !(offer as usize).is_multiple_of(align_of::<CaptureResourceOffer>()) {
        state.invalid = true;
        return STATUS_INVALID_ARGUMENT;
    }
    let offer = unsafe { &*offer };
    if offer.id == 0 || state.ids.len() >= MAX_PROCESSING_OFFERS || !state.ids.insert(offer.id) {
        state.invalid = true;
        return STATUS_INVALID_ARGUMENT;
    }
    if let Ok(offer) = unsafe { read_offer(offer) } {
        state.offers.push(offer);
    }
    STATUS_OK
}
fn enumerate(
    call: impl FnOnce(CaptureResourceOfferSink, &mut u64) -> Status,
) -> Result<(u64, Vec<CpuProcessingOffer>), ConnectionError> {
    let mut collected = Collected {
        ids: Default::default(),
        offers: Vec::new(),
        invalid: false,
    };
    let mut generation = 0;
    let status = call(
        CaptureResourceOfferSink {
            context: std::ptr::from_mut(&mut collected).cast(),
            write: collect,
        },
        &mut generation,
    );
    if collected.invalid || generation == 0 && status == STATUS_OK {
        return Err(ConnectionError::Protocol);
    }
    if status != STATUS_OK {
        return Err(ConnectionError::Native(status));
    }
    Ok((generation, collected.offers))
}
fn source_matches(caps: CpuResourceCapabilitiesV2, source: SourceFormat) -> bool {
    let color = caps.color;
    let depth = if caps.base.format == PIXEL_P010 {
        10
    } else {
        8
    };
    depth == source.bit_depth
        && match source.dynamic_range {
            DynamicRange::Hdr => {
                color.primaries == FRAME_PRIMARIES_BT2020
                    && color.transfer == FRAME_TRANSFER_PQ
                    && color.matrix == FRAME_MATRIX_BT2020_NCL
            }
            DynamicRange::Sdr => {
                color.primaries == FRAME_PRIMARIES_BT709
                    && matches!(color.transfer, FRAME_TRANSFER_BT709 | FRAME_TRANSFER_SRGB)
            }
        }
}
fn target_matches(caps: CpuResourceCapabilitiesV2, target: &CpuProcessingTarget) -> bool {
    target.width >= caps.base.min_width
        && target.width <= caps.base.max_width
        && target.height >= caps.base.min_height
        && target.height <= caps.base.max_height
        && target.formats.iter().any(|format| {
            format.pixel_format == caps.base.format && tuple(format.color) == tuple(caps.color)
        })
}
/// One bounded synchronous source→processor→output connection. It stores no
/// pixels or work queue. Drop cancels the processor before its binding may be
/// released; callbacks and independent output copies retire synchronously.
pub struct CpuResourceProcessingSession {
    processor: *const FrameResourceProcessorApi,
    capture_stream: u64,
    stream: u64,
    source: SourceFormat,
    target: CpuProcessingTarget,
    output: Option<(u64, CpuProcessingOffer)>,
    selected: Option<Selection>,
    active: bool,
    retired: bool,
    module_thread: PhantomData<Rc<()>>,
}
impl CpuResourceProcessingSession {
    /// # Safety
    /// The table must be bound under the exact FRAME_RESOURCE_PROCESSOR family
    /// and outlive this session and its drop/cancellation, on its owning module
    /// thread. Keep the provider loaded through synchronous retirement.
    pub unsafe fn new(
        processor: &FrameResourceProcessorApi,
        capture_stream: u64,
        stream: u64,
        source: SourceFormat,
        target: CpuProcessingTarget,
    ) -> Result<Self, ConnectionError> {
        if processor.base.base.header.major != FRAME_RESOURCE_PROCESSOR_INTERFACE_MAJOR
            || processor.base.base.header.size < FRAME_RESOURCE_PROCESSOR_API_V1_0_SIZE
            || capture_stream == 0
            || stream == 0
            || source.validate().is_err()
            || target.width == 0
            || target.height == 0
            || target.width > 8192
            || target.height > 8192
            || target.formats.is_empty()
            || target.formats.len() > MAX_PROCESSING_OFFERS
        {
            return Err(ConnectionError::Protocol);
        }
        for format in &target.formats {
            if !known(CpuResourceCapabilitiesV2 {
                base: CpuResourceCapabilities {
                    format: format.pixel_format,
                    min_width: target.width,
                    max_width: target.width,
                    min_height: target.height,
                    max_height: target.height,
                },
                color: format.color,
            }) {
                return Err(ConnectionError::Protocol);
            }
        }
        Ok(Self {
            processor,
            capture_stream,
            stream,
            source,
            target,
            output: None,
            selected: None,
            active: false,
            retired: false,
            module_thread: PhantomData,
        })
    }
    pub fn is_retired(&self) -> bool {
        self.retired
    }
    /// Negotiated output color belongs to the processor, never the capture.
    /// Static mastering metadata is unspecified by the v1.0 processor ABI.
    pub fn output_format(&self) -> Option<CpuSourceFormat> {
        self.selected.map(|selected| CpuSourceFormat {
            pixel_format: selected.output.capabilities.base.format,
            color: selected.output.capabilities.color,
        })
    }
    pub fn cancel(&mut self) {
        if self.active {
            // SAFETY: constructor pins this exact provider through cancellation.
            let processor = unsafe { &*self.processor };
            unsafe { (processor.base.cancel)(processor.base.base.context, self.stream) };
            self.active = false;
        }
        self.output = None;
        self.selected = None;
        self.retired = true;
    }
    /// # Safety
    /// Capture must be the authentic selected Capture-family binding, and its
    /// stream, frame descriptors and pixels remain live for this synchronous
    /// operation. The callback must finish accesses before returning.
    pub unsafe fn forward(
        &mut self,
        capture: &CaptureApiV1_8,
        mut output: impl FnMut(&CaptureResourceFrame) -> Status,
    ) -> Result<Status, ConnectionError> {
        if self.retired {
            return Err(ConnectionError::Stale);
        }
        if self.selected.is_none()
            && let Err(error) = unsafe { self.negotiate(capture) }
        {
            if !matches!(error, ConnectionError::Native(MEDIA_BUSY | MEDIA_PENDING)) {
                self.cancel();
            }
            return Err(error);
        }
        let selected = self.selected.expect("negotiated selection");
        let mut state = Delivery {
            processor: unsafe { &*self.processor },
            stream: self.stream,
            selected,
            target: &self.target,
            output: &mut output,
            calls: 0,
            result: STATUS_OK,
            invalid: false,
        };
        let sink = CaptureResourceFrameSink {
            context: std::ptr::from_mut(&mut state).cast(),
            write: source_frame,
        };
        let status = unsafe {
            (capture.read_resource_frame)(capture.base.base.base.context, self.capture_stream, sink)
        };
        if state.invalid
            || state.calls > 1
            || state.calls == 0 && status == STATUS_OK
            || state.calls == 1 && state.result != status
        {
            self.cancel();
            return Err(ConnectionError::Protocol);
        }
        if status == MEDIA_UNSUPPORTED {
            self.cancel();
            return Err(ConnectionError::Stale);
        }
        if !matches!(status, STATUS_OK | MEDIA_BUSY | MEDIA_PENDING) {
            self.cancel();
            return Err(ConnectionError::Native(status));
        }
        Ok(status)
    }
    unsafe fn negotiate(&mut self, capture: &CaptureApiV1_8) -> Result<(), ConnectionError> {
        let processor = unsafe { &*self.processor };
        let context = processor.base.base.context;
        let (generation, output) = if let Some(output) = self.output {
            output
        } else {
            let (generation, outputs) = enumerate(|sink, generation| unsafe {
                (processor.enumerate_outputs)(context, self.stream, sink, generation)
            })?;
            // Ordered target formats preserve the consumer's explicit precision
            // preference; no output format or color is inferred from the input.
            let output = self
                .target
                .formats
                .iter()
                .find_map(|format| {
                    outputs.iter().find(|offer| {
                        offer.capabilities.base.format == format.pixel_format
                            && tuple(offer.capabilities.color) == tuple(format.color)
                            && target_matches(offer.capabilities, &self.target)
                    })
                })
                .copied()
                .ok_or(ConnectionError::NoCompatibleOffer)?;
            self.active = true;
            let status =
                unsafe { (processor.select_output)(context, self.stream, generation, output.id) };
            if status != STATUS_OK {
                // The ABI does not promise that an unsuccessful output selection
                // had no effect. Retire this exact stream before any retry.
                self.cancel();
                return Err(ConnectionError::Native(status));
            }
            self.output = Some((generation, output));
            (generation, output)
        };
        let (input_generation, inputs) = enumerate(|sink, generation| unsafe {
            (processor.base.enumerate_resources)(context, self.stream, sink, generation)
        })?;
        let (source_generation, sources) = enumerate(|sink, generation| unsafe {
            (capture.enumerate_resources)(
                capture.base.base.base.context,
                self.capture_stream,
                sink,
                generation,
            )
        })?;
        for source in sources
            .into_iter()
            .filter(|offer| source_matches(offer.capabilities, self.source))
        {
            for input in inputs
                .iter()
                .filter(|offer| compatible(source.capabilities, offer.capabilities))
            {
                let status = with_offer(source, |offer| unsafe {
                    (processor.base.configure)(
                        context,
                        self.stream,
                        input_generation,
                        input.id,
                        source_generation,
                        offer,
                    )
                });
                if status == MEDIA_UNSUPPORTED {
                    continue;
                }
                if status != STATUS_OK {
                    // Only UNSUPPORTED guarantees that the previous input
                    // configuration was left unchanged. A transient result can
                    // still have acquired work that cancellation must retire.
                    self.cancel();
                    return Err(ConnectionError::Native(status));
                }
                let status = unsafe {
                    (capture.select_resource)(
                        capture.base.base.base.context,
                        self.capture_stream,
                        source_generation,
                        source.id,
                    )
                };
                if status != STATUS_OK {
                    return Err(ConnectionError::Native(status));
                }
                self.selected = Some(Selection {
                    source_generation,
                    source,
                    output_generation: generation,
                    output,
                });
                return Ok(());
            }
        }
        Err(ConnectionError::NoCompatibleOffer)
    }
}
impl Drop for CpuResourceProcessingSession {
    fn drop(&mut self) {
        self.cancel();
    }
}
struct Delivery<'a> {
    processor: &'a FrameResourceProcessorApi,
    stream: u64,
    selected: Selection,
    target: &'a CpuProcessingTarget,
    output: &'a mut dyn FnMut(&CaptureResourceFrame) -> Status,
    calls: u32,
    result: Status,
    invalid: bool,
}
struct Output<'a, 'b> {
    delivery: &'a mut Delivery<'b>,
    source: &'a CaptureResourceFrame,
    calls: u32,
    result: Status,
    invalid: bool,
}
unsafe extern "C" fn source_frame(
    context: *mut c_void,
    frame: *const CaptureResourceFrame,
) -> Status {
    if context.is_null() {
        return STATUS_INVALID_ARGUMENT;
    }
    let delivery = unsafe { &mut *context.cast::<Delivery<'_>>() };
    delivery.calls += 1;
    if frame.is_null()
        || !(frame as usize).is_multiple_of(align_of::<CaptureResourceFrame>())
        || delivery.calls != 1
    {
        delivery.invalid = true;
        return STATUS_INVALID_ARGUMENT;
    }
    let frame = unsafe { &*frame };
    let selection = delivery.selected;
    if frame.offer_generation != selection.source_generation
        || frame.offer_id != selection.source.id
        || unsafe { input(frame, selection.source.capabilities) }.is_err()
    {
        delivery.invalid = true;
        return STATUS_INVALID_ARGUMENT;
    }
    let processor = delivery.processor;
    let stream = delivery.stream;
    let mut output = Output {
        delivery,
        source: frame,
        calls: 0,
        result: STATUS_OK,
        invalid: false,
    };
    let sink = CaptureResourceFrameSink {
        context: std::ptr::from_mut(&mut output).cast(),
        write: processed_frame,
    };
    let status = unsafe { (processor.process)(processor.base.base.context, stream, frame, sink) };
    if output.invalid
        || output.calls > 1
        || output.calls == 0 && status == STATUS_OK
        || output.calls == 1 && output.result != status
    {
        output.delivery.invalid = true;
        return STATUS_INVALID_ARGUMENT;
    }
    output.delivery.result = status;
    status
}
unsafe extern "C" fn processed_frame(
    context: *mut c_void,
    frame: *const CaptureResourceFrame,
) -> Status {
    if context.is_null() {
        return STATUS_INVALID_ARGUMENT;
    }
    let output = unsafe { &mut *context.cast::<Output<'_, '_>>() };
    output.calls += 1;
    if frame.is_null()
        || !(frame as usize).is_multiple_of(align_of::<CaptureResourceFrame>())
        || output.calls != 1
    {
        output.invalid = true;
        return STATUS_INVALID_ARGUMENT;
    }
    let frame = unsafe { &*frame };
    let selection = output.delivery.selected;
    let source = output.source;
    if frame.offer_generation != selection.output_generation
        || frame.offer_id != selection.output.id
        || frame.width != output.delivery.target.width
        || frame.height != output.delivery.target.height
        || frame.sequence != source.sequence
        || frame.timestamp_us != source.timestamp_us
        || frame.capture_pacing_timestamp_us != source.capture_pacing_timestamp_us
        || frame.freshness != source.freshness
        || !same_clock(frame.clock, source.clock)
        || !same_clock(frame.pacing_clock, source.pacing_clock)
        || unsafe { input(frame, selection.output.capabilities) }.is_err()
    {
        output.invalid = true;
        return STATUS_INVALID_ARGUMENT;
    }
    output.result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        (output.delivery.output)(frame)
    }))
    .unwrap_or_else(|_| {
        output.invalid = true;
        STATUS_INTERNAL_ERROR
    });
    output.result
}
fn same_clock(a: FrameClockMetadata, b: FrameClockMetadata) -> bool {
    a.version == b.version
        && a.size == b.size
        && a.domain == b.domain
        && a.reserved == b.reserved
        && a.epoch_id == b.epoch_id
}
