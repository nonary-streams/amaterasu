//! Hardware-free exercise of the unchanged managed plane's metadata retry path.
use super::*;
use crate::plane::{NegotiatedRoute, Plane, VideoUnitSelection};
use amaterasu_contracts::media_profile::{FormatId, VideoProfile};
use amaterasu_module_sdk::{Command, Response, media::metadata::*};
use std::{mem::size_of, ptr};

struct State {
    delivered: bool,
    framed: bool,
    retired: bool,
    plan_calls: usize,
    check_calls: usize,
    submit_calls: usize,
    retire_calls: usize,
    wire: Vec<u8>,
    data: Vec<u8>,
    metadata: Vec<u8>,
    limits: VideoFramingLimits,
}
struct Host {
    state: State,
    video: NegotiatedVideoApiV1_2,
    framer: VideoFramingApiV2_2,
    transport: NegotiatedVideoPlanTransportApiV2_1,
    media: MediaTransportApi,
    control: ControlTransportApi,
    external_framer: *const NativeServiceFactory,
    external_binding: usize,
}
fn native(size: usize, major: u32, minor: u32) -> NativeInterface {
    NativeInterface {
        header: AbiHeader {
            size: size as u32,
            major,
            minor,
        },
        context: ptr::null_mut(),
        release: None,
    }
}
unsafe extern "C" fn invoke(context: *mut c_void, bytes: BorrowedBytes, sink: ReplySink) -> Status {
    let host = unsafe { &mut *context.cast::<Host>() };
    let command: Command = serde_json::from_slice(unsafe { bytes.read() }.unwrap()).unwrap();
    let response = match command {
        Command::NativeBindTyped {
            service,
            interface_major,
            min_minor,
            min_size,
            ..
        } => {
            if service == "test.framer/1" && !host.external_framer.is_null() {
                let factory = unsafe { &*host.external_framer };
                let query = NativeInterfaceQuery {
                    header: AbiHeader::of::<NativeInterfaceQuery>(),
                    interface_major,
                    min_minor,
                    min_size,
                };
                let mut table = ptr::null();
                let status = unsafe {
                    (factory.bind.unwrap())(
                        factory.context,
                        BorrowedBytes::new(b"consumer"),
                        &query,
                        &mut table,
                    )
                };
                if status != STATUS_OK || table.is_null() {
                    return STATUS_INVALID_ARGUMENT;
                }
                host.external_binding = table as usize;
                let response = Response::NativeBound {
                    binding: table as u64,
                    address: table as usize,
                };
                unsafe { sink.send(&serde_json::to_vec(&response).unwrap()) };
                return STATUS_OK;
            }
            let address = match service.as_str() {
                "test.units/1" => &host.video.base.base.base as *const _ as usize,
                "test.framer/1" => &host.framer.base.base.base as *const _ as usize,
                "test.plan/1" => &host.transport.base.base as *const _ as usize,
                "amaterasu.media-transport/1" => &host.media.base as *const _ as usize,
                "amaterasu.control-transport/1" => &host.control.base as *const _ as usize,
                _ => 0,
            };
            if address == 0 {
                Response::error("unknown", "service")
            } else {
                Response::NativeBound {
                    binding: address as u64,
                    address,
                }
            }
        }
        Command::NativeRelease { binding } => {
            if binding as usize == host.external_binding && host.external_binding != 0 {
                let table = unsafe { &*(host.external_binding as *const NativeInterface) };
                if let Some(release) = table.release {
                    unsafe { release(table.context) };
                }
                host.external_binding = 0;
            }
            Response::Ok
        }
        _ => Response::error("unknown", "request"),
    };
    unsafe { sink.send(&serde_json::to_vec(&response).unwrap()) };
    STATUS_OK
}
unsafe extern "C" fn legacy_video(_: *mut c_void, _: u64, _: NegotiatedVideoUnitSink) -> Status {
    MEDIA_UNSUPPORTED
}
unsafe extern "C" fn metadata_video(
    context: *mut c_void,
    _: u64,
    sink: NegotiatedVideoMetadataUnitSinkV1,
) -> Status {
    let state = unsafe { &mut *context.cast::<State>() };
    if state.delivered {
        return MEDIA_PENDING;
    }
    let unit = NegotiatedVideoMetadataUnitV1 {
        size: size_of::<NegotiatedVideoMetadataUnitV1>() as u32,
        version: 1,
        unit: NegotiatedVideoUnit {
            size: size_of::<NegotiatedVideoUnit>() as u32,
            version: 1,
            stream: 10,
            generation: 10,
            offer_id: 9,
            codec: EncoderFormatId {
                namespace_id: 42,
                value: 9,
                schema: 1,
            },
            framing: EncoderFormatId {
                namespace_id: 42,
                value: 10,
                schema: 1,
            },
            access_unit_id: 1,
            submission_id: 1,
            source_id: 77,
            config_generation: 1,
            timestamp_us: 100,
            clock: FrameClockMetadata {
                size: size_of::<FrameClockMetadata>() as u32,
                version: 1,
                domain: FRAME_CLOCK_STREAM_RELATIVE,
                reserved: 0,
                epoch_id: [1; 16],
            },
            flags: VIDEO_UNIT_KEYFRAME,
            reserved: 0,
            data: BorrowedBytes::new(&state.data),
        },
        metadata: BorrowedBytes::new(&state.metadata),
    };
    let status = unsafe { (sink.write)(sink.context, &unit) };
    if status == STATUS_OK {
        state.delivered = true;
    }
    status
}
unsafe extern "C" fn framed(context: *mut c_void, stream: u64, unit: u64) -> Status {
    let state = unsafe { &mut *context.cast::<State>() };
    if stream != 10 || unit != 1 || state.framed || !state.delivered {
        return STATUS_INVALID_ARGUMENT;
    }
    state.framed = true;
    STATUS_OK
}
unsafe extern "C" fn check(_: *mut c_void, _: u64, _: u64) -> Status {
    MEDIA_UNSUPPORTED
}
unsafe extern "C" fn retire(_: *mut c_void, _: u64, _: u64) -> Status {
    MEDIA_UNSUPPORTED
}
unsafe extern "C" fn admit(
    context: *mut c_void,
    stream: u64,
    unit: u64,
    sink: VideoMetadataAdmissionSinkV1,
) -> Status {
    let state = unsafe { &mut *context.cast::<State>() };
    if stream != 10 || unit != 1 || !state.framed || state.retired {
        return STATUS_INVALID_ARGUMENT;
    }
    state.check_calls += 1;
    // The callback's transport uses the same state; no reference spans the callback.
    let status = unsafe { (sink.admit)(sink.context) };
    let state = unsafe { &mut *context.cast::<State>() };
    if status == STATUS_OK {
        state.retire_calls += 1;
        state.retired = true;
    }
    status
}
unsafe extern "C" fn legacy_plan(
    _: *mut c_void,
    _: u64,
    _: *const NegotiatedVideoUnit,
    _: *const VideoFramingLimits,
    _: VideoFramePlanSink,
) -> Status {
    MEDIA_UNSUPPORTED
}
unsafe extern "C" fn probe(
    _: *mut c_void,
    _: *const EncoderFormatId,
    _: *const EncoderFormatId,
    _: *const VideoFramingLimits,
) -> Status {
    STATUS_OK
}
unsafe extern "C" fn probe_metadata(
    _: *mut c_void,
    _: *const EncoderFormatId,
    _: *const EncoderFormatId,
    schema: *const FrameMetadataSchemaId,
    _: *const VideoFramingLimits,
) -> Status {
    let schema = unsafe { &*schema };
    if (schema.namespace_id, schema.value, schema.schema) == (1, 1, 1) {
        STATUS_OK
    } else {
        MEDIA_UNSUPPORTED
    }
}
unsafe extern "C" fn plan_metadata(
    context: *mut c_void,
    _: u64,
    unit: *const NegotiatedVideoMetadataUnitV1,
    _: *const VideoFramingLimits,
    sink: VideoFramePlanSink,
) -> Status {
    let state = unsafe { &mut *context.cast::<State>() };
    state.plan_calls += 1;
    let unit = unsafe { &*unit };
    if unsafe { unit.metadata.read() }.unwrap() != state.metadata {
        return STATUS_INVALID_ARGUMENT;
    }
    let mut blocks = [VideoFecBlock::default(); VIDEO_FRAME_MAX_BLOCKS];
    blocks[0] = VideoFecBlock {
        first_data_shard: 0,
        data_shards: 1,
        parity_shards: 0,
        percentage: 0,
    };
    let plan = VideoFramePlan {
        size: size_of::<VideoFramePlan>() as u32,
        version: 1,
        framed_bytes: unit.unit.data,
        record_starts: BorrowedBytes::new(&[]),
        critical_prefix_shards: 0,
        block_count: 1,
        blocks,
    };
    unsafe { (sink.write)(sink.context, &plan) }
}
unsafe extern "C" fn old_limits(
    _: *mut c_void,
    _: u64,
    _: *const VideoPlanSelectionV2,
    _: *mut VideoFramingLimits,
) -> Status {
    MEDIA_UNSUPPORTED
}
unsafe extern "C" fn new_limits(
    context: *mut c_void,
    _: u64,
    selection: *const VideoPlanSelectionV3,
    output: *mut VideoFramingLimits,
) -> Status {
    let state = unsafe { &mut *context.cast::<State>() };
    if unsafe { (*selection).base_v2() }.is_none() {
        return STATUS_INVALID_ARGUMENT;
    }
    unsafe { output.write(state.limits) };
    STATUS_OK
}
unsafe extern "C" fn submit_plan(
    context: *mut c_void,
    _: u64,
    _: *const NegotiatedVideoUnit,
    plan: *const VideoFramePlan,
) -> Status {
    let state = unsafe { &mut *context.cast::<State>() };
    state.submit_calls += 1;
    if state.submit_calls == 1 {
        MEDIA_BUSY
    } else {
        let plan = unsafe { &*plan };
        state.wire = unsafe { plan.framed_bytes.read() }.unwrap().to_vec();
        STATUS_OK
    }
}
unsafe extern "C" fn drain(_: *mut c_void, _: u64) -> Status {
    STATUS_OK
}
unsafe extern "C" fn media_video(_: *mut c_void, _: u64, _: *const EncodedPacket) -> Status {
    MEDIA_UNSUPPORTED
}
unsafe extern "C" fn media_audio(_: *mut c_void, _: u64, _: *const EncodedAudioPacket) -> Status {
    STATUS_OK
}
unsafe extern "C" fn control_send(_: *mut c_void, _: u64, _: *const ControlMessage) -> Status {
    STATUS_OK
}
unsafe extern "C" fn control_receive(_: *mut c_void, _: u64, _: ControlMessageSink) -> Status {
    MEDIA_PENDING
}

pub(crate) struct Fixture {
    host: Box<Host>,
    pub limits: VideoFramingLimits,
}
impl Fixture {
    pub fn new(external_framer: *const NativeServiceFactory) -> Self {
        let schema = amaterasu_module_sdk::encode::FormatId {
            namespace_id: 1,
            value: 1,
            schema: 1,
        };
        let metadata = FrameMetadataBundle {
            identity: FrameMetadataIdentity {
                session_id: 10,
                submission_id: 1,
                source_id: 77,
                source_generation: 9,
                config_generation: 1,
                timestamp_us: 30,
                clock_domain: FRAME_CLOCK_HOST_MONOTONIC,
                clock_epoch: [1; 16],
            },
            owner: CarriageOwner::FramerInsert,
            items: vec![FrameMetadataItem {
                schema,
                required: true,
                payload: vec![0xb5, 1],
            }],
        }
        .encode()
        .unwrap();
        let limits = VideoFramingLimits {
            size: size_of::<VideoFramingLimits>() as u32,
            version: VIDEO_FRAMING_LIMITS_VERSION,
            packet_size: 1024,
            crypto_overhead: 0,
            network_overhead: 28,
            allowed_extensions: 0,
            max_datagrams: 16,
            max_queue_bytes: 65536,
            min_parity_shards: 0,
            max_fec_percentage: 0,
            max_wire_bytes: 65536,
            min_fec_percentage: 0,
            max_fec_blocks: 1,
            fec_policy_flags: 0,
            reserved: 0,
        };
        let mut host = Box::new(Host {
            state: State {
                delivered: false,
                framed: false,
                retired: false,
                plan_calls: 0,
                check_calls: 0,
                submit_calls: 0,
                retire_calls: 0,
                wire: Vec::new(),
                data: vec![1, 2, 3],
                metadata,
                limits,
            },
            video: NegotiatedVideoApiV1_2 {
                base: NegotiatedVideoApiV1_1 {
                    base: NegotiatedVideoApi {
                        base: native(size_of::<NegotiatedVideoApiV1_2>(), 1, 2),
                        receive: legacy_video,
                    },
                    receive_metadata: metadata_video,
                    framed_metadata: framed,
                    check_metadata: check,
                    retire_metadata: retire,
                },
                admit_metadata: admit,
            },
            framer: VideoFramingApiV2_2 {
                base: VideoFramingApiV2_1 {
                    base: VideoFramingApi {
                        base: native(size_of::<VideoFramingApiV2_2>(), 2, 2),
                        plan: legacy_plan,
                    },
                    probe,
                },
                probe_metadata,
                plan_metadata,
            },
            transport: NegotiatedVideoPlanTransportApiV2_1 {
                base: NegotiatedVideoPlanTransportApiV2 {
                    base: native(size_of::<NegotiatedVideoPlanTransportApiV2_1>(), 2, 1),
                    probe: old_limits,
                    prepare: old_limits,
                    submit: submit_plan,
                    drain,
                },
                probe_metadata: new_limits,
                prepare_metadata: new_limits,
            },
            media: MediaTransportApi {
                base: native(size_of::<MediaTransportApi>(), 1, 0),
                submit_video: media_video,
                submit_audio: media_audio,
                drain,
            },
            control: ControlTransportApi {
                base: native(size_of::<ControlTransportApi>(), 1, 0),
                send: control_send,
                receive: control_receive,
            },
            external_framer,
            external_binding: 0,
        });
        let state = (&mut host.state as *mut State).cast();
        host.video.base.base.base.context = state;
        host.framer.base.base.base.context = state;
        host.transport.base.base.context = state;
        host.media.base.context = state;
        host.control.base.context = state;

        Self { host, limits }
    }
    pub fn runtime(&mut self) -> RuntimeClient {
        unsafe {
            RuntimeClient::from_endpoint(HostApi {
                header: AbiHeader::of::<HostApi>(),
                context: (&mut *self.host as *mut Host).cast(),
                invoke,
            })
        }
    }
    pub fn wire(&self) -> &[u8] {
        &self.host.state.wire
    }
    pub fn retired(&self) -> bool {
        self.host.state.retired
    }
}

fn exercise_managed_plane(external_framer: *const NativeServiceFactory, marker: Option<u8>) {
    let mut fixture = Fixture::new(external_framer);
    let runtime = fixture.runtime();
    let limits = fixture.limits;
    let codec = EncoderFormatId {
        namespace_id: 42,
        value: 9,
        schema: 1,
    };
    let framing = EncoderFormatId {
        namespace_id: 42,
        value: 10,
        schema: 1,
    };
    let route = NegotiatedRoute {
        profile: VideoProfile {
            codec: FormatId {
                namespace_id: 42,
                value: 9,
                schema: 1,
            },
            framing: FormatId {
                namespace_id: 42,
                value: 10,
                schema: 1,
            },
            wire_selector: 9,
            payload_type: 99,
            encoding_name: "TEST9".into(),
            media_format: None,
            wire_attributes: Vec::new(),
        },
        video_service: "test.units/1".into(),
        framer_service: "test.framer/1".into(),
        transport_service: "test.plan/1".into(),
        clock_origin_us: 1,
        limits,
        selection: VideoUnitSelection {
            stream: 10,
            generation: 10,
            offer_id: 9,
            codec,
            framing,
            source_id: 77,
            config_generation: 1,
            clock_epoch: [1; 16],
            metadata_required: true,
            metadata_schema: Some(FrameMetadataSchemaId {
                namespace_id: 1,
                value: 1,
                schema: 1,
            }),
            source_generation: Some(9),
        },
    };
    let handles = Handles {
        video: 10,
        hdr: false,
        audio_capture: None,
        audio_encoder: None,
        media_transport: 40,
        control_transport: 50,
        stream_audio: false,
        audio_channels: 2,
        video_codec: CODEC_H264,
    };
    let mut wrong_schema = route.clone();
    wrong_schema
        .selection
        .metadata_schema
        .as_mut()
        .unwrap()
        .value = 2;
    assert!(Plane::new_planned(&runtime, handles, &Default::default(), &wrong_schema).is_err());
    let mut stale_source = route.clone();
    stale_source.selection.source_generation = Some(10);
    let mut rejected =
        Plane::new_planned(&runtime, handles, &Default::default(), &stale_source).unwrap();
    assert!(rejected.tick(true).is_err());
    assert_eq!(fixture.host.state.submit_calls, 0);
    drop(rejected);
    let mut plane = Plane::new_planned(&runtime, handles, &Default::default(), &route).unwrap();
    assert_eq!(plane.tick(true).unwrap().accepted_video_access_units, 0);
    assert_eq!(plane.tick(true).unwrap().accepted_video_access_units, 1);
    assert_eq!(
        (
            fixture.host.state.check_calls,
            fixture.host.state.submit_calls,
            fixture.host.state.retire_calls
        ),
        (2, 2, 1)
    );
    if let Some(marker) = marker {
        assert_eq!(fixture.host.state.wire, [1, 2, 3, marker, 0xb5, 1]);
    } else {
        assert_eq!(fixture.host.state.plan_calls, 1);
    }
    assert!(fixture.host.state.retired);
    drop(plane);
}

#[test]
fn managed_plane_caches_carriage_through_transport_and_retirement_backpressure() {
    exercise_managed_plane(ptr::null(), None);
}

#[test]
#[ignore = "requires separately built metadata framing provider artifacts"]
fn unchanged_managed_plane_loads_two_metadata_framing_artifacts() {
    with_two_metadata_framers(|factory, marker| exercise_managed_plane(factory, Some(marker)));
}

pub(crate) fn with_two_metadata_framers(mut run: impl FnMut(*const NativeServiceFactory, u8)) {
    for (variable, marker) in [
        ("AMATERASU_ROUTE_PROVIDER_ONE", 1),
        ("AMATERASU_ROUTE_PROVIDER_TWO", 2),
    ] {
        let path = std::env::var(variable).expect("provider artifact path");
        // SAFETY: fixture library and bundle outlive all bindings and the plane.
        unsafe {
            let library = libloading::Library::new(path).unwrap();
            let create: unsafe extern "C" fn() -> *mut c_void =
                *library.get(b"route_fixture_create").unwrap();
            let get: unsafe extern "C" fn(*mut c_void) -> *const NativeServiceFactory =
                *library.get(b"route_fixture_framer").unwrap();
            let destroy: unsafe extern "C" fn(*mut c_void) =
                *library.get(b"route_fixture_destroy").unwrap();
            let bundle = create();
            assert!(!bundle.is_null());
            run(get(bundle), marker);
            destroy(bundle);
        }
    }
}
