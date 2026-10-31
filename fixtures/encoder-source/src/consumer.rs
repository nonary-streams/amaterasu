//! Synchronous CPU consumer for the legacy fixture encoder output queue.
use crate::native::Shared;
use amaterasu_abi::*;
use amaterasu_media::*;
use std::{
    collections::HashSet,
    ffi::c_void,
    mem::size_of,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::Mutex,
};

pub struct Registration {
    pub factory: NativeServiceFactory,
    _context: Box<Shared>,
}
impl Registration {
    pub fn new(shared: Shared) -> Self {
        let mut context = Box::new(shared);
        Self {
            factory: NativeServiceFactory {
                header: AbiHeader::of::<NativeServiceFactory>(),
                context: (&mut *context as *mut Shared).cast(),
                bind: Some(bind),
            },
            _context: context,
        }
    }
}
struct Binding {
    api: FrameResourceConsumerApi,
    owner: String,
    shared: Shared,
    configured: Mutex<HashSet<u64>>,
}
pub(crate) struct Selection {
    generation: u64,
    id: u64,
    format: u32,
    epoch: [u8; 16],
    origin: u64,
    width: u32,
    height: u32,
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
fn is_none(value: &FrameDescriptor) -> bool {
    value.kind == FRAME_SYNC_NONE
        && value.version == 0
        && value.size == 0
        && value.reserved == 0
        && value.namespace == [0; 16]
        && value.data.is_null()
}
fn cpu_descriptor(value: &FrameDescriptor, size: usize) -> bool {
    value.kind == FRAME_RESOURCE_CPU
        && value.version == 1
        && value.size as usize >= size
        && value.reserved == 0
        && value.namespace == [0; 16]
        && !value.data.is_null()
}
unsafe extern "C" fn bind(
    context: *mut c_void,
    principal: BorrowedBytes,
    query: *const NativeInterfaceQuery,
    output: *mut *const NativeInterface,
) -> Status {
    guarded(|| {
        if context.is_null() || query.is_null() || output.is_null() {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: runtime lends valid query, output, context and principal bytes.
        unsafe {
            *output = std::ptr::null();
            let q = &*query;
            if !q.header.supports::<NativeInterfaceQuery>()
                || q.interface_major != FRAME_RESOURCE_CONSUMER_INTERFACE_MAJOR
                || q.min_minor > FRAME_RESOURCE_CONSUMER_INTERFACE_MINOR
                || q.min_size > FRAME_RESOURCE_CONSUMER_API_V1_0_SIZE
            {
                return MEDIA_UNSUPPORTED;
            }
            let Some(owner) = principal
                .read()
                .ok()
                .and_then(|b| std::str::from_utf8(b).ok())
                .filter(|s| !s.is_empty() && s.len() <= 128)
            else {
                return STATUS_INVALID_ARGUMENT;
            };
            let mut binding = Box::new(Binding {
                api: FrameResourceConsumerApi {
                    base: NativeInterface {
                        header: AbiHeader {
                            size: FRAME_RESOURCE_CONSUMER_API_V1_0_SIZE,
                            major: FRAME_RESOURCE_CONSUMER_INTERFACE_MAJOR,
                            minor: FRAME_RESOURCE_CONSUMER_INTERFACE_MINOR,
                        },
                        context: std::ptr::null_mut(),
                        release: Some(release),
                    },
                    enumerate_resources: enumerate,
                    configure,
                    submit,
                    cancel,
                },
                owner: owner.into(),
                shared: (&*context.cast::<Shared>()).clone(),
                configured: Mutex::new(HashSet::new()),
            });
            binding.api.base.context = (&mut *binding as *mut Binding).cast();
            let binding = Box::into_raw(binding);
            *output = &(*binding).api.base;
        }
        STATUS_OK
    })
}
unsafe extern "C" fn release(context: *mut c_void) {
    let _ = guarded(|| {
        if !context.is_null() {
            // SAFETY: binding is released once after all native calls have finished.
            let binding = unsafe { Box::from_raw(context.cast::<Binding>()) };
            let ids = binding
                .configured
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let mut state = binding
                .shared
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            for id in ids.iter() {
                if state.authorized(&binding.owner, *id) {
                    state.consumers.remove(id);
                }
            }
        }
        STATUS_OK
    });
}
unsafe extern "C" fn enumerate(
    context: *mut c_void,
    id: u64,
    sink: CaptureResourceOfferSink,
    generation: *mut u64,
) -> Status {
    guarded(|| {
        if context.is_null() || generation.is_null() {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: binding and output pointer live during this call.
        let binding = unsafe { &*context.cast::<Binding>() };
        let Ok(state) = binding.shared.try_lock() else {
            return MEDIA_BUSY;
        };
        if state.submitted_dimensions(&binding.owner, id).is_none() {
            return MEDIA_CLOSED;
        }
        // SAFETY: caller supplies a writable generation pointer.
        unsafe {
            *generation = 1;
        }
        for (offer_id, format) in [(1, PIXEL_BGRA8), (2, PIXEL_NV12)] {
            let capabilities = CpuResourceCapabilities {
                format,
                min_width: 16,
                max_width: 3840,
                min_height: 16,
                max_height: 2160,
            };
            let offer = CaptureResourceOffer {
                id: offer_id,
                ownership: FRAME_OWNERSHIP_BORROWED,
                accessibility: FRAME_ACCESS_CPU,
                reserved: 0,
                resource: FrameDescriptor {
                    kind: FRAME_RESOURCE_CPU,
                    version: 1,
                    size: size_of::<CpuResourceCapabilities>() as u32,
                    reserved: 0,
                    namespace: [0; 16],
                    data: (&capabilities as *const CpuResourceCapabilities).cast(),
                },
                acquire_sync: none(),
                completion_sync: none(),
            };
            // SAFETY: offer/capabilities and sink live for synchronous callback.
            let status = unsafe { (sink.write)(sink.context, &offer) };
            if status != STATUS_OK {
                return status;
            }
        }
        STATUS_OK
    })
}
unsafe extern "C" fn configure(
    context: *mut c_void,
    id: u64,
    consumer_generation: u64,
    consumer_id: u64,
    producer_generation: u64,
    producer: *const CaptureResourceOffer,
) -> Status {
    guarded(|| {
        if context.is_null() || producer.is_null() {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: binding and producer offer are borrowed for this call.
        let (binding, offer) = unsafe { (&*context.cast::<Binding>(), &*producer) };
        let format = match consumer_id {
            1 => PIXEL_BGRA8,
            2 => PIXEL_NV12,
            _ => return MEDIA_UNSUPPORTED,
        };
        if consumer_generation != 1
            || producer_generation == 0
            || offer.id == 0
            || offer.reserved != 0
            || !matches!(
                offer.ownership,
                FRAME_OWNERSHIP_BORROWED | FRAME_OWNERSHIP_RETAINABLE
            )
            || offer.accessibility & FRAME_ACCESS_CPU == 0
            || !cpu_descriptor(&offer.resource, size_of::<CpuResourceCapabilities>())
            || !is_none(&offer.acquire_sync)
            || !is_none(&offer.completion_sync)
        {
            return MEDIA_UNSUPPORTED;
        }
        // SAFETY: producer offers a readable capabilities struct during this call; unaligned access supported.
        let cap = unsafe {
            std::ptr::read_unaligned(offer.resource.data.cast::<CpuResourceCapabilities>())
        };
        let mut ids = binding
            .configured
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut state = binding
            .shared
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some((width, height)) = state.submitted_dimensions(&binding.owner, id) else {
            return MEDIA_CLOSED;
        };
        let Some(clock) = state.submitted_clock(&binding.owner, id) else {
            return MEDIA_UNSUPPORTED;
        };
        if cap.format != format
            || width < cap.min_width
            || width > cap.max_width
            || height < cap.min_height
            || height > cap.max_height
        {
            return MEDIA_UNSUPPORTED;
        }
        state.consumers.insert(
            id,
            Selection {
                generation: producer_generation,
                id: offer.id,
                format,
                epoch: clock.epoch,
                origin: clock.origin,
                width,
                height,
            },
        );
        ids.insert(id);
        STATUS_OK
    })
}
unsafe extern "C" fn submit(
    context: *mut c_void,
    id: u64,
    frame: *const CaptureResourceFrame,
    flags: u32,
) -> Status {
    guarded(|| {
        if context.is_null() || frame.is_null() {
            return STATUS_INVALID_ARGUMENT;
        }
        if flags & !FRAME_RESOURCE_SUBMIT_FORCE_IDR != 0 {
            return MEDIA_UNSUPPORTED;
        }
        // SAFETY: binding and frame descriptor remain live throughout this call.
        let (binding, frame) = unsafe { (&*context.cast::<Binding>(), &*frame) };
        let Ok(mut state) = binding.shared.try_lock() else {
            return MEDIA_BUSY;
        };
        if state.submitted_dimensions(&binding.owner, id).is_none() {
            return MEDIA_CLOSED;
        }
        let Some(selection) = state.consumers.get(&id) else {
            return MEDIA_CLOSED;
        };
        if frame.offer_generation != selection.generation
            || frame.offer_id != selection.id
            || frame.width != selection.width
            || frame.height != selection.height
            || frame.reserved != 0
            || !(frame.color.version == 0 && frame.color.size == 0
                || frame.color.version == 1
                    && frame.color.size >= size_of::<FrameColorMetadata>() as u32)
            || !cpu_descriptor(&frame.resource, size_of::<CpuFrame>())
            || !is_none(&frame.acquire_sync)
            || frame.clock.version != 1
            || frame.clock.size < size_of::<FrameClockMetadata>() as u32
            || frame.clock.domain != FRAME_CLOCK_HOST_MONOTONIC
            || frame.clock.reserved != 0
            || frame.clock.epoch_id != selection.epoch
            || frame.sequence == 0
            || frame.sequence
                <= state
                    .last_resource_sequence(&binding.owner, id)
                    .unwrap_or(u64::MAX)
            || frame.timestamp_us < selection.origin
        {
            return MEDIA_UNSUPPORTED;
        }
        // SAFETY: descriptor points to a readable CpuFrame until submit returns.
        let mut cpu = unsafe { std::ptr::read_unaligned(frame.resource.data.cast::<CpuFrame>()) };
        if cpu.format != selection.format
            || cpu.width != frame.width
            || cpu.height != frame.height
            || cpu.timestamp_us != frame.timestamp_us
            || cpu.sequence != frame.sequence
            || cpu.validate().is_err()
        {
            return STATUS_INVALID_ARGUMENT;
        }
        cpu.timestamp_us -= selection.origin;
        let status = state.submit_resource(
            &binding.owner,
            id,
            &cpu,
            u32::from(flags & FRAME_RESOURCE_SUBMIT_FORCE_IDR != 0),
        );
        if status == STATUS_OK {
            state.record_resource_sequence(&binding.owner, id, frame.sequence);
        }
        status
    })
}
unsafe extern "C" fn cancel(context: *mut c_void, id: u64) {
    let _ = guarded(|| {
        if context.is_null() {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: runtime keeps binding alive through cancellation.
        let binding = unsafe { &*context.cast::<Binding>() };
        let mut ids = binding
            .configured
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut state = binding
            .shared
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.authorized(&binding.owner, id) && ids.remove(&id) {
            state.consumers.remove(&id);
        }
        STATUS_OK
    });
}
fn guarded(f: impl FnOnce() -> Status) -> Status {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or(STATUS_INTERNAL_ERROR)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::State;
    use serde_json::json;
    use std::sync::Arc;
    unsafe extern "C" fn write_offer(
        context: *mut c_void,
        offer: *const CaptureResourceOffer,
    ) -> Status {
        // SAFETY: test sink and offered descriptor are live during callback.
        let (ids, offer) = unsafe { (&mut *context.cast::<Vec<u64>>(), &*offer) };
        ids.push(offer.id);
        STATUS_OK
    }
    unsafe extern "C" fn retain(_: *mut c_void) -> Status {
        panic!("fixture must not retain submitted frame")
    }
    unsafe extern "C" fn release_frame(_: *mut c_void, _: *const FrameDescriptor) {
        panic!("fixture must not release borrowed frame")
    }
    #[derive(Default)]
    struct Packets {
        flags: Vec<u32>,
        timestamps: Vec<u64>,
    }
    unsafe extern "C" fn packet(context: *mut c_void, packet: *const EncodedPacket) -> Status {
        // SAFETY: state invokes callback synchronously with valid packet and sink.
        let (sink, packet) = unsafe { (&mut *context.cast::<Packets>(), &*packet) };
        sink.flags.push(packet.flags);
        sink.timestamps.push(packet.timestamp_us);
        STATUS_OK
    }
    #[test]
    fn cpu_offer_submission_is_owned_bounded_and_caller_scoped() {
        let shared = Arc::new(Mutex::new(State::default()));
        let reply=shared.lock().unwrap().control("alice",json!({"action":"open","encoder":{"clsid":"example.fake-h264","hardware_url":null,"friendly_name":"Example fake H.264"},"configuration":{"width":16,"height":16,"fps":60,"bitrate":1000000},"operation_id":"generic","resource_output_clock":{"domain":"stream_relative","epoch_id":"01010101010101010101010101010101","host_origin_us":100},"resource_callback_contract":"cross_service_safe"})).0;
        let id = reply["session_id"].as_u64().unwrap();
        let registration = Registration::new(shared.clone());
        let query = NativeInterfaceQuery {
            header: AbiHeader::of::<NativeInterfaceQuery>(),
            interface_major: FRAME_RESOURCE_CONSUMER_INTERFACE_MAJOR,
            min_minor: 0,
            min_size: FRAME_RESOURCE_CONSUMER_API_V1_0_SIZE,
        };
        let mut pointers = Vec::new();
        for name in ["alice", "bob"] {
            let mut pointer = std::ptr::null();
            // SAFETY: all factory inputs remain live through bind.
            assert_eq!(
                unsafe {
                    registration.factory.bind.unwrap()(
                        registration.factory.context,
                        BorrowedBytes::new(name.as_bytes()),
                        &query,
                        &mut pointer,
                    )
                },
                STATUS_OK
            );
            pointers.push(pointer);
        }
        // SAFETY: both returned bindings remain live until released below.
        let alice = unsafe { &*pointers[0].cast::<FrameResourceConsumerApi>() };
        // SAFETY: second binding has identical negotiated type and lifetime.
        let bob = unsafe { &*pointers[1].cast::<FrameResourceConsumerApi>() };
        let mut ids: Vec<u64> = Vec::new();
        let mut generation = 0;
        let capabilities = CpuResourceCapabilities {
            format: PIXEL_BGRA8,
            min_width: 16,
            max_width: 100,
            min_height: 16,
            max_height: 100,
        };
        let offer = CaptureResourceOffer {
            id: 44,
            ownership: FRAME_OWNERSHIP_BORROWED,
            accessibility: FRAME_ACCESS_CPU,
            reserved: 0,
            resource: FrameDescriptor {
                kind: FRAME_RESOURCE_CPU,
                version: 1,
                size: size_of::<CpuResourceCapabilities>() as u32,
                reserved: 0,
                namespace: [0; 16],
                data: (&capabilities as *const CpuResourceCapabilities).cast(),
            },
            acquire_sync: none(),
            completion_sync: none(),
        };
        let pixels = vec![0; 16 * 16 * 4];
        let cpu = CpuFrame {
            width: 16,
            height: 16,
            format: PIXEL_BGRA8,
            stride: 64,
            data: pixels.as_ptr(),
            len: pixels.len(),
            timestamp_us: 123,
            sequence: 7,
        };
        let mut frame = CaptureResourceFrame {
            offer_generation: 4,
            offer_id: 44,
            timestamp_us: 123,
            capture_pacing_timestamp_us: 0,
            sequence: 7,
            width: 16,
            height: 16,
            freshness: 0,
            reserved: 0,
            color: FrameColorMetadata::default(),
            clock: FrameClockMetadata {
                version: 1,
                size: size_of::<FrameClockMetadata>() as u32,
                domain: FRAME_CLOCK_HOST_MONOTONIC,
                reserved: 0,
                epoch_id: [1; 16],
            },
            pacing_clock: FrameClockMetadata::default(),
            resource: FrameDescriptor {
                kind: FRAME_RESOURCE_CPU,
                version: 1,
                size: size_of::<CpuFrame>() as u32,
                reserved: 0,
                namespace: [0; 16],
                data: (&cpu as *const CpuFrame).cast(),
            },
            acquire_sync: none(),
            context: std::ptr::null_mut(),
            retain,
            release: release_frame,
        };
        // SAFETY: bindings, all borrowed offers, frames, data and sinks remain live for these synchronous calls.
        unsafe {
            let sink = CaptureResourceOfferSink {
                context: (&mut ids as *mut Vec<u64>).cast(),
                write: write_offer,
            };
            assert_eq!(
                (bob.enumerate_resources)(bob.base.context, id, sink, &mut generation),
                MEDIA_CLOSED
            );
            assert_eq!(
                (alice.enumerate_resources)(alice.base.context, id, sink, &mut generation),
                STATUS_OK
            );
            assert_eq!((generation, ids), (1, vec![1, 2]));
            assert_eq!(
                (bob.configure)(bob.base.context, id, 1, 1, 4, &offer),
                MEDIA_CLOSED
            );
            assert_eq!(
                (alice.configure)(alice.base.context, id, 1, 1, 4, &offer),
                STATUS_OK
            );
            assert_eq!((bob.submit)(bob.base.context, id, &frame, 0), MEDIA_CLOSED);
            assert_eq!(
                (alice.submit)(alice.base.context, id, &frame, 2),
                MEDIA_UNSUPPORTED
            );
            frame.clock.epoch_id = [0; 16];
            assert_eq!(
                (alice.submit)(alice.base.context, id, &frame, 0),
                MEDIA_UNSUPPORTED
            );
            frame.clock.epoch_id = [1; 16];
            assert_eq!(
                (alice.submit)(
                    alice.base.context,
                    id,
                    &frame,
                    FRAME_RESOURCE_SUBMIT_FORCE_IDR
                ),
                STATUS_OK
            );
            frame.clock.epoch_id = [2; 16];
            assert_eq!(
                (alice.submit)(alice.base.context, id, &frame, 0),
                MEDIA_UNSUPPORTED
            );
            frame.clock.epoch_id = [1; 16];
            assert_eq!(
                (alice.submit)(alice.base.context, id, &frame, 0),
                MEDIA_UNSUPPORTED
            );
            let cpu2 = CpuFrame { sequence: 8, ..cpu };
            frame.sequence = 8;
            frame.resource.data = (&cpu2 as *const CpuFrame).cast();
            assert_eq!((alice.submit)(alice.base.context, id, &frame, 0), STATUS_OK);
            let cpu3 = CpuFrame { sequence: 9, ..cpu };
            frame.sequence = 9;
            frame.resource.data = (&cpu3 as *const CpuFrame).cast();
            assert_eq!(
                (alice.submit)(alice.base.context, id, &frame, 0),
                MEDIA_BUSY
            );
            assert_eq!(shared.lock().unwrap().drain("alice", id), STATUS_OK);
            let mut packets = Packets::default();
            let sink = PacketSink {
                context: (&mut packets as *mut Packets).cast(),
                write: packet,
            };
            assert_eq!(shared.lock().unwrap().receive("alice", id, sink), STATUS_OK);
            assert_eq!(shared.lock().unwrap().receive("alice", id, sink), STATUS_OK);
            assert_eq!(packets.flags[0] & FLAG_KEYFRAME, FLAG_KEYFRAME);
            assert_eq!(packets.flags[1] & FLAG_KEYFRAME, 0);
            assert_eq!(packets.timestamps, vec![23, 23]);
            (alice.cancel)(alice.base.context, id);
            (alice.cancel)(alice.base.context, id);
            assert_eq!(
                (alice.submit)(alice.base.context, id, &frame, 0),
                MEDIA_CLOSED
            );
            assert_eq!(shared.lock().unwrap().receive("alice", id, sink), STATUS_OK);
            assert_eq!(*packets.flags.last().unwrap(), FLAG_END_OF_STREAM);
            for pointer in pointers {
                ((*pointer).release.unwrap())((*pointer).context);
            }
        }
    }
}
