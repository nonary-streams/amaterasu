//! Full managed route with encrypted loopback RTSP and synthetic non-codec bytes.
use super::*;
use crate::{
    StreamSession,
    pipeline::tests::harness::{Broker, Call},
};
use amaterasu_gamestream::crypto::{Direction, RtspOpener, RtspSealer};
use amaterasu_module_sdk::{Command, ManagedModule, Response};
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    net::{TcpStream, UdpSocket},
    time::{Duration, Instant},
};

const PLAN: &str = "test.plan/1";
const ENCODER_CONTROL: &str = "test.encoder/1";
const ENCODER_NATIVE: &str = "test.encoder-native/1";
const VIDEO_CONTROL: &str = "amaterasu.video/1";
const SESSION: &str = "amaterasu.session/1";
const STATE: &str = "amaterasu.state/1";
const RTSP: &str = "amaterasu.rtsp/1";
const STREAM: &str = "amaterasu.stream-session/1";
const LAUNCH: &str = "amaterasu.launch/1";
const DISPLAY: &str = "amaterasu.display/1";
const CAPTURE: &str = "test.capture/1";
const RESOURCE: &str = "test.resource/1";
const PROCESSOR: &str = "test.processor/1";
const CAPTURE_CAPS: &str = "test.capture-caps/1";
const PROCESSOR_CAPS: &str = "test.processor-caps/1";
const ENCODER_CAPS: &str = "test.encoder-caps/1";
const BUILTIN_ENCODE: &str = "amaterasu.encode/1";

struct Fixture {
    launcher: RuntimeClient,
    nvhttp: RuntimeClient,
    rtsp_owner: RuntimeClient,
    provider: RuntimeClient,
    rtsp: amaterasu_rtsp::test_support::ManagedRtsp,
    session: StreamSession,
    video: amaterasu_video::test_support::VideoModule,
    transport: amaterasu_media_transport::test_support::ManagedTransport,
    broker: Broker,
    _dynamic_provider: DynamicProvider,
    consumer: RuntimeClient,
    marker: u8,
    _control: Box<Factory>,
    selected_encoder: String,
    opened_encoders: usize,
    encoder_live: bool,
    closed_encoders: usize,
    metadata: bool,
    probes: Vec<Value>,
    _guard: RuntimeGuard,
}
impl Fixture {
    fn tick(&mut self) {
        self.rtsp.tick().unwrap();
        self.session.tick().unwrap();
        self.video.tick().unwrap();
        self.transport.tick().unwrap();
        for _ in 0..16 {
            let Response::Request {
                id,
                service,
                payload,
                ..
            } = self.provider.send(Command::Poll {}).unwrap()
            else {
                break;
            };
            let result = if service == LAUNCH {
                assert_eq!(payload["action"], "verify_display_admission");
                json!({"ok":true,"verified":true,"rtsp_operation_id":payload["rtsp_operation_id"],
                    "request_sha256":payload["request_sha256"],"preflight_remaining_ms":10_000})
            } else if service == ENCODER_CONTROL {
                match payload["action"].as_str().unwrap_or("") {
                    "probe" if self.metadata => {
                        self.probes.push(payload.clone());
                        json!({"ok":true,"settled":true,"usable":true,"codec":"h264",
                            "encoder":payload["encoder"],"configuration":payload["configuration"]})
                    }
                    "probe_source" => {
                        self.probes.push(payload.clone());
                        json!({"ok":true,"settled":true,"usable":payload["encoder"]["clsid"] == self.selected_encoder,
                            "source_id":payload["source_id"],"encoder":payload["encoder"],"configuration":payload["configuration"]})
                    }
                    "open" => {
                        assert_eq!(payload["encoder"]["clsid"], self.selected_encoder);
                        self.opened_encoders += 1;
                        self.encoder_live = true;
                        json!({"ok":true,"session_id":77,"packet_boundaries":true,"max_reference_frames":0,
                            "extension_profile":payload["configuration"]["extension_profile"],
                            "resource_output_clock":payload["resource_output_clock"],
                            "resource_callback_contract":payload["resource_callback_contract"],
                            "resource_sequence_policy":"strict_source_echo"})
                    }
                    "lookup" if self.encoder_live => json!({"ok":true,"session_id":77}),
                    "lookup" => json!({"ok":false,"code":"not_found"}),
                    "close" => {
                        self.encoder_live = false;
                        self.closed_encoders += 1;
                        json!({"ok":true,"session_id":77})
                    }
                    "drain" => json!({"ok":true,"session_id":77}),
                    _ => json!({"ok":false,"code":"unsupported"}),
                }
            } else if service == CAPTURE {
                match payload["action"].as_str().unwrap_or("") {
                    "open" => {
                        json!({"ok":true,"stream":1,"resource_capture_clock":payload["resource_capture_clock"],"resource_callback_contract":"cross_service_safe"})
                    }
                    "lookup" | "close" => json!({"ok":true,"stream":1}),
                    _ => json!({"ok":false,"code":"unsupported"}),
                }
            } else {
                self.broker.execute(&Call {
                    service,
                    payload,
                    parent: None,
                })
            };
            self.provider
                .require_success(Command::Complete {
                    request: id,
                    payload: result,
                })
                .unwrap();
        }
    }
    fn request(&mut self, service: &str, operation: &str, payload: Value) -> Value {
        let Response::Accepted { id } = self
            .launcher
            .send(Command::Request {
                service: service.into(),
                payload,
                operation_id: operation.into(),
                timeout_ms: 5000,
                parent: None,
            })
            .unwrap()
        else {
            panic!("request rejected");
        };
        let deadline = Instant::now() + Duration::from_secs(4);
        loop {
            self.tick();
            match self
                .launcher
                .send(Command::TakeResult { request: id })
                .unwrap()
            {
                Response::Result { payload } => {
                    self.launcher
                        .require_success(Command::ReleaseResult { request: id })
                        .unwrap();
                    return payload;
                }
                Response::Pending => {}
                result => panic!("unexpected result {result:?}"),
            }
            assert!(Instant::now() < deadline, "request {operation} timed out");
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    fn nvhttp_request(&mut self, service: &str, operation: &str, payload: Value) -> Value {
        let Response::Accepted { id } = self
            .nvhttp
            .send(Command::Request {
                service: service.into(),
                payload,
                operation_id: operation.into(),
                timeout_ms: 5000,
                parent: None,
            })
            .unwrap()
        else {
            panic!("NVHTTP request rejected")
        };
        let deadline = Instant::now() + Duration::from_secs(4);
        loop {
            self.tick();
            match self
                .nvhttp
                .send(Command::TakeResult { request: id })
                .unwrap()
            {
                Response::Result { payload } => {
                    self.nvhttp
                        .require_success(Command::ReleaseResult { request: id })
                        .unwrap();
                    return payload;
                }
                Response::Pending => {}
                result => panic!("unexpected NVHTTP result {result:?}"),
            }
            assert!(Instant::now() < deadline, "NVHTTP request timed out");
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    fn video_status(&mut self) -> Value {
        let Response::Accepted { id } = self
            .consumer
            .send(Command::Request {
                service: VIDEO_CONTROL.into(),
                payload: json!({"action":"status"}),
                operation_id: "fixture-video-status".into(),
                timeout_ms: 5000,
                parent: None,
            })
            .unwrap()
        else {
            panic!("status rejected")
        };
        for _ in 0..1000 {
            self.tick();
            match self
                .consumer
                .send(Command::TakeResult { request: id })
                .unwrap()
            {
                Response::Result { payload } => {
                    self.consumer
                        .require_success(Command::ReleaseResult { request: id })
                        .unwrap();
                    return payload;
                }
                Response::Pending => (),
                other => panic!("status result {other:?}"),
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        panic!("video status timed out")
    }
    fn transaction(
        &mut self,
        address: &str,
        sealer: &mut RtspSealer,
        opener: &mut RtspOpener,
        request: String,
    ) -> String {
        let mut client = TcpStream::connect(address).unwrap();
        client
            .write_all(&sealer.seal(request.as_bytes()).unwrap())
            .unwrap();
        client.set_nonblocking(true).unwrap();
        let timeout = if request.starts_with("TEARDOWN ") {
            Duration::from_secs(20)
        } else {
            Duration::from_secs(4)
        };
        let deadline = Instant::now() + timeout;
        let mut bytes = Vec::new();
        loop {
            self.tick();
            let mut buffer = [0; 8192];
            match client.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => bytes.extend_from_slice(&buffer[..n]),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(error) => panic!("RTSP read failed: {error}"),
            }
            if Instant::now() >= deadline {
                panic!(
                    "RTSP response timed out; video status: {}",
                    self.video_status()
                );
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        String::from_utf8(opener.open(&bytes).unwrap().plaintext).unwrap()
    }
}

// Each artifact owns its factory and is kept loaded until all module bindings
// and queues have retired. The native descriptor pointers are borrowed only.
struct DynamicProvider {
    bundle: *mut c_void,
    destroy: unsafe extern "C" fn(*mut c_void),
    library: libloading::Library,
}
impl DynamicProvider {
    unsafe fn load(path: &str) -> Self {
        let library = unsafe { libloading::Library::new(path) }.expect("fixture provider library");
        let create: unsafe extern "C" fn() -> *mut c_void =
            *unsafe { library.get(b"route_fixture_create") }.unwrap();
        let destroy = *unsafe { library.get(b"route_fixture_destroy") }.unwrap();
        let bundle = unsafe { create() };
        assert!(!bundle.is_null());
        Self {
            bundle,
            destroy,
            library,
        }
    }
    unsafe fn factory(&self, symbol: &[u8]) -> &NativeServiceFactory {
        let get: unsafe extern "C" fn(*mut c_void) -> *const NativeServiceFactory =
            *unsafe { self.library.get(symbol) }.unwrap();
        let pointer = unsafe { get(self.bundle) };
        assert!(!pointer.is_null());
        unsafe { &*pointer }
    }
}
impl Drop for DynamicProvider {
    fn drop(&mut self) {
        unsafe { (self.destroy)(self.bundle) };
    }
}

struct RuntimeGuard {
    api: &'static RuntimeApi,
    runtime: *mut c_void,
}
impl Drop for RuntimeGuard {
    fn drop(&mut self) {
        unsafe { (self.api.destroy)(self.runtime) };
    }
}
fn fixture(marker: u8, encoder: &str, transport_name: &str) -> Fixture {
    fixture_with_metadata(marker, encoder, transport_name, false)
}
fn fixture_with_metadata(
    marker: u8,
    encoder: &str,
    transport_name: &str,
    metadata: bool,
) -> Fixture {
    let profile = json!({"codec":{"namespace_id":42,"value":9,"schema":1},
        "framing":{"namespace_id":42,"value":10,"schema":1},"wire_selector":9,"payload_type":99,"encoding_name":"TEST9"});
    let mut transport_profile = profile.clone();
    if transport_name == "PAYLOAD" {
        transport_profile["payload_type"] = json!(100);
    } else {
        transport_profile["encoding_name"] = json!(transport_name);
    }
    let mut native: serde_json::Map<String, Value> = [
        (VIDEO, video_framing::VIDEO_UNIT_CONTRACT, "video"),
        (
            VIDEO_CONTROL,
            amaterasu_module_sdk::video::CONTRACT,
            "video",
        ),
        (
            ENCODER_NATIVE,
            amaterasu_module_sdk::media::native_contracts::ENCODER_PROVIDER,
            "provider",
        ),
        (FRAMER, video_framing::FRAMING_CONTRACT, "provider"),
        (
            PLAN,
            video_framing::VIDEO_PLAN_TRANSPORT_V2_CONTRACT,
            "transport",
        ),
        (
            MEDIA,
            amaterasu_module_sdk::transport::MEDIA_CONTRACT,
            "transport",
        ),
        (
            CONTROL,
            amaterasu_module_sdk::transport::CONTROL_CONTRACT,
            "provider",
        ),
    ]
    .iter()
    .map(|(service, contract, provider)| {
        (
            (*service).into(),
            json!({"provider":provider,"contract":contract}),
        )
    })
    .collect();
    if metadata {
        native.insert(BUILTIN_ENCODE.into(), json!({"provider":"provider","contract":amaterasu_module_sdk::media::native_contracts::ENCODER}));
        native.insert(ENCODER_CONTROL.into(), json!({"provider":"provider","contract":amaterasu_module_sdk::media::native_contracts::ENCODER}));
        for (service, contract) in [
            (
                CAPTURE,
                amaterasu_module_sdk::media::native_contracts::CAPTURE,
            ),
            (
                RESOURCE,
                amaterasu_module_sdk::media::native_contracts::FRAME_RESOURCE_CONSUMER,
            ),
            (
                PROCESSOR,
                amaterasu_module_sdk::media::native_contracts::FRAME_RESOURCE_PROCESSOR,
            ),
            (
                CAPTURE_CAPS,
                amaterasu_module_sdk::media::metadata::CAPABILITIES_CONTRACT,
            ),
            (
                PROCESSOR_CAPS,
                amaterasu_module_sdk::media::metadata::CAPABILITIES_CONTRACT,
            ),
            (
                ENCODER_CAPS,
                amaterasu_module_sdk::media::metadata::CAPABILITIES_CONTRACT,
            ),
        ] {
            native.insert(
                service.into(),
                json!({"provider":"provider","contract":contract}),
            );
        }
    }
    let all_calls = [
        RTSP,
        STREAM,
        SESSION,
        STATE,
        MEDIA,
        CONTROL,
        VIDEO_CONTROL,
        ENCODER_CONTROL,
        LAUNCH,
        DISPLAY,
    ];
    let mut settings = json!({"principals":{
        "provider":{"provide":[SESSION,STATE,CONTROL,ENCODER_CONTROL,LAUNCH,DISPLAY],"native_provide":[ENCODER_NATIVE,FRAMER,CONTROL]},
        "transport":{"provide":[MEDIA],"native_provide":[MEDIA,PLAN]},
        "video":{"provide":[VIDEO_CONTROL],"call":[ENCODER_CONTROL],"native_provide":[VIDEO,VIDEO_CONTROL],"native_use":[ENCODER_NATIVE]},
        "consumer":{"provide":[STREAM],"call":all_calls,"native_use":[VIDEO,FRAMER,PLAN,MEDIA,CONTROL]},
        "amaterasu.rtsp":{"provide":[RTSP],"call":all_calls},
        "launcher":{"call":all_calls},"amaterasu.nvhttp":{"call":all_calls}},
        "settings":{
            "transport":{"enabled":true,"allowed_principals":["consumer"],"plan_service_v2":PLAN,"extension_profiles":[transport_profile]},
            "video":{"enabled":true,"capture_providers":[],"unit_service":VIDEO,"source_encoder_providers":{"fixture":{
                "control_service":ENCODER_CONTROL,"native_service":ENCODER_NATIVE,"source_ids":[6],"extension_profiles":[profile]}}},
            "consumer":{"enabled":true,"allowed_principals":["amaterasu.rtsp"],"extension_route":{
                "profile":profile,"source_id":6,"encoder_provider":"fixture","encoder":{"clsid":encoder,"friendly_name":"Source","hardware_url":null},
                "video_unit_service":VIDEO,"framer_service":FRAMER,"plan_transport_service":PLAN}},
            "amaterasu.rtsp":{"enabled":true,"extension_enabled":true,"bind":"127.0.0.1:0","allowed_principals":["launcher","amaterasu.nvhttp"]}},
        "service_selections":{"native":native}});
    if metadata {
        let schema = json!({"namespace_id":1,"value":1,"schema":1});
        for principal in ["consumer", "amaterasu.rtsp", "launcher", "amaterasu.nvhttp"] {
            settings["principals"][principal]["call"]
                .as_array_mut()
                .unwrap()
                .push(json!(CAPTURE));
        }
        settings["principals"]["provider"]["provide"]
            .as_array_mut()
            .unwrap()
            .push(json!(CAPTURE));
        for service in [
            CAPTURE,
            RESOURCE,
            PROCESSOR,
            CAPTURE_CAPS,
            PROCESSOR_CAPS,
            ENCODER_CAPS,
            ENCODER_CONTROL,
            BUILTIN_ENCODE,
        ] {
            settings["principals"]["provider"]["native_provide"]
                .as_array_mut()
                .unwrap()
                .push(json!(service));
            settings["principals"]["video"]["native_use"]
                .as_array_mut()
                .unwrap()
                .push(json!(service));
        }
        settings["principals"]["video"]["call"]
            .as_array_mut()
            .unwrap()
            .push(json!(CAPTURE));
        settings["settings"]["transport"]["metadata_profiles"] =
            json!([{"profile":profile,"schema":schema}]);
        settings["settings"]["video"]["external_capture_providers"] =
            json!({"fixture_capture":{"service":CAPTURE,"frame_transport":"generic_cpu"}});
        settings["settings"]["video"]["external_encoder_providers"] =
            json!({"fixture":ENCODER_CONTROL});
        settings["settings"]["video"]["external_encoder_consumers"] = json!({"fixture":RESOURCE});
        settings["settings"]["video"]["generic_processor_service"] = json!(PROCESSOR);
        // The source-mode registration shares the control service but must use
        // a distinct alias from the submitted resource consumer registration.
        let source_registration =
            settings["settings"]["video"]["source_encoder_providers"]["fixture"].clone();
        settings["settings"]["video"]["source_encoder_providers"]
            .as_object_mut()
            .unwrap()
            .remove("fixture");
        settings["settings"]["video"]["source_encoder_providers"]["source_fixture"] =
            source_registration;
        settings["settings"]["video"]["source_encoder_providers"]["source_fixture"]["source_ids"] =
            json!([1]);
        settings["settings"]["video"]["metadata_route"] = json!({"profile":profile,"schema":schema,"capture_service":CAPTURE,
            "encoder_control_service":ENCODER_CONTROL,"capture_capabilities":CAPTURE_CAPS,"processor_capabilities":PROCESSOR_CAPS,"encoder_capabilities":ENCODER_CAPS});
        settings["settings"]["consumer"]["extension_route"]["source_id"] = json!(0);
        settings["settings"]["consumer"]["extension_route"]["metadata_schema"] = schema;
    }
    let api = unsafe { &*amaterasu_runtime_v1() };
    let mut runtime = std::ptr::null_mut();
    assert_eq!(
        unsafe {
            (api.create)(
                BorrowedBytes::new(&serde_json::to_vec(&settings).unwrap()),
                &mut runtime,
            )
        },
        STATUS_OK
    );
    let endpoint = |name: &str| {
        let mut out = MaybeUninit::uninit();
        assert_eq!(
            unsafe {
                (api.endpoint)(
                    runtime,
                    BorrowedBytes::new(name.as_bytes()),
                    out.as_mut_ptr(),
                )
            },
            STATUS_OK
        );
        unsafe { RuntimeClient::from_endpoint(out.assume_init()) }
    };
    let provider = endpoint("provider");
    for service in [SESSION, STATE, CONTROL, ENCODER_CONTROL, LAUNCH, DISPLAY] {
        provider
            .require_success(Command::Register {
                service: service.into(),
            })
            .unwrap();
    }
    if metadata {
        provider
            .require_success(Command::Register {
                service: CAPTURE.into(),
            })
            .unwrap();
    }
    let control = Factory::new(std::ptr::null_mut(), Kind::Control);
    let path = std::env::var(if marker == 1 {
        "AMATERASU_ROUTE_PROVIDER_ONE"
    } else {
        "AMATERASU_ROUTE_PROVIDER_TWO"
    })
    .expect("build fixture providers and provide their absolute paths");
    let dynamic_provider = unsafe { DynamicProvider::load(&path) };
    unsafe {
        provider
            .register_native_typed(
                ENCODER_NATIVE,
                amaterasu_module_sdk::media::native_contracts::ENCODER_PROVIDER,
                dynamic_provider.factory(if metadata {
                    b"route_fixture_submitted_encoder"
                } else {
                    b"route_fixture_encoder"
                }),
            )
            .unwrap();
        if metadata {
            for (service, contract, symbol) in [
                (
                    CAPTURE,
                    amaterasu_module_sdk::media::native_contracts::CAPTURE,
                    b"route_fixture_submitted_capture".as_slice(),
                ),
                (
                    RESOURCE,
                    amaterasu_module_sdk::media::native_contracts::FRAME_RESOURCE_CONSUMER,
                    b"route_fixture_submitted_consumer".as_slice(),
                ),
                (
                    PROCESSOR,
                    amaterasu_module_sdk::media::native_contracts::FRAME_RESOURCE_PROCESSOR,
                    b"route_fixture_submitted_processor".as_slice(),
                ),
                (
                    CAPTURE_CAPS,
                    amaterasu_module_sdk::media::metadata::CAPABILITIES_CONTRACT,
                    b"route_fixture_submitted_capabilities".as_slice(),
                ),
                (
                    PROCESSOR_CAPS,
                    amaterasu_module_sdk::media::metadata::CAPABILITIES_CONTRACT,
                    b"route_fixture_submitted_capabilities".as_slice(),
                ),
                (
                    ENCODER_CAPS,
                    amaterasu_module_sdk::media::metadata::CAPABILITIES_CONTRACT,
                    b"route_fixture_submitted_capabilities".as_slice(),
                ),
                (
                    ENCODER_CONTROL,
                    amaterasu_module_sdk::media::native_contracts::ENCODER,
                    b"route_fixture_submitted_control".as_slice(),
                ),
                (
                    BUILTIN_ENCODE,
                    amaterasu_module_sdk::media::native_contracts::ENCODER,
                    b"route_fixture_submitted_control".as_slice(),
                ),
            ] {
                provider
                    .register_native_typed(service, contract, dynamic_provider.factory(symbol))
                    .unwrap();
            }
        }
        provider
            .register_native_typed(
                CONTROL,
                amaterasu_module_sdk::transport::CONTROL_CONTRACT,
                &control.api,
            )
            .unwrap();
        provider
            .register_native_typed(
                FRAMER,
                video_framing::FRAMING_CONTRACT,
                dynamic_provider.factory(b"route_fixture_framer"),
            )
            .unwrap();
    }
    let transport =
        amaterasu_media_transport::test_support::ManagedTransport::start(endpoint("transport"))
            .unwrap();
    if metadata {
        let runtime = endpoint("video");
        for (service, contract, minor, size) in [
            (
                CAPTURE,
                amaterasu_module_sdk::media::native_contracts::CAPTURE,
                0,
                amaterasu_media::CAPTURE_API_V1_0_SIZE,
            ),
            (
                ENCODER_CONTROL,
                amaterasu_module_sdk::media::native_contracts::ENCODER,
                0,
                size_of::<amaterasu_media::EncoderApi>() as u32,
            ),
            (
                ENCODER_NATIVE,
                amaterasu_module_sdk::media::native_contracts::ENCODER_PROVIDER,
                amaterasu_media::ENCODER_PROVIDER_INTERFACE_MINOR,
                amaterasu_media::ENCODER_PROVIDER_API_V1_0_SIZE,
            ),
            (
                RESOURCE,
                amaterasu_module_sdk::media::native_contracts::FRAME_RESOURCE_CONSUMER,
                0,
                amaterasu_media::FRAME_RESOURCE_CONSUMER_API_V1_0_SIZE,
            ),
            (
                PROCESSOR,
                amaterasu_module_sdk::media::native_contracts::FRAME_RESOURCE_PROCESSOR,
                amaterasu_media::FRAME_RESOURCE_PROCESSOR_PIPELINE_MINOR,
                amaterasu_media::FRAME_RESOURCE_PROCESSOR_API_V1_0_SIZE,
            ),
            (
                CAPTURE_CAPS,
                amaterasu_module_sdk::media::metadata::CAPABILITIES_CONTRACT,
                2,
                size_of::<amaterasu_media::FrameMetadataCapabilitiesApiV1_2>() as u32,
            ),
            (
                PROCESSOR_CAPS,
                amaterasu_module_sdk::media::metadata::CAPABILITIES_CONTRACT,
                2,
                size_of::<amaterasu_media::FrameMetadataCapabilitiesApiV1_2>() as u32,
            ),
            (
                ENCODER_CAPS,
                amaterasu_module_sdk::media::metadata::CAPABILITIES_CONTRACT,
                2,
                size_of::<amaterasu_media::FrameMetadataCapabilitiesApiV1_2>() as u32,
            ),
        ] {
            let bound = unsafe { runtime.bind_native_typed(service, contract, 1, minor, size) };
            assert!(bound.is_ok(), "native binding failed for {service}");
        }
    }
    let video = amaterasu_video::test_support::VideoModule::start(endpoint("video")).unwrap();
    let session = StreamSession::start(endpoint("consumer")).unwrap();
    let rtsp =
        amaterasu_rtsp::test_support::ManagedRtsp::start(endpoint("amaterasu.rtsp")).unwrap();
    let mut broker = Broker::new();
    if metadata {
        broker.extension_metadata_schema = Some(json!({"namespace_id":1,"value":1,"schema":1}));
    }
    Fixture {
        launcher: endpoint("launcher"),
        nvhttp: endpoint("amaterasu.nvhttp"),
        rtsp_owner: endpoint("amaterasu.rtsp"),
        provider,
        rtsp,
        session,
        video,
        transport,
        broker,
        _dynamic_provider: dynamic_provider,
        consumer: endpoint("consumer"),
        marker,
        _control: control,
        selected_encoder: "source".into(),
        opened_encoders: 0,
        encoder_live: false,
        closed_encoders: 0,
        metadata,
        probes: Vec::new(),
        _guard: RuntimeGuard { api, runtime },
    }
}

fn announcement() -> String {
    FIXTURE
        .replace(
            "x-ss-general.encryptionEnabled:0",
            "x-ss-general.encryptionEnabled:1",
        )
        .replace(
            "x-nv-video[0].packetSize:1392",
            "x-nv-video[0].packetSize:1024",
        )
        .replace(
            "x-nv-vqos[0].fec.minRequiredFecPackets:2",
            "x-nv-vqos[0].fec.minRequiredFecPackets:0",
        )
        .replace(
            "x-nv-vqos[0].bitStreamFormat:0",
            "x-nv-vqos[0].bitStreamFormat:9",
        )
        .replace(
            "x-nv-video[0].maxNumReferenceFrames:1",
            "x-nv-video[0].maxNumReferenceFrames:0",
        )
}
fn profile() -> VideoProfile {
    serde_json::from_value(json!({"codec":{"namespace_id":42,"value":9,"schema":1},
        "framing":{"namespace_id":42,"value":10,"schema":1},"wire_selector":9,"payload_type":99,"encoding_name":"TEST9"})).unwrap()
}
fn prepare(fixture: &mut Fixture, video_port: u16, audio_port: u16, hdr: bool) -> Value {
    let parsed = amaterasu_gamestream::sdp::parse_profile_announce(
        announcement().as_bytes(),
        &VideoProfileSnapshot {
            profiles: vec![profile()],
        },
    )
    .unwrap();
    fixture.request(RTSP, "launch", json!({"action":"open","operation_id":"launch",
        "peer_ip":"127.0.0.1","video_port":video_port,"audio_port":audio_port,
        "key_hex":"00112233445566778899aabbccddeeff","key_id":1,"connect_data":2,"stream_audio":false,
        "capture":{"capture_provider":if fixture.metadata {"fixture_capture"} else {"dxgi"},"output":"display","encoder":0,"audio_endpoint":"default"},
        "profile":{"hdr":hdr,"width":parsed.video.width,"height":parsed.video.height,"fps":parsed.video.fps,
            "bitrate_kbps":parsed.video.bitrate_kbps,"max_reference_frames":0,"packet_size":1024,
            "fec_percentage":0,"min_parity_shards":0,"max_fec_blocks":1,"packet_ms":parsed.audio.packet_duration_ms,
            "encrypt_video":false,"encrypt_audio":false}}))
}
fn exchange(
    fixture: &mut Fixture,
    offer_expected: bool,
    do_play: bool,
    wrong_selector: bool,
    hdr: bool,
) {
    let video_peer = UdpSocket::bind("127.0.0.1:0").unwrap();
    let audio_peer = UdpSocket::bind("127.0.0.1:0").unwrap();
    video_peer.set_nonblocking(true).unwrap();
    // The RTSP module's real startup reconciliation asks the real session
    // pipeline to finish loading its synthetic state provider first.
    for _ in 0..100 {
        fixture.tick();
        std::thread::sleep(Duration::from_millis(1));
    }
    let prepared = prepare(
        fixture,
        video_peer.local_addr().unwrap().port(),
        audio_peer.local_addr().unwrap().port(),
        hdr,
    );
    assert_eq!(prepared["state"], "ready", "{prepared}");
    let url = prepared["rtsp_url"].as_str().unwrap().to_owned();
    let address = url
        .strip_prefix("rtspenc://")
        .unwrap()
        .split('/')
        .next()
        .unwrap()
        .to_owned();
    let key = [
        0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee,
        0xff,
    ];
    let mut sealer = RtspSealer::new(&key, Direction::ClientToHost).unwrap();
    let mut opener = RtspOpener::new(&key, Direction::HostToClient).unwrap();
    let send =
        |fixture: &mut Fixture, sealer: &mut RtspSealer, opener: &mut RtspOpener, text: String| {
            let reply = fixture.transaction(&address, sealer, opener, text);
            assert!(reply.starts_with("RTSP/1.0 200"), "{reply}");
            reply
        };
    send(
        fixture,
        &mut sealer,
        &mut opener,
        format!("OPTIONS {url} RTSP/1.0\r\nCSeq: 1\r\n\r\n"),
    );
    let describe = send(
        fixture,
        &mut sealer,
        &mut opener,
        format!("DESCRIBE {url} RTSP/1.0\r\nCSeq: 2\r\nAccept: application/sdp\r\n\r\n"),
    );
    assert_eq!(
        describe.contains("a=rtpmap:99 TEST9/90000"),
        offer_expected,
        "{describe}"
    );
    if !do_play && !hdr {
        let closed = fixture.request(
            RTSP,
            "close-mismatch",
            json!({"action":"close","ticket":prepared["ticket"]}),
        );
        assert_eq!(closed["state"], "closed", "{closed}");
        return;
    }
    let mut token = String::new();
    for (cseq, target) in [
        (3, "streamid=audio/0/0"),
        (4, "streamid=video/0/0"),
        (5, "streamid=control/13/0"),
    ] {
        let session = if token.is_empty() {
            String::new()
        } else {
            format!("Session: {token}\r\n")
        };
        let reply = send(
            fixture,
            &mut sealer,
            &mut opener,
            format!(
                "SETUP {target} RTSP/1.0\r\nCSeq: {cseq}\r\n{session}Transport: unicast;X-GS-ClientPort=50000-50001\r\n\r\n"
            ),
        );
        if token.is_empty() {
            token = reply
                .lines()
                .find_map(|line| line.strip_prefix("Session: "))
                .unwrap()
                .trim()
                .to_owned();
        }
    }
    let mut body = if hdr {
        announcement().replace(
            "x-nv-video[0].dynamicRangeMode:0",
            "x-nv-video[0].dynamicRangeMode:1",
        )
    } else if wrong_selector {
        announcement().replace(
            "x-nv-vqos[0].bitStreamFormat:9",
            "x-nv-vqos[0].bitStreamFormat:10",
        )
    } else {
        announcement()
    };
    if fixture.metadata {
        body.push_str("a=x-amaterasu-video-metadata:9 1 1 1\r\n");
    }
    let announce_request = format!(
        "ANNOUNCE streamid=control/13/0 RTSP/1.0\r\nCSeq: 6\r\nSession: {token}\r\nContent-Type: application/sdp\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    if wrong_selector || hdr {
        let rejected = fixture.transaction(&address, &mut sealer, &mut opener, announce_request);
        assert!(rejected.starts_with("RTSP/1.0 406"), "{rejected}");
        assert_eq!(fixture.opened_encoders, 0);
        let _ = fixture.request(
            RTSP,
            "close-unnegotiated",
            json!({"action":"close","ticket":prepared["ticket"]}),
        );
        return;
    }
    send(fixture, &mut sealer, &mut opener, announce_request);
    send(
        fixture,
        &mut sealer,
        &mut opener,
        format!("PLAY / RTSP/1.0\r\nCSeq: 7\r\nSession: {token}\r\n\r\n"),
    );
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        fixture.tick();
        let mut bytes = [0u8; 2048];
        match video_peer.recv(&mut bytes) {
            Ok(n) => {
                assert!(n >= if fixture.metadata { 46 } else { 44 });
                assert_eq!(
                    &bytes[40..44],
                    &[
                        10,
                        20,
                        if fixture.marker == 1 { 31 } else { 32 },
                        fixture.marker
                    ]
                );
                if fixture.metadata {
                    assert_eq!(&bytes[44..46], &[0xb5, 1]);
                }
                break;
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) => panic!("{e}"),
        }
        if Instant::now() >= deadline {
            panic!(
                "no video after PLAY; video status: {}",
                fixture.video_status()
            );
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(fixture.opened_encoders, 1);
    assert_eq!(fixture.probes[0]["encoder"]["clsid"], "source");
    assert_eq!(
        fixture.probes[0]["configuration"]["extension_profile"],
        json!(profile())
    );
    let status = fixture.video_status();
    let selected = &status["streams"][0]["negotiated_unit"];
    let stream = selected["stream"].as_u64().unwrap();
    assert_eq!(selected["generation"], stream);
    assert_eq!(selected["offer_id"], 9);
    assert_eq!(selected["codec"], json!(profile().codec));
    assert_eq!(selected["framing"], json!(profile().framing));
    let binding = unsafe { video_framing::bind_video(&fixture.consumer, VIDEO) }.unwrap();
    send(
        fixture,
        &mut sealer,
        &mut opener,
        format!("TEARDOWN / RTSP/1.0\r\nCSeq: 8\r\nSession: {token}\r\n\r\n"),
    );
    assert!(!fixture.encoder_live);
    assert_eq!(fixture.closed_encoders, 1);
    unsafe extern "C" fn reject(_: *mut c_void, _: *const NegotiatedVideoUnit) -> Status {
        STATUS_INVALID_ARGUMENT
    }
    let api = unsafe { binding.table::<NegotiatedVideoApi>() }.unwrap();
    assert_eq!(
        unsafe {
            (api.receive)(
                binding.context(),
                stream,
                NegotiatedVideoUnitSink {
                    context: std::ptr::null_mut(),
                    write: reject,
                },
            )
        },
        MEDIA_CLOSED
    );
}

#[test]
#[ignore = "requires two separately built submitted metadata providers and loopback"]
fn managed_rtsp_negotiates_submitted_metadata_route() {
    for marker in [1, 2] {
        let mut host = fixture_with_metadata(marker, "source", "TEST9", true);
        exchange(&mut host, true, true, false, false);
    }
}

#[test]
#[ignore = "requires two separately built native fixture provider artifacts"]
fn managed_rtsp_negotiates_the_real_session_route() {
    for marker in [1, 2] {
        let mut host = fixture(marker, "source", "TEST9");
        exchange(&mut host, true, true, false, false);
    }
    {
        let mut host = fixture(1, "source", "TEST9");
        exchange(&mut host, true, true, true, false);
    }
    for (encoder, wire_name) in [
        ("sibling", "TEST9"),
        ("source", "MISMATCH"),
        ("source", "PAYLOAD"),
    ] {
        let mut host = fixture(1, encoder, wire_name);
        exchange(&mut host, false, false, false, false);
        assert_eq!(host.opened_encoders, 0);
        assert_eq!(host.probes[0]["encoder"]["clsid"], encoder);
    }
}

#[test]
#[ignore = "requires two separately built native fixture provider artifacts"]
fn managed_rtsp_withholds_unsupported_hdr_metadata_route() {
    for marker in [1, 2] {
        let mut host = fixture(marker, "source", "TEST9");
        exchange(&mut host, false, false, false, true);
        // The session never probes or opens the source, because this profile
        // cannot negotiate or carry frame-associated HDR metadata yet.
        assert!(host.probes.is_empty());
        assert_eq!(host.opened_encoders, 0);
    }
}

#[test]
#[ignore = "requires the separately built native fixture provider and ephemeral loopback ports"]
fn managed_rtsp_and_stream_preserve_actual_display_preparation_timeout() {
    let mut host = fixture(1, "source", "TEST9");
    for _ in 0..100 {
        host.tick();
        std::thread::sleep(Duration::from_millis(1));
    }
    let video = UdpSocket::bind("127.0.0.1:0").unwrap();
    let audio = UdpSocket::bind("127.0.0.1:0").unwrap();
    let display = json!({"expected_topology_revision":"0123456789abcdef0123456789abcdef",
        "configuration":{"displays":[
            {"id":"desktop-primary","width":1920,"height":1080,"refresh":{"numerator":60,"denominator":1},"x":0,"y":0,"rotation":"identity","hdr_enabled":false},
            {"id":"desktop-secondary","width":1280,"height":720,"refresh":{"numerator":60,"denominator":1},"x":1920,"y":0,"rotation":"identity","hdr_enabled":false}]},
        "mode_policy":{"display_id":"desktop-primary","configuration_option":"ensure_active","remapping":{},"context":{
            "resolution_is_automatic":true,"refresh_rate_is_automatic":true,"client_display_mode_override":false,"optimize_game_settings":false}}});
    let reply = host.nvhttp_request(RTSP,"display-launch",json!({"action":"open","operation_id":"display-launch",
        "application_operation_id":"launch-one","display_admission_token":"ab".repeat(32),
        "display_policy_snapshot":{"global_revision":1,"global_overrides":{},"catalog_revision":0,"certificate_id":"cd".repeat(32),"application_overrides":{},"client_overrides":{},"effective_overrides":{}},
        "display_request":display,"peer_ip":"127.0.0.1","video_port":video.local_addr().unwrap().port(),"audio_port":audio.local_addr().unwrap().port(),
        "key_hex":"00112233445566778899aabbccddeeff","key_id":1,"connect_data":2,"stream_audio":false,
        "capture":{"capture_provider":"dxgi","output":"display","encoder":0,"audio_endpoint":"default"}}));
    assert_eq!(reply["ok"], true, "{reply}");
    assert_eq!(reply["display_applied"], true, "{reply}");
    assert_eq!(
        host.broker.display_lease.as_deref(),
        Some("display-0000000000000001")
    );
    let request = match host
        .rtsp_owner
        .send(Command::Request {
            service: STREAM.into(),
            payload: json!({"action":"status","session":reply["session"]}),
            operation_id: "display-stream-status".into(),
            timeout_ms: 5000,
            parent: None,
        })
        .unwrap()
    {
        Response::Accepted { id } => id,
        other => panic!("stream status rejected: {other:?}"),
    };
    let mut status = None;
    for _ in 0..1000 {
        host.tick();
        if let Response::Result { payload } = host
            .rtsp_owner
            .send(Command::TakeResult { request })
            .unwrap()
        {
            status = Some(payload);
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    let status = status.expect("real stream status");
    assert_eq!(status["display_applied"], true, "{status}");
    assert!(
        status["display_prepare_timeout_ms"]
            .as_u64()
            .is_some_and(|value| (1..=40_000).contains(&value)),
        "{status}"
    );
    host.rtsp_owner
        .require_success(Command::ReleaseResult { request })
        .unwrap();
}
