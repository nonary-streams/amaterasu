//! A negotiated transport that carries up to three frame bytes per datagram.
use super::*;
use std::cell::{Cell, RefCell};

pub(super) const SHARD_PAYLOAD: usize = 3;
const HEADER_BYTES: usize = 1;
pub(super) const FRAMER_MARKER: u8 = 3;

pub(super) struct Transport {
    pub(super) factory: NativeServiceFactory,
    pub(super) datagrams: RefCell<Vec<Vec<u8>>>,
    stream: u64,
    selected: Cell<Option<VideoPlanSelection>>,
    busy: Cell<bool>,
}

impl Transport {
    pub(super) fn new(stream: u64) -> Box<Self> {
        let mut transport = Box::new(Self {
            factory: NativeServiceFactory {
                header: AbiHeader::of::<NativeServiceFactory>(),
                context: std::ptr::null_mut(),
                bind: Some(bind),
            },
            datagrams: RefCell::default(),
            stream,
            selected: Cell::new(None),
            busy: Cell::new(true),
        });
        transport.factory.context = (&mut *transport as *mut Self).cast();
        transport
    }
}

const LIMITS: VideoFramingLimits = VideoFramingLimits {
    size: size_of::<VideoFramingLimits>() as u32,
    version: VIDEO_FRAMING_LIMITS_VERSION,
    packet_size: 1024,
    crypto_overhead: 0,
    network_overhead: 28,
    allowed_extensions: 0,
    max_datagrams: 4,
    max_queue_bytes: 64,
    min_parity_shards: 0,
    max_fec_percentage: 0,
    max_wire_bytes: 128,
    min_fec_percentage: 0,
    max_fec_blocks: 1,
    fec_policy_flags: 0,
    reserved: 0,
};

#[repr(C)]
struct Binding {
    api: NegotiatedVideoPlanTransportApiV2,
    transport: *const Transport,
}

unsafe extern "C" fn release(context: *mut c_void) {
    // SAFETY: the runtime releases each binding allocated by bind once.
    drop(unsafe { Box::from_raw(context.cast::<Binding>()) });
}

unsafe extern "C" fn bind(
    context: *mut c_void,
    principal: BorrowedBytes,
    query: *const NativeInterfaceQuery,
    output: *mut *const NativeInterface,
) -> Status {
    // SAFETY: runtime borrows the principal and query for this bind call.
    if unsafe { principal.read() }.ok() != Some(b"consumer") {
        return STATUS_INVALID_ARGUMENT;
    }
    let query = unsafe { &*query };
    if !query.header.supports::<NativeInterfaceQuery>()
        || query.interface_major != 2
        || query.min_minor > 0
        || query.min_size as usize > size_of::<NegotiatedVideoPlanTransportApiV2>()
    {
        return STATUS_INVALID_ARGUMENT;
    }
    let mut table = Box::new(Binding {
        api: NegotiatedVideoPlanTransportApiV2 {
            base: NativeInterface {
                header: AbiHeader {
                    size: size_of::<NegotiatedVideoPlanTransportApiV2>() as u32,
                    major: 2,
                    minor: 0,
                },
                context: std::ptr::null_mut(),
                release: Some(release),
            },
            probe,
            prepare,
            submit,
            drain,
        },
        transport: context.cast(),
    });
    table.api.base.context = (&mut *table as *mut Binding).cast();
    // SAFETY: runtime provides writable output and owns the binding until release.
    unsafe { *output = Box::into_raw(table).cast() };
    STATUS_OK
}

unsafe extern "C" fn probe(
    context: *mut c_void,
    stream: u64,
    selection: *const VideoPlanSelectionV2,
    output: *mut VideoFramingLimits,
) -> Status {
    // SAFETY: binding, fixture transport, and caller descriptors outlive this call.
    let (transport, selection) = unsafe { (&*(*context.cast::<Binding>()).transport, &*selection) };
    if transport.stream != stream
        || selection.wire_name() != Some(b"TEST9")
        || selection.payload_type != 99
    {
        return MEDIA_UNSUPPORTED;
    }
    unsafe { *output = LIMITS };
    STATUS_OK
}

unsafe extern "C" fn prepare(
    context: *mut c_void,
    stream: u64,
    selection: *const VideoPlanSelectionV2,
    output: *mut VideoFramingLimits,
) -> Status {
    // SAFETY: forwards the same live caller descriptors to probe.
    let status = unsafe { probe(context, stream, selection, output) };
    if status == STATUS_OK {
        let (transport, selection) =
            unsafe { (&*(*context.cast::<Binding>()).transport, &*selection) };
        transport.selected.set(selection.base_v1());
    }
    status
}

unsafe extern "C" fn submit(
    context: *mut c_void,
    stream: u64,
    unit: *const NegotiatedVideoUnit,
    plan: *const VideoFramePlan,
) -> Status {
    // SAFETY: the binding and both caller descriptors outlive this call.
    let (transport, unit, plan) =
        unsafe { (&*(*context.cast::<Binding>()).transport, &*unit, &*plan) };
    let Some(selected) = transport.selected.get() else {
        return MEDIA_UNSUPPORTED;
    };
    if transport.stream != stream
        || unit
            .validate_for(
                selected.stream,
                selected.generation,
                selected.offer_id,
                selected.codec,
                selected.framing,
            )
            .is_err()
        || unit.source_id != selected.source_id
        || unit.config_generation != selected.config_generation
        || unit.clock.epoch_id != selected.clock_epoch
    {
        return STATUS_INVALID_ARGUMENT;
    }
    let Ok(counts) = (unsafe { plan.validate_shape(&LIMITS) }) else {
        return STATUS_INVALID_ARGUMENT;
    };
    let shard_count = plan.framed_bytes.len.div_ceil(SHARD_PAYLOAD);
    let queue_bytes = plan.framed_bytes.len + HEADER_BYTES * shard_count;
    let wire_bytes = queue_bytes + LIMITS.network_overhead as usize * shard_count;
    if shard_count != counts.data_shards as usize
        || queue_bytes > LIMITS.max_queue_bytes as usize
        || wire_bytes as u64 > LIMITS.max_wire_bytes
    {
        return STATUS_INVALID_ARGUMENT;
    }
    if transport.busy.replace(false) {
        return MEDIA_BUSY;
    }
    // SAFETY: shape validation bounded the caller-owned synchronous buffer.
    let bytes = unsafe { plan.framed_bytes.read() }.unwrap();
    transport
        .datagrams
        .borrow_mut()
        .extend(
            bytes
                .chunks(SHARD_PAYLOAD)
                .enumerate()
                .map(|(index, shard)| {
                    let mut datagram = vec![index as u8];
                    datagram.extend_from_slice(shard);
                    datagram
                }),
        );
    STATUS_OK
}

unsafe extern "C" fn drain(_: *mut c_void, _: u64) -> Status {
    STATUS_OK
}
