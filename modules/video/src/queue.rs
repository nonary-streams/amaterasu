//! Bounded encoded packet ownership shared with native consumer bindings.
use crate::plane::{MetadataCapabilities, MetadataFlow};
use amaterasu_media::{
    ENCODER_CHUNK_ACCESS_UNIT_END, ENCODER_CHUNK_ACCESS_UNIT_START, ENCODER_CHUNK_END_OF_STREAM,
    ENCODER_CHUNK_KEYFRAME, EncodedPacket, EncoderFormatId, EncoderOutputChunk, FLAG_END_OF_STREAM,
    FRAME_CLOCK_STREAM_RELATIVE, FrameClockMetadata, MAX_PACKET_BYTES, MAX_VIDEO_ACCESS_UNIT_BYTES,
    MAX_VIDEO_ACCESS_UNIT_FRAGMENTS, MEDIA_BUSY, MEDIA_CLOSED, MEDIA_PENDING, NegotiatedVideoUnit,
    NegotiatedVideoUnitSink, PacketSink, VIDEO_UNIT_END_OF_STREAM, VIDEO_UNIT_KEYFRAME,
    VIDEO_UNIT_VERSION,
};
use amaterasu_module_sdk::abi::{BorrowedBytes, STATUS_INVALID_ARGUMENT, STATUS_OK, Status};
use amaterasu_module_sdk::media::metadata::{
    EncodedMetadataAssociation, EncodedMetadataSelection, MetadataApplication,
};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{Arc, Mutex},
};

struct Packet {
    bytes: Vec<u8>,
    codec: u32,
    flags: u32,
    timestamp_us: u64,
    sequence: u64,
}

struct Stream {
    owner: String,
    /// Host-clock microseconds at the stream's timestamp origin. Packets
    /// leave the queue stamped on the shared host clock (see
    /// `amaterasu_media::host_monotonic_micros`) so consumers can measure
    /// host latency; inside video they stay relative to the stream's open.
    clock_origin_us: u64,
    geometry: Option<amaterasu_media::CaptureGeometry>,
    hdr: Option<amaterasu_media::HdrMetadata>,
    packets: VecDeque<Packet>,
    bytes: usize,
    eos_queued: bool,
    eos_accepted: bool,
    negotiated: Option<UnitQueue>,
}

/// Authenticated session composition fixes these values before output begins.
#[derive(Clone, Copy)]
pub struct UnitSelection {
    pub stream: u64,
    pub generation: u64,
    pub offer_id: u64,
    pub source_id: u64,
    pub config_generation: u64,
    pub codec: EncoderFormatId,
    pub framing: EncoderFormatId,
    pub clock_epoch: [u8; 16],
}

struct Unit {
    access_unit_id: u64,
    submission_id: u64,
    timestamp_us: u64,
    flags: u32,
    bytes: Vec<u8>,
    fragments: usize,
    metadata: Option<UnitMetadata>,
}

struct MetadataSelection {
    flow: Arc<Mutex<MetadataFlow>>,
    capabilities: MetadataCapabilities,
}

struct UnitMetadata {
    bytes: Vec<u8>,
    association: EncodedMetadataAssociation,
    delivered: bool,
}

struct UnitQueue {
    selection: UnitSelection,
    current: Option<Unit>,
    ready: VecDeque<Unit>,
    bytes: usize,
    eos_queued: bool,
    eos_accepted: bool,
    metadata: Option<MetadataSelection>,
    failed: bool,
}

fn unit_header(selection: UnitSelection, unit: &Unit) -> NegotiatedVideoUnit {
    NegotiatedVideoUnit {
        size: std::mem::size_of::<NegotiatedVideoUnit>() as u32,
        version: VIDEO_UNIT_VERSION,
        stream: selection.stream,
        generation: selection.generation,
        offer_id: selection.offer_id,
        codec: selection.codec,
        framing: selection.framing,
        access_unit_id: unit.access_unit_id,
        submission_id: unit.submission_id,
        source_id: selection.source_id,
        config_generation: selection.config_generation,
        timestamp_us: unit.timestamp_us,
        clock: FrameClockMetadata {
            size: std::mem::size_of::<FrameClockMetadata>() as u32,
            version: 1,
            domain: FRAME_CLOCK_STREAM_RELATIVE,
            reserved: 0,
            epoch_id: selection.clock_epoch,
        },
        flags: unit.flags,
        reserved: 0,
        data: BorrowedBytes::new(&unit.bytes),
    }
}

fn associate_metadata(queue: &UnitQueue, unit: &mut Unit) -> Result<(), Status> {
    let Some(selected) = &queue.metadata else {
        return Ok(());
    };
    let header = unit_header(queue.selection, unit);
    let mut flow = selected.flow.lock().map_err(|_| STATUS_INVALID_ARGUMENT)?;
    let bytes = flow
        .output_bytes(selected.capabilities)
        .map_err(|_| STATUS_INVALID_ARGUMENT)?;
    let mut association = flow
        .claim_output(
            EncodedMetadataSelection {
                stream: header.stream,
                generation: header.generation,
                offer_id: header.offer_id,
                codec: header.codec,
                framing: header.framing,
                clock_epoch: header.clock.epoch_id,
            },
            selected.capabilities,
        )
        .map_err(|_| STATUS_INVALID_ARGUMENT)?;
    association
        .accept_encoder_output(
            &header,
            &bytes,
            MetadataApplication::Forwarded,
            selected.capabilities.encoder,
        )
        .map_err(|_| STATUS_INVALID_ARGUMENT)?;
    unit.metadata = Some(UnitMetadata {
        bytes,
        association,
        delivered: false,
    });
    Ok(())
}

#[derive(Default)]
pub struct Queues {
    streams: BTreeMap<u64, Stream>,
}

impl Queues {
    pub fn clear(&mut self) {
        self.streams.clear();
    }

    pub fn open(&mut self, id: u64, owner: &str, clock_origin_us: u64) {
        self.streams.insert(
            id,
            Stream {
                owner: owner.into(),
                clock_origin_us,
                geometry: None,
                hdr: None,
                packets: VecDeque::new(),
                bytes: 0,
                eos_queued: false,
                eos_accepted: false,
                negotiated: None,
            },
        );
    }

    pub fn set_geometry(&mut self, id: u64, geometry: Option<amaterasu_media::CaptureGeometry>) {
        if let Some(stream) = self.streams.get_mut(&id) {
            stream.geometry = geometry;
        }
    }

    pub fn geometry(
        &self,
        owner: &str,
        id: u64,
    ) -> Result<Option<amaterasu_media::CaptureGeometry>, Status> {
        self.streams
            .get(&id)
            .filter(|stream| stream.owner == owner)
            .map(|stream| stream.geometry)
            .ok_or(MEDIA_CLOSED)
    }

    pub fn set_hdr(&mut self, id: u64, hdr: Option<amaterasu_media::HdrMetadata>) {
        if let Some(stream) = self.streams.get_mut(&id) {
            stream.hdr = hdr;
        }
    }
    pub fn hdr(
        &self,
        owner: &str,
        id: u64,
    ) -> Result<Option<amaterasu_media::HdrMetadata>, Status> {
        self.streams
            .get(&id)
            .filter(|stream| stream.owner == owner)
            .map(|stream| stream.hdr)
            .ok_or(MEDIA_CLOSED)
    }
    pub fn close(&mut self, id: u64) {
        self.streams.remove(&id);
    }

    pub fn configure_units(&mut self, id: u64, selection: UnitSelection) -> Result<(), Status> {
        let stream = self.streams.get_mut(&id).ok_or(MEDIA_CLOSED)?;
        let valid = |id: EncoderFormatId| id.namespace_id != 0 && id.value != 0 && id.schema != 0;
        if selection.stream != id
            || selection.generation == 0
            || selection.offer_id == 0
            || selection.source_id == 0
            || selection.config_generation == 0
            || selection.clock_epoch == [0; 16]
            || !valid(selection.codec)
            || !valid(selection.framing)
            || stream.negotiated.is_some()
            || !stream.packets.is_empty()
            || stream.eos_queued
        {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        stream.negotiated = Some(UnitQueue {
            selection,
            current: None,
            ready: VecDeque::new(),
            bytes: 0,
            eos_queued: false,
            eos_accepted: false,
            metadata: None,
            failed: false,
        });
        Ok(())
    }

    pub(crate) fn configure_metadata(
        &mut self,
        id: u64,
        flow: Arc<Mutex<MetadataFlow>>,
        capabilities: MetadataCapabilities,
    ) -> Result<(), Status> {
        let queue = self
            .streams
            .get_mut(&id)
            .and_then(|stream| stream.negotiated.as_mut())
            .ok_or(MEDIA_CLOSED)?;
        if queue.metadata.is_some() || queue.current.is_some() || !queue.ready.is_empty() {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        queue.metadata = Some(MetadataSelection { flow, capabilities });
        Ok(())
    }

    /// # Safety
    /// The encoder owns initialized chunk bytes throughout this synchronous call.
    pub unsafe fn push_unit_chunk(&mut self, id: u64, chunk: &EncoderOutputChunk) -> Status {
        let Some(queue) = self
            .streams
            .get_mut(&id)
            .and_then(|s| s.negotiated.as_mut())
        else {
            return MEDIA_CLOSED;
        };
        if queue.eos_queued || queue.failed {
            return MEDIA_CLOSED;
        }
        let selection = queue.selection;
        let same = |a: EncoderFormatId, b: EncoderFormatId| {
            a.namespace_id == b.namespace_id && a.value == b.value && a.schema == b.schema
        };
        let flags = chunk.flags;
        let eos = flags == ENCODER_CHUNK_END_OF_STREAM;
        let start = flags & ENCODER_CHUNK_ACCESS_UNIT_START != 0;
        let end = flags & ENCODER_CHUNK_ACCESS_UNIT_END != 0;
        let picture = flags & ENCODER_CHUNK_KEYFRAME != 0;
        if chunk.size < std::mem::size_of::<EncoderOutputChunk>() as u32
            || chunk.source_id != selection.source_id
            || chunk.config_generation != selection.config_generation
            || !same(chunk.codec, selection.codec)
            || !same(chunk.framing, selection.framing)
            || flags
                & !(ENCODER_CHUNK_ACCESS_UNIT_START
                    | ENCODER_CHUNK_ACCESS_UNIT_END
                    | ENCODER_CHUNK_KEYFRAME
                    | ENCODER_CHUNK_END_OF_STREAM)
                != 0
            || (flags & ENCODER_CHUNK_END_OF_STREAM != 0 && !eos)
            || (eos && (chunk.bytes.len != 0 || queue.current.is_some()))
            || (!eos
                && (chunk.access_unit_id == 0
                    || chunk.bytes.len == 0
                    || chunk.bytes.len > MAX_PACKET_BYTES
                    || chunk.bytes.ptr.is_null()))
        {
            return STATUS_INVALID_ARGUMENT;
        }
        if queue
            .current
            .as_ref()
            .map_or(0, |unit| unit.bytes.len())
            .checked_add(chunk.bytes.len)
            .is_none_or(|n| n > MAX_VIDEO_ACCESS_UNIT_BYTES)
        {
            return STATUS_INVALID_ARGUMENT;
        }
        if queue.ready.len() >= 4
            || queue
                .bytes
                .checked_add(chunk.bytes.len)
                .is_none_or(|n| n > MAX_VIDEO_ACCESS_UNIT_BYTES)
        {
            return MEDIA_BUSY;
        }
        match &queue.current {
            Some(unit)
                if eos
                    || start
                    || unit.access_unit_id != chunk.access_unit_id
                    || unit.submission_id != chunk.submission_id
                    || unit.timestamp_us != chunk.timestamp_us
                    || (unit.flags == VIDEO_UNIT_KEYFRAME) != picture
                    || unit.fragments >= MAX_VIDEO_ACCESS_UNIT_FRAGMENTS =>
            {
                return STATUS_INVALID_ARGUMENT;
            }
            None if !eos && !start => return STATUS_INVALID_ARGUMENT,
            _ => {}
        }
        let bytes = if chunk.bytes.len == 0 {
            &[][..]
        } else {
            // SAFETY: caller guarantees this chunk allocation remains live.
            unsafe { std::slice::from_raw_parts(chunk.bytes.ptr, chunk.bytes.len) }
        };
        if eos {
            queue.eos_queued = true;
            queue.ready.push_back(Unit {
                access_unit_id: chunk.access_unit_id,
                submission_id: chunk.submission_id,
                timestamp_us: chunk.timestamp_us,
                flags: VIDEO_UNIT_END_OF_STREAM,
                bytes: Vec::new(),
                fragments: 0,
                metadata: None,
            });
            return STATUS_OK;
        }
        let unit = queue.current.get_or_insert_with(|| Unit {
            access_unit_id: chunk.access_unit_id,
            submission_id: chunk.submission_id,
            timestamp_us: chunk.timestamp_us,
            flags: if picture { VIDEO_UNIT_KEYFRAME } else { 0 },
            bytes: Vec::new(),
            fragments: 0,
            metadata: None,
        });
        unit.bytes.extend_from_slice(bytes);
        unit.fragments += 1;
        queue.bytes += bytes.len();
        if end {
            let mut unit = queue.current.take().expect("unit initialized");
            if associate_metadata(queue, &mut unit).is_err() {
                queue.failed = true;
                return STATUS_INVALID_ARGUMENT;
            }
            queue.ready.push_back(unit);
        }
        STATUS_OK
    }

    /// # Safety
    /// Sink must obey the synchronous borrowed-unit contract and not reenter.
    pub unsafe fn receive_unit(
        &mut self,
        owner: &str,
        id: u64,
        sink: NegotiatedVideoUnitSink,
    ) -> Status {
        let Some(queue) = self
            .streams
            .get_mut(&id)
            .filter(|s| s.owner == owner)
            .and_then(|s| s.negotiated.as_mut())
        else {
            return MEDIA_CLOSED;
        };
        if queue.failed || queue.metadata.is_some() {
            return STATUS_INVALID_ARGUMENT;
        }
        let Some(unit) = queue.ready.front() else {
            return if queue.eos_accepted {
                MEDIA_CLOSED
            } else {
                MEDIA_PENDING
            };
        };
        let header = unit_header(queue.selection, unit);
        // SAFETY: queue retains header and bytes during this synchronous callback.
        let status = unsafe { (sink.write)(sink.context, &header) };
        if status == STATUS_OK {
            queue.eos_accepted = unit.flags == VIDEO_UNIT_END_OF_STREAM;
            queue.bytes -= unit.bytes.len();
            queue.ready.pop_front();
        }
        status
    }

    /// # Safety
    /// Sink borrows the unit and envelope synchronously and must not reenter.
    pub(crate) unsafe fn receive_unit_metadata(
        &mut self,
        owner: &str,
        id: u64,
        sink: amaterasu_media::NegotiatedVideoMetadataUnitSinkV1,
    ) -> Status {
        let Some(queue) = self
            .streams
            .get_mut(&id)
            .filter(|stream| stream.owner == owner)
            .and_then(|stream| stream.negotiated.as_mut())
        else {
            return MEDIA_CLOSED;
        };
        if queue.failed || queue.metadata.is_none() {
            return STATUS_INVALID_ARGUMENT;
        }
        let Some(unit) = queue.ready.front_mut() else {
            return if queue.eos_accepted {
                MEDIA_CLOSED
            } else {
                MEDIA_PENDING
            };
        };
        let header = unit_header(queue.selection, unit);
        if unit
            .metadata
            .as_ref()
            .is_some_and(|metadata| metadata.delivered)
        {
            return MEDIA_BUSY;
        }
        let envelope = unit
            .metadata
            .as_ref()
            .map_or(&[][..], |metadata| metadata.bytes.as_slice());
        if header.flags != VIDEO_UNIT_END_OF_STREAM && envelope.is_empty() {
            return STATUS_INVALID_ARGUMENT;
        }
        let wrapped = amaterasu_media::NegotiatedVideoMetadataUnitV1 {
            size: std::mem::size_of::<amaterasu_media::NegotiatedVideoMetadataUnitV1>() as u32,
            version: 1,
            unit: header,
            metadata: BorrowedBytes::new(envelope),
        };
        // SAFETY: queue owns unit and envelope until the callback returns.
        let status = unsafe { (sink.write)(sink.context, &wrapped) };
        if status == STATUS_OK {
            if let Some(metadata) = unit.metadata.as_mut() {
                metadata.delivered = true;
            } else {
                queue.eos_accepted = true;
                queue.ready.pop_front();
            }
        }
        status
    }

    fn with_metadata_unit<T>(
        &mut self,
        owner: &str,
        id: u64,
        unit_id: u64,
        operation: impl FnOnce(
            &MetadataSelection,
            &NegotiatedVideoUnit,
            &mut UnitMetadata,
        ) -> Result<T, Status>,
    ) -> Result<T, Status> {
        let queue = self
            .streams
            .get_mut(&id)
            .filter(|stream| stream.owner == owner)
            .and_then(|stream| stream.negotiated.as_mut())
            .ok_or(MEDIA_CLOSED)?;
        if queue.failed {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        let selected = queue.metadata.as_ref().ok_or(STATUS_INVALID_ARGUMENT)?;
        if !selected
            .flow
            .lock()
            .map_err(|_| STATUS_INVALID_ARGUMENT)?
            .is_current(selected.capabilities)
        {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        let unit = queue.ready.front_mut().ok_or(MEDIA_PENDING)?;
        let header = unit_header(queue.selection, unit);
        if unit.access_unit_id != unit_id {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        let metadata = unit
            .metadata
            .as_mut()
            .filter(|metadata| metadata.delivered)
            .ok_or(STATUS_INVALID_ARGUMENT)?;
        operation(selected, &header, metadata)
    }

    pub(crate) fn framed_metadata(&mut self, owner: &str, id: u64, unit: u64) -> Status {
        self.with_metadata_unit(owner, id, unit, |selected, header, metadata| {
            metadata
                .association
                .accept_framer_output(
                    header,
                    &metadata.bytes,
                    MetadataApplication::FramerApplied,
                    selected.capabilities.encoder,
                )
                .map_err(|_| STATUS_INVALID_ARGUMENT)
        })
        .map_or_else(|status| status, |_| STATUS_OK)
    }

    /// Preserved v1.1 compatibility operation. New required-metadata routes
    /// must use `admit_metadata`, which combines this check with admission.
    pub(crate) fn check_metadata(&mut self, owner: &str, id: u64, unit: u64) -> Status {
        self.with_metadata_unit(owner, id, unit, |selected, header, metadata| {
            metadata
                .association
                .check_transport(header, &metadata.bytes, selected.capabilities.encoder)
                .map_err(|_| STATUS_INVALID_ARGUMENT)
        })
        .map_or_else(|status| status, |_| STATUS_OK)
    }

    /// Preserved v1.1 compatibility retirement operation. Its split check and
    /// retire calls cannot establish an atomic transport admission boundary.
    pub(crate) fn retire_metadata(&mut self, owner: &str, id: u64, unit: u64) -> Status {
        let result = self.with_metadata_unit(owner, id, unit, |selected, header, metadata| {
            let mut flow = selected.flow.lock().map_err(|_| STATUS_INVALID_ARGUMENT)?;
            flow.retire(
                &mut metadata.association,
                header,
                &metadata.bytes,
                selected.capabilities,
            )
            .map_err(|_| STATUS_INVALID_ARGUMENT)
        });
        if let Err(status) = result {
            return status;
        }
        let Some(queue) = self
            .streams
            .get_mut(&id)
            .and_then(|stream| stream.negotiated.as_mut())
        else {
            return MEDIA_CLOSED;
        };
        let Some(retired) = queue.ready.pop_front() else {
            return MEDIA_PENDING;
        };
        queue.bytes -= retired.bytes.len();
        STATUS_OK
    }

    /// Validate, admit and retire while holding the same flow and queue
    /// barriers. Cancellation waits for the callback; BUSY consumes nothing.
    /// # Safety
    /// The sink is live synchronously, must not reenter Video and must return
    /// OK only after the transport has admitted exactly this unit.
    pub(crate) unsafe fn admit_metadata(
        &mut self,
        owner: &str,
        id: u64,
        unit_id: u64,
        sink: amaterasu_media::VideoMetadataAdmissionSinkV1,
    ) -> Status {
        let Some(queue) = self
            .streams
            .get_mut(&id)
            .filter(|stream| stream.owner == owner)
            .and_then(|stream| stream.negotiated.as_mut())
        else {
            return MEDIA_CLOSED;
        };
        if queue.failed {
            return STATUS_INVALID_ARGUMENT;
        }
        let Some(selected) = queue.metadata.as_ref() else {
            return STATUS_INVALID_ARGUMENT;
        };
        let Some(unit) = queue.ready.front_mut() else {
            return MEDIA_PENDING;
        };
        let header = unit_header(queue.selection, unit);
        if unit.access_unit_id != unit_id {
            return STATUS_INVALID_ARGUMENT;
        }
        let Some(metadata) = unit.metadata.as_mut().filter(|metadata| metadata.delivered) else {
            return STATUS_INVALID_ARGUMENT;
        };
        let Ok(mut flow) = selected.flow.try_lock() else {
            return MEDIA_BUSY;
        };
        if !flow.is_current(selected.capabilities)
            || metadata
                .association
                .check_transport(&header, &metadata.bytes, selected.capabilities.encoder)
                .is_err()
        {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: caller guarantees the synchronous callback and no Video reentry.
        let status = unsafe { (sink.admit)(sink.context) };
        if status != STATUS_OK {
            return status;
        }
        if flow
            .retire(
                &mut metadata.association,
                &header,
                &metadata.bytes,
                selected.capabilities,
            )
            .is_err()
        {
            queue.failed = true;
            return amaterasu_module_sdk::abi::STATUS_INTERNAL_ERROR;
        }
        drop(flow);
        let retired = queue.ready.pop_front().expect("admitted metadata unit");
        queue.bytes -= retired.bytes.len();
        STATUS_OK
    }

    pub fn eos_queued(&self, id: u64) -> bool {
        self.streams.get(&id).is_some_and(|stream| {
            stream
                .negotiated
                .as_ref()
                .map_or(stream.eos_queued, |q| q.eos_queued)
        })
    }

    pub fn eos_accepted(&self, id: u64) -> bool {
        self.streams.get(&id).is_some_and(|stream| {
            stream
                .negotiated
                .as_ref()
                .map_or(stream.eos_accepted, |q| q.eos_accepted)
        })
    }

    /// # Safety
    /// Packet memory must be readable until this synchronous copy returns.
    pub unsafe fn push(&mut self, id: u64, packet: &EncodedPacket) -> Status {
        let Some(stream) = self.streams.get_mut(&id) else {
            return MEDIA_CLOSED;
        };
        if stream.negotiated.is_some() {
            return STATUS_INVALID_ARGUMENT;
        }
        if stream.eos_queued {
            return MEDIA_CLOSED;
        }
        if let Err(status) = packet.validate() {
            return status;
        }
        if stream
            .clock_origin_us
            .checked_add(packet.timestamp_us)
            .is_none()
        {
            return STATUS_INVALID_ARGUMENT;
        }
        if stream.packets.len() >= 4 || stream.bytes.saturating_add(packet.len) > MAX_PACKET_BYTES {
            return MEDIA_BUSY;
        }
        // SAFETY: provider retains packet for the whole sink invocation.
        let bytes = match unsafe { packet.bytes() } {
            Ok(bytes) => bytes.to_vec(),
            Err(status) => return status,
        };
        stream.eos_queued = packet.flags == FLAG_END_OF_STREAM;
        stream.bytes += bytes.len();
        stream.packets.push_back(Packet {
            bytes,
            codec: packet.codec,
            flags: packet.flags,
            timestamp_us: packet.timestamp_us,
            sequence: packet.sequence,
        });
        STATUS_OK
    }

    /// # Safety
    /// Sink follows borrowed packet rules and must not reenter this queue.
    pub unsafe fn receive(&mut self, owner: &str, id: u64, sink: PacketSink) -> Status {
        let Some(stream) = self
            .streams
            .get_mut(&id)
            .filter(|stream| stream.owner == owner)
        else {
            return MEDIA_CLOSED;
        };
        if stream.negotiated.is_some() {
            return STATUS_INVALID_ARGUMENT;
        }
        let Some(packet) = stream.packets.front() else {
            return if stream.eos_accepted {
                MEDIA_CLOSED
            } else {
                MEDIA_PENDING
            };
        };
        let borrowed = EncodedPacket {
            codec: packet.codec,
            flags: packet.flags,
            data: packet.bytes.as_ptr(),
            len: packet.bytes.len(),
            timestamp_us: stream.clock_origin_us + packet.timestamp_us,
            sequence: packet.sequence,
        };
        // SAFETY: queue owns packet during callback; rejection retains it.
        let status = unsafe { sink.send(&borrowed) };
        if status == STATUS_OK {
            stream.eos_accepted = packet.flags == FLAG_END_OF_STREAM;
            stream.bytes -= packet.bytes.len();
            stream.packets.pop_front();
        }
        status
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use amaterasu_media::CODEC_H264;
    use std::ffi::c_void;
    unsafe extern "C" fn reject(_: *mut c_void, _: *const EncodedPacket) -> Status {
        MEDIA_BUSY
    }
    unsafe extern "C" fn accept(_: *mut c_void, packet: *const EncodedPacket) -> Status {
        // SAFETY: queue borrows this packet for the callback.
        assert_eq!(unsafe { (*packet).bytes() }.unwrap(), &[1, 2, 3]);
        STATUS_OK
    }
    #[test]
    fn rejects_timestamp_that_overflows_the_negotiated_host_epoch() {
        let mut queues = Queues::default();
        queues.open(1, "alice", u64::MAX - 1);
        let bytes = [1];
        let packet = EncodedPacket {
            codec: CODEC_H264,
            flags: 0,
            data: bytes.as_ptr(),
            len: 1,
            timestamp_us: 2,
            sequence: 1,
        };
        assert_eq!(unsafe { queues.push(1, &packet) }, STATUS_INVALID_ARGUMENT);
        assert_eq!(
            unsafe {
                queues.receive(
                    "alice",
                    1,
                    PacketSink {
                        context: std::ptr::null_mut(),
                        write: reject,
                    },
                )
            },
            MEDIA_PENDING
        );
    }
    #[test]
    fn bounded_queue_preserves_rejected_packet_and_enforces_owner() {
        let mut queues = Queues::default();
        queues.open(1, "alice", 0);
        let bytes = [1, 2, 3];
        let packet = EncodedPacket {
            codec: CODEC_H264,
            flags: 0,
            data: bytes.as_ptr(),
            len: bytes.len(),
            timestamp_us: 1,
            sequence: 1,
        };
        for _ in 0..4 {
            assert_eq!(unsafe { queues.push(1, &packet) }, STATUS_OK);
        }
        assert_eq!(unsafe { queues.push(1, &packet) }, MEDIA_BUSY);
        let rejected = PacketSink {
            context: std::ptr::null_mut(),
            write: reject,
        };
        assert_eq!(unsafe { queues.receive("bob", 1, rejected) }, MEDIA_CLOSED);
        assert_eq!(unsafe { queues.receive("alice", 1, rejected) }, MEDIA_BUSY);
        assert_eq!(queues.streams[&1].packets.len(), 4);
        let accepted = PacketSink {
            context: std::ptr::null_mut(),
            write: accept,
        };
        assert_eq!(unsafe { queues.receive("alice", 1, accepted) }, STATUS_OK);
        assert_eq!(queues.streams[&1].packets.len(), 3);
        assert_eq!(queues.streams[&1].bytes, 9);
        queues.close(1);
        assert_eq!(
            unsafe { queues.receive("alice", 1, accepted) },
            MEDIA_CLOSED
        );
    }
    #[test]
    fn byte_budget_rejects_before_copying_another_packet() {
        let mut queues = Queues::default();
        queues.open(1, "alice", 0);
        let bytes = vec![1; MAX_PACKET_BYTES];
        let packet = EncodedPacket {
            codec: CODEC_H264,
            flags: 0,
            data: bytes.as_ptr(),
            len: bytes.len(),
            timestamp_us: 1,
            sequence: 1,
        };
        assert_eq!(unsafe { queues.push(1, &packet) }, STATUS_OK);
        assert_eq!(unsafe { queues.push(1, &packet) }, MEDIA_BUSY);
        assert_eq!(queues.streams[&1].packets.len(), 1);
    }
    #[test]
    fn eos_waits_for_fifo_capacity_and_is_delivered_only_once() {
        unsafe extern "C" fn count(context: *mut c_void, packet: *const EncodedPacket) -> Status {
            // SAFETY: test sink borrows a counter and packet for this call.
            if unsafe { (*packet).flags } == FLAG_END_OF_STREAM {
                unsafe {
                    *context.cast::<usize>() += 1;
                }
            }
            STATUS_OK
        }
        let mut queues = Queues::default();
        queues.open(1, "alice", 0);
        let bytes = [1];
        let packet = EncodedPacket {
            codec: CODEC_H264,
            flags: 0,
            data: bytes.as_ptr(),
            len: 1,
            timestamp_us: 1,
            sequence: 1,
        };
        for _ in 0..4 {
            assert_eq!(unsafe { queues.push(1, &packet) }, STATUS_OK);
        }
        let eos = EncodedPacket {
            flags: FLAG_END_OF_STREAM,
            data: std::ptr::null(),
            len: 0,
            ..packet
        };
        assert_eq!(unsafe { queues.push(1, &eos) }, MEDIA_BUSY);
        let mut count_value = 0usize;
        let sink = PacketSink {
            context: (&mut count_value as *mut usize).cast(),
            write: count,
        };
        assert_eq!(unsafe { queues.receive("alice", 1, sink) }, STATUS_OK);
        assert_eq!(unsafe { queues.push(1, &eos) }, STATUS_OK);
        assert_eq!(unsafe { queues.push(1, &eos) }, MEDIA_CLOSED);
        for _ in 0..4 {
            assert_eq!(unsafe { queues.receive("alice", 1, sink) }, STATUS_OK);
        }
        assert_eq!(count_value, 1);
        assert!(queues.eos_accepted(1));
        assert_eq!(unsafe { queues.receive("alice", 1, sink) }, MEDIA_CLOSED);
    }
}

#[cfg(test)]
mod geometry_tests {
    use super::*;
    use amaterasu_media::{CaptureGeometry, DesktopRect};
    #[test]
    fn geometry_tracks_the_owned_stream_and_peer_topology_without_crossing_callers() {
        let mut queues = Queues::default();
        queues.open(1, "alice", 0);
        let first = CaptureGeometry::new(
            DesktopRect::new(1536, 0, 1920, 1080).unwrap(),
            DesktopRect::new(0, 0, 3456, 1080).unwrap(),
        )
        .unwrap();
        queues.set_geometry(1, Some(first));
        assert_eq!(queues.geometry("alice", 1).unwrap(), Some(first));
        assert_eq!(queues.geometry("bob", 1), Err(MEDIA_CLOSED));
        let expanded =
            CaptureGeometry::new(first.output, DesktopRect::new(0, 0, 5376, 1080).unwrap())
                .unwrap();
        queues.set_geometry(1, Some(expanded));
        assert_eq!(queues.geometry("alice", 1).unwrap(), Some(expanded));
        queues.close(1);
        assert_eq!(queues.geometry("alice", 1), Err(MEDIA_CLOSED));
    }

    #[test]
    fn packets_leave_on_the_host_clock() {
        use amaterasu_media::{CODEC_H264, FLAG_KEYFRAME};
        use std::ffi::c_void;
        let mut queues = Queues::default();
        queues.open(1, "alice", 1_000_000);
        let data = [0u8, 0, 0, 1, 0x65];
        let packet = EncodedPacket {
            codec: CODEC_H264,
            flags: FLAG_KEYFRAME,
            data: data.as_ptr(),
            len: data.len(),
            timestamp_us: 250,
            sequence: 1,
        };
        // SAFETY: the packet bytes outlive the synchronous copy.
        assert_eq!(unsafe { queues.push(1, &packet) }, STATUS_OK);
        unsafe extern "C" fn record(context: *mut c_void, packet: *const EncodedPacket) -> Status {
            // SAFETY: the test passes a live u64 and a packet valid for this call.
            unsafe { *context.cast::<u64>() = (*packet).timestamp_us };
            STATUS_OK
        }
        let mut seen = 0u64;
        let sink = PacketSink {
            context: (&mut seen as *mut u64).cast(),
            write: record,
        };
        // SAFETY: the sink and its context outlive this synchronous receive.
        assert_eq!(unsafe { queues.receive("alice", 1, sink) }, STATUS_OK);
        assert_eq!(seen, 1_000_250);
    }
}
