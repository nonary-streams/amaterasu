//! Owner-scoped plan admission into the production media transport.
use super::{Shared, guarded};
use amaterasu_media::{
    MEDIA_BACKEND_ERROR, MEDIA_BUSY, MEDIA_UNSUPPORTED, NegotiatedVideoPlanTransportApiV2,
    NegotiatedVideoPlanTransportApiV2_1, NegotiatedVideoUnit, VideoFramePlan, VideoFramingLimits,
    VideoPlanSelectionV2, VideoPlanSelectionV3,
};
use amaterasu_module_sdk::abi::*;
use std::{ffi::c_void, sync::TryLockError};

pub struct Factory {
    pub api: NativeServiceFactory,
    shared: Shared,
}
impl Factory {
    pub fn new(shared: Shared) -> Box<Self> {
        let mut factory = Box::new(Self {
            api: NativeServiceFactory {
                header: AbiHeader::of::<NativeServiceFactory>(),
                context: std::ptr::null_mut(),
                bind: Some(bind),
            },
            shared,
        });
        factory.api.context = (&mut *factory as *mut Self).cast();
        factory
    }
}
struct Binding {
    api: NegotiatedVideoPlanTransportApiV2_1,
    shared: Shared,
    owner: String,
}
unsafe extern "C" fn bind(
    context: *mut c_void,
    principal: BorrowedBytes,
    query: *const NativeInterfaceQuery,
    output: *mut *const NativeInterface,
) -> Status {
    guarded(|| {
        if context.is_null()
            || query.is_null()
            || output.is_null()
            || !(query as usize).is_multiple_of(align_of::<NativeInterfaceQuery>())
            || !(output as usize).is_multiple_of(align_of::<*const NativeInterface>())
        {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: runtime supplies live query and writable output.
        unsafe {
            *output = std::ptr::null();
        }
        let query = unsafe { &*query };
        if !query.header.supports::<NativeInterfaceQuery>()
            || query.interface_major != 2
            || query.min_minor > 1
            || query.min_size as usize > size_of::<NegotiatedVideoPlanTransportApiV2_1>()
        {
            return MEDIA_UNSUPPORTED;
        }
        // SAFETY: runtime lends principal bytes for this synchronous call.
        let owner = match unsafe { principal.read() }
            .ok()
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
        {
            Some(owner) if !owner.is_empty() && owner.len() <= 128 => owner.to_owned(),
            _ => return STATUS_INVALID_ARGUMENT,
        };
        let factory = unsafe { &*context.cast::<Factory>() };
        let mut binding = Box::new(Binding {
            api: NegotiatedVideoPlanTransportApiV2_1 {
                base: NegotiatedVideoPlanTransportApiV2 {
                    base: NativeInterface {
                        header: AbiHeader {
                            size: size_of::<NegotiatedVideoPlanTransportApiV2_1>() as u32,
                            major: 2,
                            minor: 1,
                        },
                        context: std::ptr::null_mut(),
                        release: Some(release),
                    },
                    probe,
                    prepare,
                    submit,
                    drain,
                },
                probe_metadata,
                prepare_metadata,
            },
            shared: factory.shared.clone(),
            owner,
        });
        binding.api.base.base.context = (&mut *binding as *mut Binding).cast();
        let binding = Box::into_raw(binding);
        // SAFETY: table allocation transfers to runtime until release.
        unsafe {
            *output = &(*binding).api.base.base;
        }
        STATUS_OK
    })
}

unsafe extern "C" fn probe_metadata(
    context: *mut c_void,
    stream: u64,
    selection: *const VideoPlanSelectionV3,
    limits: *mut VideoFramingLimits,
) -> Status {
    metadata_limits(context, stream, selection, limits, false)
}
unsafe extern "C" fn prepare_metadata(
    context: *mut c_void,
    stream: u64,
    selection: *const VideoPlanSelectionV3,
    limits: *mut VideoFramingLimits,
) -> Status {
    metadata_limits(context, stream, selection, limits, true)
}
fn metadata_limits(
    context: *mut c_void,
    stream: u64,
    selection: *const VideoPlanSelectionV3,
    limits: *mut VideoFramingLimits,
    commit: bool,
) -> Status {
    guarded(|| {
        if selection.is_null()
            || limits.is_null()
            || !(selection as usize).is_multiple_of(align_of::<VideoPlanSelectionV3>())
            || !(limits as usize).is_multiple_of(align_of::<VideoFramingLimits>())
        {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: the caller lends aligned descriptors and writable limits synchronously.
        unsafe {
            with_binding(context, |controller, owner| {
                let result = if commit {
                    controller.prepare_metadata_video_plan(owner, stream, &*selection)
                } else {
                    controller.probe_metadata_video_plan(owner, stream, &*selection)
                };
                match result {
                    Ok(selected) => {
                        limits.write(selected);
                        STATUS_OK
                    }
                    Err(status) => status,
                }
            })
        }
    })
}
unsafe extern "C" fn release(context: *mut c_void) {
    let _ = guarded(|| {
        if !context.is_null() {
            unsafe {
                drop(Box::from_raw(context.cast::<Binding>()));
            }
        }
        STATUS_OK
    });
}

unsafe fn with_binding(
    context: *mut c_void,
    operation: impl FnOnce(&mut super::Controller, &str) -> Status,
) -> Status {
    if context.is_null() {
        return STATUS_INVALID_ARGUMENT;
    }
    // SAFETY: runtime retains the binding until all callbacks return.
    let binding = unsafe { &*context.cast::<Binding>() };
    let mut controller = match binding.shared.try_lock() {
        Ok(controller) => controller,
        Err(TryLockError::WouldBlock) => return MEDIA_BUSY,
        Err(TryLockError::Poisoned(_)) => return MEDIA_BACKEND_ERROR,
    };
    operation(&mut controller, &binding.owner)
}

unsafe extern "C" fn probe(
    context: *mut c_void,
    stream: u64,
    selection: *const VideoPlanSelectionV2,
    limits: *mut VideoFramingLimits,
) -> Status {
    plan_limits(context, stream, selection, limits, false)
}
unsafe extern "C" fn prepare(
    context: *mut c_void,
    stream: u64,
    selection: *const VideoPlanSelectionV2,
    limits: *mut VideoFramingLimits,
) -> Status {
    plan_limits(context, stream, selection, limits, true)
}
fn plan_limits(
    context: *mut c_void,
    stream: u64,
    selection: *const VideoPlanSelectionV2,
    limits: *mut VideoFramingLimits,
    commit: bool,
) -> Status {
    guarded(|| {
        if selection.is_null()
            || limits.is_null()
            || !(selection as usize).is_multiple_of(align_of::<VideoPlanSelectionV2>())
            || !(limits as usize).is_multiple_of(align_of::<VideoFramingLimits>())
        {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: authorized caller keeps descriptors live and output writable.
        unsafe {
            with_binding(context, |controller, owner| {
                let result = if commit {
                    controller.prepare_video_plan(owner, stream, &*selection)
                } else {
                    controller.probe_video_plan(owner, stream, &*selection)
                };
                match result {
                    Ok(selected) => {
                        limits.write(selected);
                        STATUS_OK
                    }
                    Err(status) => status,
                }
            })
        }
    })
}
unsafe extern "C" fn submit(
    context: *mut c_void,
    stream: u64,
    unit: *const NegotiatedVideoUnit,
    plan: *const VideoFramePlan,
) -> Status {
    guarded(|| {
        if unit.is_null()
            || plan.is_null()
            || !(unit as usize).is_multiple_of(align_of::<NegotiatedVideoUnit>())
            || !(plan as usize).is_multiple_of(align_of::<VideoFramePlan>())
        {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: caller retains both headers and their nested byte borrows.
        unsafe {
            with_binding(context, |controller, owner| {
                controller.submit_video_plan(owner, stream, &*unit, &*plan)
            })
        }
    })
}
unsafe extern "C" fn drain(context: *mut c_void, stream: u64) -> Status {
    guarded(|| unsafe {
        with_binding(context, |controller, owner| controller.drain(owner, stream))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        controller::Controller,
        model::{Operation, Settings},
        stream::tests::{receivers, request},
    };
    use amaterasu_contracts::media_profile::{FormatId, VideoProfile};
    use amaterasu_media::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn native_plan_transport_checks_owner_profile_and_host_fec_before_delivery() {
        let (video, audio) = receivers();
        let codec = EncoderFormatId {
            namespace_id: 7,
            value: 2,
            schema: 1,
        };
        let framing = EncoderFormatId {
            namespace_id: 7,
            value: 3,
            schema: 1,
        };
        let profile = VideoProfile {
            codec: FormatId {
                namespace_id: 7,
                value: 2,
                schema: 1,
            },
            framing: FormatId {
                namespace_id: 7,
                value: 3,
                schema: 1,
            },
            wire_selector: 4,
            payload_type: 100,
            encoding_name: "EXAMPLE".into(),
            media_format: None,
            wire_attributes: Vec::new(),
        };
        let settings = Settings {
            enabled: true,
            allowed_principals: vec!["owner".into()],
            plan_service: Some("example.plan/1".into()),
            extension_profiles: vec![profile],
            ..Default::default()
        };
        let shared = Arc::new(Mutex::new(Controller::new(settings)));
        let stream = shared
            .lock()
            .unwrap()
            .handle("owner", Operation::Open(request(&video, &audio, false)))
            .unwrap()
            .created
            .unwrap();
        let factory = Factory::new(shared.clone());
        let query = NativeInterfaceQuery {
            header: AbiHeader::of::<NativeInterfaceQuery>(),
            interface_major: 2,
            min_minor: 0,
            min_size: size_of::<NegotiatedVideoPlanTransportApiV2>() as u32,
        };
        let bind_owner = |owner: &str| {
            let mut table = std::ptr::null();
            assert_eq!(
                unsafe {
                    bind(
                        factory.api.context,
                        BorrowedBytes::new(owner.as_bytes()),
                        &query,
                        &mut table,
                    )
                },
                STATUS_OK
            );
            table.cast::<NegotiatedVideoPlanTransportApiV2>()
        };
        let mut selection = VideoPlanSelectionV2 {
            base: amaterasu_media::VideoPlanSelection {
                size: size_of::<VideoPlanSelectionV2>() as u32,
                version: 2,
                stream: 5,
                generation: 5,
                offer_id: 4,
                source_id: 6,
                config_generation: 1,
                codec,
                framing,
                clock_epoch: [3; 16],
                clock_origin_us: host_monotonic_micros().max(1),
                metadata_required: 0,
                reserved: 0,
            },
            payload_type: 100,
            encoding_name_len: 7,
            encoding_name: {
                let mut name = [0; 32];
                name[..7].copy_from_slice(b"EXAMPLE");
                name
            },
        };
        let mut limits = std::mem::MaybeUninit::<VideoFramingLimits>::uninit();
        let other = bind_owner("other");
        assert_eq!(
            unsafe {
                ((*other).prepare)(
                    (*other).base.context,
                    stream,
                    &selection,
                    limits.as_mut_ptr(),
                )
            },
            MEDIA_CLOSED
        );
        unsafe { ((*other).base.release.unwrap())((*other).base.context) };
        let table = bind_owner("owner");
        // Both the pure probe and the committing prepare must reject every
        // mismatch before the stream can acquire transport state.
        let mut check_rejected = |selection: &VideoPlanSelectionV2| {
            for operation in [unsafe { (*table).probe }, unsafe { (*table).prepare }] {
                assert_eq!(
                    unsafe {
                        operation(
                            (*table).base.context,
                            stream,
                            selection,
                            limits.as_mut_ptr(),
                        )
                    },
                    STATUS_INVALID_ARGUMENT
                );
            }
        };
        let exact = selection;
        selection.payload_type = 101;
        check_rejected(&selection);
        selection = exact;
        selection.encoding_name[0] = b'Z';
        check_rejected(&selection);
        selection = exact;
        selection.encoding_name[31] = b'X';
        check_rejected(&selection);
        selection = exact;
        selection.encoding_name_len = 33;
        check_rejected(&selection);
        selection = exact;
        selection.base.version = 1;
        check_rejected(&selection);
        selection = exact;
        selection.base.metadata_required = 1;
        check_rejected(&selection);
        selection = exact;
        selection.base.offer_id = 5;
        assert_eq!(
            unsafe {
                ((*table).prepare)(
                    (*table).base.context,
                    stream,
                    &selection,
                    limits.as_mut_ptr(),
                )
            },
            STATUS_INVALID_ARGUMENT
        );
        selection.base.offer_id = 4;
        selection.base.metadata_required = 1;
        assert_eq!(
            unsafe {
                ((*table).prepare)(
                    (*table).base.context,
                    stream,
                    &selection,
                    limits.as_mut_ptr(),
                )
            },
            STATUS_INVALID_ARGUMENT
        );
        selection.base.metadata_required = 0;
        assert_eq!(
            unsafe {
                ((*table).prepare)(
                    (*table).base.context,
                    stream,
                    &selection,
                    limits.as_mut_ptr(),
                )
            },
            STATUS_OK
        );
        let limits = unsafe { limits.assume_init() };
        assert_eq!(
            (limits.min_fec_percentage, limits.min_parity_shards),
            (20, 2)
        );
        let data = [1, 2, 3, 4];
        let unit = NegotiatedVideoUnit {
            size: size_of::<NegotiatedVideoUnit>() as u32,
            version: VIDEO_UNIT_VERSION,
            stream: 5,
            generation: 5,
            offer_id: 4,
            codec,
            framing,
            access_unit_id: 1,
            submission_id: 1,
            source_id: 6,
            config_generation: 1,
            timestamp_us: 0,
            clock: FrameClockMetadata {
                version: 1,
                size: size_of::<FrameClockMetadata>() as u32,
                domain: FRAME_CLOCK_STREAM_RELATIVE,
                reserved: 0,
                epoch_id: [3; 16],
            },
            flags: VIDEO_UNIT_KEYFRAME,
            reserved: 0,
            data: BorrowedBytes::new(&data),
        };
        let mut blocks = [VideoFecBlock::default(); VIDEO_FRAME_MAX_BLOCKS];
        blocks[0] = VideoFecBlock {
            first_data_shard: 0,
            data_shards: 1,
            parity_shards: 0,
            percentage: 0,
        };
        let mut plan = VideoFramePlan {
            size: size_of::<VideoFramePlan>() as u32,
            version: 1,
            framed_bytes: BorrowedBytes::new(&data),
            record_starts: BorrowedBytes::new(&[]),
            critical_prefix_shards: 0,
            block_count: 1,
            blocks,
        };
        assert_eq!(
            unsafe { ((*table).submit)((*table).base.context, stream, &unit, &plan) },
            STATUS_INVALID_ARGUMENT
        );
        plan.blocks[0].parity_shards = 2;
        plan.blocks[0].percentage = 200;
        assert_eq!(
            unsafe { ((*table).submit)((*table).base.context, stream, &unit, &plan) },
            STATUS_OK
        );
        shared.lock().unwrap().tick();
        let mut packet = [0u8; 2048];
        let received = video.recv(&mut packet).unwrap();
        assert!(received >= 44);
        assert_eq!(&packet[40..44], &data);
        unsafe { ((*table).base.release.unwrap())((*table).base.context) };
    }
    #[test]
    fn metadata_selection_cannot_downgrade_to_legacy_plan_or_change_schema() {
        let profile = VideoProfile {
            codec: FormatId {
                namespace_id: 7,
                value: 2,
                schema: 1,
            },
            framing: FormatId {
                namespace_id: 7,
                value: 3,
                schema: 1,
            },
            wire_selector: 4,
            payload_type: 100,
            encoding_name: "EXAMPLE".into(),
            media_format: None,
            wire_attributes: Vec::new(),
        };
        let schema = FormatId {
            namespace_id: 1,
            value: 1,
            schema: 1,
        };
        let settings = Settings {
            enabled: true,
            allowed_principals: vec!["owner".into()],
            plan_service_v2: Some("example.plan/1".into()),
            extension_profiles: vec![profile.clone()],
            metadata_profiles: vec![crate::model::MetadataProfile { profile, schema }],
            ..Default::default()
        };
        settings.validate().unwrap();
        let mut controller = Controller::new(settings);
        let stream = 1;
        let mut selection = VideoPlanSelectionV3 {
            base: VideoPlanSelectionV2 {
                base: VideoPlanSelection {
                    size: size_of::<VideoPlanSelectionV3>() as u32,
                    version: 3,
                    stream: 5,
                    generation: 5,
                    offer_id: 4,
                    source_id: 6,
                    config_generation: 1,
                    codec: EncoderFormatId {
                        namespace_id: 7,
                        value: 2,
                        schema: 1,
                    },
                    framing: EncoderFormatId {
                        namespace_id: 7,
                        value: 3,
                        schema: 1,
                    },
                    clock_epoch: [3; 16],
                    clock_origin_us: host_monotonic_micros().max(1),
                    metadata_required: 1,
                    reserved: 0,
                },
                payload_type: 100,
                encoding_name_len: 7,
                encoding_name: {
                    let mut name = [0; 32];
                    name[..7].copy_from_slice(b"EXAMPLE");
                    name
                },
            },
            schema: FrameMetadataSchemaId {
                namespace_id: 1,
                value: 1,
                schema: 1,
            },
            carriage_owner: FRAME_METADATA_OWNER_FRAMER,
            reserved: 0,
        };
        let v2 = selection.base_v2().unwrap();
        assert!(controller.probe_video_plan("owner", stream, &v2).is_err());
        assert!(
            controller
                .probe_legacy_video_plan("owner", stream, &v2.base_v1().unwrap())
                .is_err()
        );
        assert!(
            controller
                .probe_metadata_video_plan("other", stream, &selection)
                .is_err()
        );
        selection.schema.value = 2;
        assert!(
            controller
                .probe_metadata_video_plan("owner", stream, &selection)
                .is_err()
        );
        assert!(
            controller
                .prepare_metadata_video_plan("owner", stream, &selection)
                .is_err()
        );
        selection.schema.value = 1;
        selection.carriage_owner = FRAME_METADATA_OWNER_ENCODER;
        assert!(
            controller
                .probe_metadata_video_plan("owner", stream, &selection)
                .is_err()
        );
        selection.carriage_owner = FRAME_METADATA_OWNER_FRAMER;
        assert_eq!(
            controller
                .probe_metadata_video_plan("owner", stream, &selection)
                .unwrap_err(),
            MEDIA_CLOSED
        );
        assert_eq!(
            controller
                .prepare_metadata_video_plan("owner", stream, &selection)
                .unwrap_err(),
            MEDIA_CLOSED
        );
    }
}
