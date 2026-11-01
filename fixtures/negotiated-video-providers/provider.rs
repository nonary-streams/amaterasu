#[path = "submitted.rs"]
mod submitted;
use amaterasu_media::*;
use amaterasu_module_sdk::{
    abi::*,
    encode::{self, EncoderProvider, OutputQueue, ProviderAdapter, SessionMode},
    video_framing::{self, FramingProvider, OwnedFramePlan},
};
use std::{ffi::c_void, mem::size_of};

struct Encoder {
    queue: OutputQueue,
}
impl EncoderProvider for Encoder {
    fn probe(&self, config: encode::SessionConfig) -> Result<encode::ControlResponse, Status> {
        if config.mode != SessionMode::Source
            || config.source_id != 6
            || config.codec.namespace_id != 42
            || config.codec.value != 9
            || config.framing.namespace_id != 42
            || config.framing.value != 10
        {
            return Err(MEDIA_UNSUPPORTED);
        }
        Ok(encode::ControlResponse {
            kind: 0,
            request_id: 0,
            effective_generation: 1,
            effective_submission_id: 0,
            outcome: ENCODER_OUTCOME_APPLIED,
            schema: Default::default(),
            data: Vec::new(),
        })
    }
    fn configure(&self, session: u64, config: encode::SessionConfig) -> Result<(), Status> {
        self.probe(config.clone())?;
        self.queue
            .push(encode::EncodedPacket {
                flags: ENCODER_CHUNK_ACCESS_UNIT_START
                    | ENCODER_CHUNK_ACCESS_UNIT_END
                    | ENCODER_CHUNK_KEYFRAME,
                session_id: session,
                access_unit_id: 1,
                submission_id: 0,
                source_id: config.source_id,
                config_generation: config.config_generation,
                timestamp_us: 100,
                codec: config.codec,
                framing: config.framing,
                bytes: vec![10, 20, ENCODER_MARKER],
            })
            .map_err(|_| STATUS_INVALID_ARGUMENT)
    }
    fn output(&self, session: u64) -> Result<OutputQueue, Status> {
        if session == 77 {
            Ok(self.queue.clone())
        } else {
            Err(STATUS_INVALID_ARGUMENT)
        }
    }
    fn control(
        &self,
        _: u64,
        _: encode::ControlRequest,
    ) -> Result<encode::ControlResponse, Status> {
        Err(MEDIA_UNSUPPORTED)
    }
    fn drain(&self, session: u64) -> Result<(), Status> {
        if self.queue.snapshot().ended {
            return Ok(());
        }
        self.queue.drain().map_err(|_| STATUS_INVALID_ARGUMENT)?;
        self.queue
            .finish(encode::EncodedPacket {
                flags: ENCODER_CHUNK_END_OF_STREAM,
                session_id: session,
                access_unit_id: 0,
                submission_id: 0,
                source_id: 6,
                config_generation: 1,
                timestamp_us: 100,
                codec: encode::FormatId {
                    namespace_id: 42,
                    value: 9,
                    schema: 1,
                },
                framing: encode::FormatId {
                    namespace_id: 42,
                    value: 10,
                    schema: 1,
                },
                bytes: Vec::new(),
            })
            .map_err(|_| MEDIA_BUSY)
    }
    fn destroy(&self, _: u64) {
        self.queue.cancel();
    }
    fn release(&self) -> Result<(), Status> {
        self.queue.cancel();
        Ok(())
    }
}
unsafe extern "C" fn bind_encoder(
    _: *mut c_void,
    principal: BorrowedBytes,
    query: *const NativeInterfaceQuery,
    output: *mut *const NativeInterface,
) -> Status {
    if query.is_null() || output.is_null() || unsafe { principal.read() }.ok() != Some(b"video") {
        return STATUS_INVALID_ARGUMENT;
    }
    let query = unsafe { &*query };
    if !query.header.supports::<NativeInterfaceQuery>()
        || query.interface_major != 1
        || query.min_minor > 0
        || query.min_size as usize > size_of::<EncoderProviderApi>()
    {
        return STATUS_INVALID_ARGUMENT;
    }
    let queue = OutputQueue::new(SessionMode::Source, 4, 1024).unwrap();
    let table = ProviderAdapter::new(Encoder { queue }).into_binding();
    unsafe {
        *output = table.cast();
    }
    STATUS_OK
}
struct Framer;
impl FramingProvider for Framer {
    fn probe(
        &self,
        principal: &str,
        codec: EncoderFormatId,
        framing: EncoderFormatId,
        limits: &VideoFramingLimits,
    ) -> Result<(), Status> {
        if principal != "consumer"
            || (codec.namespace_id, codec.value, codec.schema) != (42, 9, 1)
            || (framing.namespace_id, framing.value, framing.schema) != (42, 10, 1)
            || limits.min_fec_percentage != 0
        {
            return Err(MEDIA_UNSUPPORTED);
        }
        Ok(())
    }
    fn plan(
        &self,
        principal: &str,
        _: &NegotiatedVideoUnit,
        bytes: &[u8],
        _: &VideoFramingLimits,
    ) -> Result<OwnedFramePlan, Status> {
        if principal != "consumer" {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        let mut framed = bytes.to_vec();
        framed.push(FRAMER_MARKER);
        Ok(OwnedFramePlan {
            framed_bytes: framed,
            record_starts: vec![],
            critical_prefix_shards: 0,
            blocks: vec![VideoFecBlock {
                first_data_shard: 0,
                data_shards: 1,
                parity_shards: 0,
                percentage: 0,
            }],
        })
    }

    fn probe_metadata(
        &self,
        principal: &str,
        codec: EncoderFormatId,
        framing: EncoderFormatId,
        schema: FrameMetadataSchemaId,
        limits: &VideoFramingLimits,
    ) -> Result<(), Status> {
        self.probe(principal, codec, framing, limits)?;
        if (schema.namespace_id, schema.value, schema.schema) != (1, 1, 1) {
            return Err(MEDIA_UNSUPPORTED);
        }
        Ok(())
    }

    fn plan_metadata(
        &self,
        principal: &str,
        unit: &NegotiatedVideoUnit,
        bytes: &[u8],
        metadata: &amaterasu_module_sdk::media::metadata::FrameMetadataBundle,
        limits: &VideoFramingLimits,
    ) -> Result<OwnedFramePlan, Status> {
        let item = metadata
            .items
            .iter()
            .find(|item| {
                item.required
                    && (
                        item.schema.namespace_id,
                        item.schema.value,
                        item.schema.schema,
                    ) == (1, 1, 1)
            })
            .ok_or(STATUS_INVALID_ARGUMENT)?;
        let mut plan = self.plan(principal, unit, bytes, limits)?;
        // Synthetic test bytes demonstrate which independent framer was used;
        // this is not a real HDR or codec bitstream.
        plan.framed_bytes.extend_from_slice(&item.payload);
        Ok(plan)
    }
}
struct Bundle {
    submitted: submitted::SubmittedBundle,
    encoder: NativeServiceFactory,
    framer: Box<video_framing::Factory<Framer>>,
}
#[unsafe(no_mangle)]
pub extern "C" fn route_fixture_create() -> *mut c_void {
    Box::into_raw(Box::new(Bundle {
        submitted: submitted::SubmittedBundle::new(),
        encoder: NativeServiceFactory {
            header: AbiHeader::of::<NativeServiceFactory>(),
            context: std::ptr::null_mut(),
            bind: Some(bind_encoder),
        },
        framer: video_framing::Factory::new(Framer),
    }))
    .cast()
}
/// # Safety
/// `bundle` must be a live bundle returned by `route_fixture_create`; the
/// returned factory must not outlive it or the loaded provider library.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn route_fixture_encoder(bundle: *mut c_void) -> *const NativeServiceFactory {
    unsafe { &(*bundle.cast::<Bundle>()).encoder }
}
/// # Safety
/// `bundle` must be a live bundle returned by `route_fixture_create`; the
/// returned factory must not outlive it or the loaded provider library.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn route_fixture_framer(bundle: *mut c_void) -> *const NativeServiceFactory {
    unsafe { &(*bundle.cast::<Bundle>()).framer.api }
}
/// # Safety
/// `bundle` must be null or a uniquely owned bundle returned by
/// `route_fixture_create`. Release every binding before destroying it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn route_fixture_destroy(bundle: *mut c_void) {
    if !bundle.is_null() {
        unsafe {
            drop(Box::from_raw(bundle.cast::<Bundle>()));
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn route_fixture_submitted_capture(
    bundle: *mut c_void,
) -> *const NativeServiceFactory {
    &**unsafe { &(*bundle.cast::<Bundle>()).submitted.capture }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn route_fixture_submitted_consumer(
    bundle: *mut c_void,
) -> *const NativeServiceFactory {
    &**unsafe { &(*bundle.cast::<Bundle>()).submitted.consumer }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn route_fixture_submitted_processor(
    bundle: *mut c_void,
) -> *const NativeServiceFactory {
    &**unsafe { &(*bundle.cast::<Bundle>()).submitted.processor }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn route_fixture_submitted_encoder(
    bundle: *mut c_void,
) -> *const NativeServiceFactory {
    &**unsafe { &(*bundle.cast::<Bundle>()).submitted.encoder }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn route_fixture_submitted_capabilities(
    bundle: *mut c_void,
) -> *const NativeServiceFactory {
    &**unsafe { &(*bundle.cast::<Bundle>()).submitted.capabilities }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn route_fixture_submitted_control(
    bundle: *mut c_void,
) -> *const NativeServiceFactory {
    &**unsafe { &(*bundle.cast::<Bundle>()).submitted.legacy_encoder }
}
