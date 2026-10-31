use super::*;
use amaterasu_contracts::media_profile::{FormatId, VideoProfile, VideoProfileSnapshot};
use amaterasu_gamestream::{
    rtsp::{
        Method, Request,
        negotiation::{Action, Capabilities, Negotiation, Step, TrustedLaunch},
    },
    sdp::{ColorMode, NegotiatedVideoFormat, TransportSupport},
};
use amaterasu_module_sdk::{
    RuntimeClient,
    abi::*,
    media_profile,
    video_framing::{self, FramingProvider, OwnedFramePlan},
};
use std::{ffi::c_void, mem::MaybeUninit};

unsafe extern "C" {
    fn amaterasu_runtime_v1() -> *const RuntimeApi;
}
const PROFILE: &str = "test.profile/1";
const VIDEO: &str = "test.video/1";
const FRAMER: &str = "test.framer/1";
const VIDEO_TRANSPORT: &str = "test.video-transport/1";
const MEDIA: &str = "amaterasu.media-transport/1";
const CONTROL: &str = "amaterasu.control-transport/1";
const FIXTURE: &str =
    include_str!("../../../../crates/gamestream/src/sdp/fixtures/moonlight-h264.sdp");

struct Profile(VideoProfile);
impl media_profile::Provider for Profile {
    fn snapshot(&self) -> Result<VideoProfileSnapshot, Status> {
        Ok(VideoProfileSnapshot {
            profiles: vec![self.0.clone()],
        })
    }
}
struct FramerA;
struct FramerB;
fn frame(principal: &str, bytes: &[u8], marker: u8) -> Result<OwnedFramePlan, Status> {
    if principal != "transport" {
        return Err(STATUS_INVALID_ARGUMENT);
    }
    let mut framed = bytes.to_vec();
    framed.push(marker);
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
impl FramingProvider for FramerA {
    fn plan(
        &self,
        principal: &str,
        _: &NegotiatedVideoUnit,
        bytes: &[u8],
        _: &VideoFramingLimits,
    ) -> Result<OwnedFramePlan, Status> {
        frame(principal, bytes, 1)
    }
}
impl FramingProvider for FramerB {
    fn plan(
        &self,
        principal: &str,
        _: &NegotiatedVideoUnit,
        bytes: &[u8],
        _: &VideoFramingLimits,
    ) -> Result<OwnedFramePlan, Status> {
        frame(principal, bytes, 2)
    }
}

#[derive(Clone, Copy)]
enum Kind {
    Video,
    Transport,
    Media,
    Control,
}
struct Factory {
    api: NativeServiceFactory,
    state: *mut State,
    kind: Kind,
}
struct State {
    sent: bool,
    busy: bool,
    bytes: Vec<u8>,
    source: Vec<u8>,
    codec: EncoderFormatId,
    framing: EncoderFormatId,
    framer: *const VideoFramingApi,
    limits: VideoFramingLimits,
    unit_drains: u32,
    legacy_drains: u32,
}
impl Factory {
    fn new(state: *mut State, kind: Kind) -> Box<Self> {
        let mut result = Box::new(Self {
            api: NativeServiceFactory {
                header: AbiHeader::of::<NativeServiceFactory>(),
                context: std::ptr::null_mut(),
                bind: Some(bind),
            },
            state,
            kind,
        });
        result.api.context = (&mut *result as *mut Self).cast();
        result
    }
}
#[repr(C)]
struct VideoBinding {
    api: NegotiatedVideoApi,
    state: *mut State,
}
#[repr(C)]
struct TransportBinding {
    api: NegotiatedVideoTransportApi,
    state: *mut State,
}
#[repr(C)]
struct MediaBinding {
    api: MediaTransportApi,
    state: *mut State,
}
#[repr(C)]
struct ControlBinding {
    api: ControlTransportApi,
}
unsafe extern "C" fn release_video(ctx: *mut c_void) {
    drop(unsafe { Box::from_raw(ctx.cast::<VideoBinding>()) });
}
unsafe extern "C" fn release_transport(ctx: *mut c_void) {
    drop(unsafe { Box::from_raw(ctx.cast::<TransportBinding>()) });
}
unsafe extern "C" fn release_media(ctx: *mut c_void) {
    drop(unsafe { Box::from_raw(ctx.cast::<MediaBinding>()) });
}
unsafe extern "C" fn release_control(ctx: *mut c_void) {
    drop(unsafe { Box::from_raw(ctx.cast::<ControlBinding>()) });
}
unsafe extern "C" fn bind(
    ctx: *mut c_void,
    principal: BorrowedBytes,
    query: *const NativeInterfaceQuery,
    out: *mut *const NativeInterface,
) -> Status {
    if ctx.is_null()
        || query.is_null()
        || out.is_null()
        || unsafe { principal.read() }.ok() != Some(b"consumer")
    {
        return STATUS_INVALID_ARGUMENT;
    }
    let factory = unsafe { &*ctx.cast::<Factory>() };
    let query = unsafe { &*query };
    if !query.header.supports::<NativeInterfaceQuery>()
        || query.interface_major != 1
        || query.min_minor != 0
    {
        return STATUS_INVALID_ARGUMENT;
    }
    unsafe {
        *out = std::ptr::null();
    }
    let prefix = |size: usize, release| NativeInterface {
        header: AbiHeader {
            size: size as u32,
            major: 1,
            minor: 0,
        },
        context: std::ptr::null_mut(),
        release: Some(release),
    };
    match factory.kind {
        Kind::Video => {
            if query.min_size as usize > size_of::<NegotiatedVideoApi>() {
                return STATUS_INVALID_ARGUMENT;
            }
            let mut table = Box::new(VideoBinding {
                api: NegotiatedVideoApi {
                    base: prefix(size_of::<NegotiatedVideoApi>(), release_video),
                    receive: video,
                },
                state: factory.state,
            });
            table.api.base.context = (&mut *table as *mut VideoBinding).cast();
            unsafe {
                *out = &(*Box::into_raw(table)).api.base;
            }
        }
        Kind::Transport => {
            if query.min_size as usize > size_of::<NegotiatedVideoTransportApi>() {
                return STATUS_INVALID_ARGUMENT;
            }
            let mut table = Box::new(TransportBinding {
                api: NegotiatedVideoTransportApi {
                    base: prefix(size_of::<NegotiatedVideoTransportApi>(), release_transport),
                    submit,
                    drain: unit_drain,
                },
                state: factory.state,
            });
            table.api.base.context = (&mut *table as *mut TransportBinding).cast();
            unsafe {
                *out = &(*Box::into_raw(table)).api.base;
            }
        }
        Kind::Media => {
            if query.min_size as usize > size_of::<MediaTransportApi>() {
                return STATUS_INVALID_ARGUMENT;
            }
            let mut table = Box::new(MediaBinding {
                api: MediaTransportApi {
                    base: prefix(size_of::<MediaTransportApi>(), release_media),
                    submit_video: legacy_video,
                    submit_audio: legacy_audio,
                    drain: legacy_drain,
                },
                state: factory.state,
            });
            table.api.base.context = (&mut *table as *mut MediaBinding).cast();
            unsafe {
                *out = &(*Box::into_raw(table)).api.base;
            }
        }
        Kind::Control => {
            if query.min_size as usize > size_of::<ControlTransportApi>() {
                return STATUS_INVALID_ARGUMENT;
            }
            let mut table = Box::new(ControlBinding {
                api: ControlTransportApi {
                    base: prefix(size_of::<ControlTransportApi>(), release_control),
                    send: control_send,
                    receive: control_receive,
                },
            });
            table.api.base.context = (&mut *table as *mut ControlBinding).cast();
            unsafe {
                *out = &(*Box::into_raw(table)).api.base;
            }
        }
    }
    STATUS_OK
}
unsafe extern "C" fn video(ctx: *mut c_void, stream: u64, sink: NegotiatedVideoUnitSink) -> Status {
    let binding = unsafe { &*ctx.cast::<VideoBinding>() };
    let state = unsafe { &mut *binding.state };
    if stream != 10 {
        return STATUS_INVALID_ARGUMENT;
    }
    if state.sent {
        return MEDIA_PENDING;
    }
    let unit = NegotiatedVideoUnit {
        size: size_of::<NegotiatedVideoUnit>() as u32,
        version: 1,
        stream,
        generation: 7,
        offer_id: 9,
        codec: state.codec,
        framing: state.framing,
        access_unit_id: 1,
        submission_id: 1,
        source_id: 6,
        config_generation: 8,
        timestamp_us: 100,
        clock: FrameClockMetadata {
            version: 1,
            size: size_of::<FrameClockMetadata>() as u32,
            domain: FRAME_CLOCK_STREAM_RELATIVE,
            reserved: 0,
            epoch_id: [1; 16],
        },
        flags: VIDEO_UNIT_KEYFRAME,
        reserved: 0,
        data: BorrowedBytes::new(&state.source),
    };
    let result = unsafe { (sink.write)(sink.context, &unit) };
    if result == STATUS_OK {
        state.sent = true;
    }
    result
}
unsafe extern "C" fn collect(ctx: *mut c_void, plan: *const VideoFramePlan) -> Status {
    let state = unsafe { &mut *ctx.cast::<State>() };
    let plan = unsafe { &*plan };
    if unsafe { plan.validate(&state.limits) }.is_err() {
        return STATUS_INVALID_ARGUMENT;
    }
    let bytes = unsafe { std::slice::from_raw_parts(plan.framed_bytes.ptr, plan.framed_bytes.len) };
    state.bytes.extend_from_slice(bytes);
    STATUS_OK
}
unsafe extern "C" fn submit(
    ctx: *mut c_void,
    stream: u64,
    unit: *const NegotiatedVideoUnit,
) -> Status {
    if ctx.is_null() || unit.is_null() || stream != 40 {
        return STATUS_INVALID_ARGUMENT;
    }
    let state_ptr = unsafe { (*ctx.cast::<TransportBinding>()).state };
    let (api, limits) = {
        let state = unsafe { &mut *state_ptr };
        if state.busy {
            state.busy = false;
            return MEDIA_BUSY;
        }
        (state.framer, state.limits)
    };
    let sink = VideoFramePlanSink {
        context: state_ptr.cast(),
        write: collect,
    };
    unsafe { ((*api).plan)((*api).base.context, 10, unit, &limits, sink) }
}
unsafe extern "C" fn unit_drain(ctx: *mut c_void, _: u64) -> Status {
    let state = unsafe { &mut *(*ctx.cast::<TransportBinding>()).state };
    state.unit_drains += 1;
    STATUS_OK
}
unsafe extern "C" fn legacy_drain(ctx: *mut c_void, _: u64) -> Status {
    let state = unsafe { &mut *(*ctx.cast::<MediaBinding>()).state };
    state.legacy_drains += 1;
    if state.legacy_drains == 1 {
        MEDIA_BUSY
    } else {
        STATUS_OK
    }
}
unsafe extern "C" fn legacy_video(_: *mut c_void, _: u64, _: *const EncodedPacket) -> Status {
    STATUS_INVALID_ARGUMENT
}
unsafe extern "C" fn legacy_audio(_: *mut c_void, _: u64, _: *const EncodedAudioPacket) -> Status {
    STATUS_OK
}
unsafe extern "C" fn control_send(_: *mut c_void, _: u64, _: *const ControlMessage) -> Status {
    STATUS_OK
}
unsafe extern "C" fn control_receive(_: *mut c_void, _: u64, _: ControlMessageSink) -> Status {
    MEDIA_PENDING
}

fn request(method: Method, target: &str, cseq: u32, session: bool) -> Request {
    Request {
        method,
        target: target.into(),
        cseq,
        headers: if session {
            vec![("session".into(), "session-1".into())]
        } else {
            vec![]
        },
        body: vec![],
    }
}
fn client_selects(
    offer: VideoProfileSnapshot,
    selector: u32,
) -> Result<VideoProfile, &'static str> {
    let mut machine = Negotiation::with_video_profiles(
        TrustedLaunch {
            epoch: 7,
            rtsp_target: "rtspenc://127.0.0.1:48010/launch".into(),
            session_token: "session-1".into(),
            encrypted_rtsp: true,
            audio_port: 49000,
            video_port: 49002,
            control_port: 49004,
            control_connect_data: 123,
            udp_discovery: None,
        },
        Capabilities {
            transport: TransportSupport {
                hevc: false,
                av1: false,
                stereo_audio: true,
                enet_control: true,
                encrypted_control: true,
                control_v2: true,
                encrypted_remote_input: false,
                encrypted_video: true,
                encrypted_audio: true,
                fec: true,
            },
            supported_colors: vec![ColorMode::Bt709Limited],
            max_width: 1920,
            max_height: 1080,
            max_fps: 60,
            max_bitrate_kbps: 20000,
            require_video_encryption: false,
            require_audio_encryption: false,
        },
        offer.clone(),
        offer,
    )
    .map_err(|_| "profile_unavailable")?;
    let target = "rtspenc://127.0.0.1:48010/launch";
    machine
        .handle(7, &request(Method::Options, target, 1, false))
        .map_err(|_| "options")?;
    let mut describe = request(Method::Describe, target, 2, false);
    describe
        .headers
        .push(("accept".into(), "application/sdp".into()));
    let Step::Reply(reply) = machine.handle(7, &describe).map_err(|_| "describe")? else {
        return Err("describe");
    };
    if !String::from_utf8(reply)
        .map_err(|_| "describe")?
        .contains("/90000")
    {
        return Err("describe");
    }
    for (cseq, target) in [
        (3, "streamid=audio/0/0"),
        (4, "streamid=video/0/0"),
        (5, "streamid=control/13/0"),
    ] {
        let mut setup = request(Method::Setup, target, cseq, cseq > 3);
        setup.headers.push((
            "transport".into(),
            "unicast;X-GS-ClientPort=50000-50001".into(),
        ));
        machine.handle(7, &setup).map_err(|_| "setup")?;
    }
    let mut announce = request(Method::Announce, "streamid=control/13/0", 6, true);
    announce
        .headers
        .push(("content-type".into(), "application/sdp".into()));
    announce.body = FIXTURE
        .replace(
            "x-ss-general.encryptionEnabled:0",
            "x-ss-general.encryptionEnabled:1",
        )
        .replace(
            "x-nv-vqos[0].bitStreamFormat:0",
            &format!("x-nv-vqos[0].bitStreamFormat:{selector}"),
        )
        .into_bytes();
    let Step::Action {
        ticket,
        action: Action::ExtensionAnnounce(value),
    } = machine.handle(7, &announce).map_err(|_| "announce")?
    else {
        return Err("announce");
    };
    let NegotiatedVideoFormat::Extension(profile) = value.video.format else {
        return Err("profile");
    };
    machine.complete(7, ticket, true).map_err(|_| "complete")?;
    Ok(profile)
}
fn exercise<P: FramingProvider>(framer: P, selector: u32, marker: u8) {
    std::hint::black_box(size_of::<amaterasu_runtime::Broker>());
    let selections = [
        (PROFILE, media_profile::CONTRACT),
        (VIDEO, video_framing::VIDEO_UNIT_CONTRACT),
        (FRAMER, video_framing::FRAMING_CONTRACT),
        (VIDEO_TRANSPORT, video_framing::VIDEO_TRANSPORT_CONTRACT),
        (MEDIA, amaterasu_module_sdk::transport::MEDIA_CONTRACT),
        (CONTROL, amaterasu_module_sdk::transport::CONTROL_CONTRACT),
    ];
    let services: Vec<_> = selections.iter().map(|(service, _)| *service).collect();
    let native: serde_json::Map<String, serde_json::Value> = selections
        .iter()
        .map(|(service, contract)| {
            (
                service.to_string(),
                serde_json::json!({"provider":"provider","contract":contract}),
            )
        })
        .collect();
    let policy = serde_json::json!({ "principals": { "provider": {"native_provide":services}, "consumer": {"native_use":[PROFILE, VIDEO, VIDEO_TRANSPORT, MEDIA, CONTROL]}, "transport": {"native_use":[FRAMER]} }, "service_selections": {"native":native} });
    let policy = serde_json::to_vec(&policy).unwrap();
    let runtime_api = unsafe { &*amaterasu_runtime_v1() };
    let mut runtime = std::ptr::null_mut();
    assert_eq!(
        unsafe { (runtime_api.create)(BorrowedBytes::new(&policy), &mut runtime) },
        STATUS_OK
    );
    let endpoint = |name: &str| {
        let mut output = MaybeUninit::uninit();
        assert_eq!(
            unsafe {
                (runtime_api.endpoint)(
                    runtime,
                    BorrowedBytes::new(name.as_bytes()),
                    output.as_mut_ptr(),
                )
            },
            STATUS_OK
        );
        unsafe { RuntimeClient::from_endpoint(output.assume_init()) }
    };
    {
        let provider = endpoint("provider");
        let consumer = endpoint("consumer");
        let transport = endpoint("transport");
        let id = |value| FormatId {
            namespace_id: 42,
            value,
            schema: 1,
        };
        let profile = VideoProfile {
            codec: id(u64::from(selector)),
            framing: id(10),
            wire_selector: selector,
            payload_type: 99,
            encoding_name: format!("TEST{selector}"),
        };
        let profile_factory = media_profile::Factory::new(Profile(profile.clone()));
        unsafe { media_profile::register(&provider, PROFILE, &profile_factory.api) }.unwrap();
        let profile_binding = unsafe { media_profile::bind(&consumer, PROFILE) }.unwrap();
        let offer = unsafe { media_profile::read(&profile_binding) }.unwrap();
        assert_eq!(
            client_selects(offer.clone(), selector + 10),
            Err("announce")
        );
        let accepted = client_selects(offer, selector).unwrap();
        assert_eq!(accepted, profile);
        let codec = EncoderFormatId {
            namespace_id: accepted.codec.namespace_id,
            value: accepted.codec.value,
            schema: accepted.codec.schema,
        };
        let framing = EncoderFormatId {
            namespace_id: accepted.framing.namespace_id,
            value: accepted.framing.value,
            schema: accepted.framing.schema,
        };
        let framer_factory = video_framing::Factory::new(framer);
        unsafe { video_framing::register(&provider, FRAMER, &framer_factory) }.unwrap();
        let framer_binding = unsafe { video_framing::bind(&transport, FRAMER) }.unwrap();
        let framer_api = unsafe { framer_binding.table::<VideoFramingApi>() }.unwrap() as *const _;
        let limits = VideoFramingLimits {
            size: size_of::<VideoFramingLimits>() as u32,
            version: VIDEO_FRAMING_LIMITS_VERSION,
            packet_size: 1024,
            crypto_overhead: 0,
            network_overhead: 28,
            allowed_extensions: 0,
            max_datagrams: 4,
            max_queue_bytes: 8192,
            min_parity_shards: 0,
            min_fec_percentage: 0,
            max_fec_percentage: 0,
            max_fec_blocks: 1,
            fec_policy_flags: 0,
            reserved: 0,
            max_wire_bytes: 8192,
        };
        let mut state = Box::new(State {
            sent: false,
            busy: true,
            bytes: Vec::new(),
            source: vec![10, 20, 30],
            codec,
            framing,
            framer: framer_api,
            limits,
            unit_drains: 0,
            legacy_drains: 0,
        });
        let state_ptr = &mut *state as *mut State;
        let video_factory = Factory::new(state_ptr, Kind::Video);
        let transport_factory = Factory::new(state_ptr, Kind::Transport);
        let media_factory = Factory::new(state_ptr, Kind::Media);
        let control_factory = Factory::new(state_ptr, Kind::Control);
        for (service, contract, factory) in [
            (
                VIDEO,
                video_framing::VIDEO_UNIT_CONTRACT,
                &video_factory.api,
            ),
            (
                VIDEO_TRANSPORT,
                video_framing::VIDEO_TRANSPORT_CONTRACT,
                &transport_factory.api,
            ),
            (
                MEDIA,
                amaterasu_module_sdk::transport::MEDIA_CONTRACT,
                &media_factory.api,
            ),
            (
                CONTROL,
                amaterasu_module_sdk::transport::CONTROL_CONTRACT,
                &control_factory.api,
            ),
        ] {
            unsafe { provider.register_native_typed(service, contract, factory) }.unwrap();
        }
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
        let selection = VideoUnitSelection {
            stream: 10,
            generation: 7,
            offer_id: 9,
            codec,
            framing,
            source_id: 6,
            config_generation: 8,
            clock_epoch: [1; 16],
            metadata_required: false,
        };
        let mut missing_metadata = selection;
        missing_metadata.metadata_required = true;
        assert!(
            Plane::new_negotiated(
                &consumer,
                handles,
                &Default::default(),
                VIDEO,
                VIDEO_TRANSPORT,
                missing_metadata
            )
            .is_err()
        );
        let mut plane = Plane::new_negotiated(
            &consumer,
            handles,
            &Default::default(),
            VIDEO,
            VIDEO_TRANSPORT,
            selection,
        )
        .unwrap();
        let first = plane.tick(true).unwrap();
        assert_eq!(first.accepted_video_access_units, 0);
        assert!(state.bytes.is_empty());
        let second = plane.tick(true).unwrap();
        assert_eq!(second.accepted_video_access_units, 1);
        assert_eq!(state.bytes, [10, 20, 30, marker]);
        assert_eq!(plane.tick(true).unwrap().accepted_video_access_units, 0);
        assert!(!plane.pump.backend.drain_transport().unwrap());
        assert!(plane.pump.backend.drain_transport().unwrap());
        assert_eq!((state.unit_drains, state.legacy_drains), (1, 2));
        drop(plane);
        drop(framer_binding);
        drop(profile_binding);
    }
    unsafe { (runtime_api.destroy)(runtime) };
}
#[test]
fn production_pump_retains_units_and_swaps_runtime_providers_after_client_acceptance() {
    exercise(FramerA, 3, 1);
    exercise(FramerB, 4, 2);
}

#[test]
fn negotiated_unit_copy_requires_exact_selection_and_owns_payload() {
    let codec = EncoderFormatId {
        namespace_id: 1,
        value: 2,
        schema: 1,
    };
    let framing = EncoderFormatId {
        namespace_id: 1,
        value: 3,
        schema: 1,
    };
    let selection = VideoUnitSelection {
        stream: 10,
        generation: 7,
        offer_id: 9,
        codec,
        framing,
        source_id: 6,
        config_generation: 8,
        clock_epoch: [1; 16],
        metadata_required: false,
    };
    let mut bytes = vec![10, 20, 30];
    let unit = NegotiatedVideoUnit {
        size: size_of::<NegotiatedVideoUnit>() as u32,
        version: 1,
        stream: 10,
        generation: 7,
        offer_id: 9,
        codec,
        framing,
        access_unit_id: 1,
        submission_id: 1,
        source_id: 6,
        config_generation: 8,
        timestamp_us: 100,
        clock: FrameClockMetadata {
            version: 1,
            size: size_of::<FrameClockMetadata>() as u32,
            domain: FRAME_CLOCK_STREAM_RELATIVE,
            reserved: 0,
            epoch_id: [1; 16],
        },
        flags: VIDEO_UNIT_KEYFRAME,
        reserved: 0,
        data: BorrowedBytes::new(&bytes),
    };
    // SAFETY: bytes remains live and immutable for the synchronous copy.
    let packet = unsafe { VideoUnitPacket::copy(&unit, &selection) }.unwrap();
    for altered in [
        NegotiatedVideoUnit { stream: 11, ..unit },
        NegotiatedVideoUnit {
            generation: 8,
            ..unit
        },
        NegotiatedVideoUnit {
            offer_id: 10,
            ..unit
        },
        NegotiatedVideoUnit {
            codec: framing,
            ..unit
        },
        NegotiatedVideoUnit {
            framing: codec,
            ..unit
        },
        NegotiatedVideoUnit {
            source_id: 7,
            ..unit
        },
        NegotiatedVideoUnit {
            config_generation: 9,
            ..unit
        },
        NegotiatedVideoUnit {
            clock: FrameClockMetadata {
                epoch_id: [2; 16],
                ..unit.clock
            },
            ..unit
        },
        NegotiatedVideoUnit {
            flags: VIDEO_UNIT_END_OF_STREAM,
            ..unit
        },
    ] {
        // SAFETY: the unchanged borrowed payload remains live during the call.
        assert!(unsafe { VideoUnitPacket::copy(&altered, &selection) }.is_err());
    }
    bytes.fill(0);
    let header = packet.header();
    // SAFETY: packet owns this payload and remains alive for the read.
    assert_eq!(
        unsafe { std::slice::from_raw_parts(header.data.ptr, header.data.len) },
        [10, 20, 30]
    );
}
