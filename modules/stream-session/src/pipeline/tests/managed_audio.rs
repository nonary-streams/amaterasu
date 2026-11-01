//! Broker-level integration using synthetic marker bytes, not Dolby bitstreams.
use super::*;
use crate::StreamSession;
use amaterasu_gamestream::crypto::{Direction, RtspOpener, RtspSealer};
use amaterasu_media::*;
use amaterasu_module_sdk::{ManagedModule, abi::*};
use std::{
    ffi::c_void,
    io::{Read, Write},
    mem::{MaybeUninit, size_of},
    net::{TcpStream, UdpSocket},
    time::{Duration, Instant},
};

unsafe extern "C" {
    fn amaterasu_runtime_v1() -> *const RuntimeApi;
}
const ENCODER: &str = "example.ordered-encoder/1";
const CAPTURE: &str = "example.ordered-capture/1";
const TRANSPORT: &str = "example.ordered-transport/1";
const MEDIA: &str = "amaterasu.media-transport/1";
const RTSP: &str = "amaterasu.rtsp/1";
const STREAM: &str = "amaterasu.stream-session/1";
const VIDEO: &str = "amaterasu.video/1";
const AUDIO: &str = "amaterasu.audio/1";
const OPUS: &str = "amaterasu.audio-encode/1";
const CONTROL: &str = "amaterasu.control-transport/1";
const SESSION: &str = "amaterasu.session/1";
const STATE: &str = "amaterasu.state/1";

// The test owns every table until the managed modules and runtime have retired.
struct Factory {
    api: NativeServiceFactory,
    table: *const NativeInterface,
}
impl Factory {
    fn new(table: *const NativeInterface) -> Box<Self> {
        let mut result = Box::new(Self {
            api: NativeServiceFactory {
                header: AbiHeader::of::<NativeServiceFactory>(),
                context: std::ptr::null_mut(),
                bind: Some(bind),
            },
            table,
        });
        result.api.context = (&mut *result as *mut Self).cast();
        result
    }
}
unsafe extern "C" fn bind(
    context: *mut c_void,
    principal: BorrowedBytes,
    query: *const NativeInterfaceQuery,
    out: *mut *const NativeInterface,
) -> Status {
    if context.is_null()
        || query.is_null()
        || out.is_null()
        || unsafe { principal.read() }.ok() != Some(b"consumer")
    {
        return STATUS_INVALID_ARGUMENT;
    }
    let table = unsafe { (*context.cast::<Factory>()).table };
    let header = unsafe { &(*table).header };
    let query = unsafe { &*query };
    if query.interface_major != header.major
        || query.min_minor > header.minor
        || query.min_size > header.size
    {
        return STATUS_INVALID_ARGUMENT;
    }
    unsafe {
        *out = table;
    }
    STATUS_OK
}
unsafe extern "C" fn release(_: *mut c_void) {}
fn prefix<T>(context: *mut c_void, major: u32) -> NativeInterface {
    NativeInterface {
        header: AbiHeader {
            size: size_of::<T>() as u32,
            major,
            minor: 0,
        },
        context,
        release: Some(release),
    }
}
unsafe extern "C" fn no_video(context: *mut c_void, _: u64, sink: PacketSink) -> Status {
    let state = unsafe { &mut *context.cast::<Capture>() };
    if !state.video_draining || state.video_ended {
        return MEDIA_PENDING;
    }
    let packet = EncodedPacket {
        codec: CODEC_H264,
        flags: FLAG_END_OF_STREAM,
        data: std::ptr::null(),
        len: 0,
        timestamp_us: 0,
        sequence: 0,
    };
    let status = unsafe { (sink.write)(sink.context, &packet) };
    if status == STATUS_OK {
        state.video_ended = true;
    }
    status
}
unsafe extern "C" fn no_capture(_: *mut c_void, _: u64, _: AudioSink) -> Status {
    MEDIA_PENDING
}
unsafe extern "C" fn no_pcm(_: *mut c_void, _: u64, _: *const AudioFrame) -> Status {
    STATUS_INVALID_ARGUMENT
}
unsafe extern "C" fn no_audio(context: *mut c_void, _: u64, sink: AudioPacketSink) -> Status {
    let state = unsafe { &mut *context.cast::<Capture>() };
    if !state.opus_draining || state.opus_ended {
        return MEDIA_PENDING;
    }
    let packet = EncodedAudioPacket {
        codec: CODEC_OPUS,
        flags: AUDIO_PACKET_END_OF_STREAM,
        sample_rate: 48_000,
        channels: 2,
        frames: 0,
        data: std::ptr::null(),
        len: 0,
        timestamp_us: 0,
        sequence: 0,
    };
    let status = unsafe { (sink.write)(sink.context, &packet) };
    if status == STATUS_OK {
        state.opus_ended = true;
    }
    status
}
unsafe extern "C" fn done(context: *mut c_void, _: u64) -> Status {
    unsafe {
        (*context.cast::<Capture>()).opus_draining = true;
    }
    STATUS_OK
}
unsafe extern "C" fn no_control(_: *mut c_void, _: u64, _: ControlMessageSink) -> Status {
    MEDIA_PENDING
}
unsafe extern "C" fn send_control(_: *mut c_void, _: u64, _: *const ControlMessage) -> Status {
    STATUS_OK
}
struct Capture {
    samples: Vec<u8>,
    sent: u32,
    busy: u32,
    reordered: bool,
    gap: bool,
    video_draining: bool,
    video_ended: bool,
    opus_draining: bool,
    opus_ended: bool,
}
unsafe extern "C" fn capture(
    context: *mut c_void,
    stream: u64,
    sink: OrderedAudioSinkV2,
) -> Status {
    let state = unsafe { &mut *context.cast::<Capture>() };
    if stream != 20 {
        return STATUS_INVALID_ARGUMENT;
    }
    if state.busy == 0 {
        state.busy += 1;
        return MEDIA_BUSY;
    }
    if state.sent >= 4 {
        return MEDIA_PENDING;
    }
    let mut lanes = [1, 2, 0, 0, 0, 0, 0, 0];
    if state.reordered {
        lanes.swap(0, 1);
    }
    let frame = OrderedAudioFrameV2 {
        size: size_of::<OrderedAudioFrameV2>() as u32,
        version: AUDIO_ORDERED_VERSION,
        stream,
        generation: 1,
        frame: AudioFrame {
            sample_rate: 48_000,
            channels: 2,
            format: PCM_F32,
            frames: 1536,
            flags: AUDIO_TIMESTAMP_ESTIMATED,
            data: state.samples.as_ptr(),
            len: state.samples.len(),
            timestamp_us: (100_000i64
                + i64::from(state.sent) * 32_000
                + if state.gap && state.sent == 2 {
                    0
                } else {
                    [0, 1, -1, 2][state.sent as usize]
                }
                + if state.gap && state.sent >= 2 {
                    16_000
                } else {
                    0
                }) as u64,
            sequence: u64::from(state.sent),
        },
        layout: AudioChannelLayoutV2 {
            size: size_of::<AudioChannelLayoutV2>() as u32,
            version: AUDIO_ORDERED_VERSION,
            channels: 2,
            reserved: 0,
            mask: 3,
            lanes,
        },
    };
    let status = unsafe { (sink.write)(sink.context, &frame) };
    if status == STATUS_OK {
        state.sent += 1;
    }
    status
}
struct Provider {
    library: libloading::Library,
    table: *mut OrderedAudioEncoderApiV2,
    destroy: unsafe extern "C" fn(*mut OrderedAudioEncoderApiV2),
}
impl Provider {
    fn load(path: &str) -> Self {
        // SAFETY: these are task-built test artifacts with the documented fixture ABI.
        let library = unsafe { libloading::Library::new(path).unwrap() };
        let create: unsafe extern "C" fn(u32) -> *mut OrderedAudioEncoderApiV2 =
            unsafe { *library.get(b"fixture_create_v2_channels\0").unwrap() };
        let destroy = unsafe { *library.get(b"fixture_destroy_v2\0").unwrap() };
        let table = unsafe { create(2) };
        assert!(!table.is_null());
        unsafe {
            (*table).base.release = Some(release);
        }
        Self {
            library,
            table,
            destroy,
        }
    }
    fn busy_once(&self) {
        let set: unsafe extern "C" fn(*mut OrderedAudioEncoderApiV2) =
            unsafe { *self.library.get(b"fixture_busy_once_v2\0").unwrap() };
        unsafe { set(self.table) };
    }
    fn busy_count(&self) -> u32 {
        let get: unsafe extern "C" fn(*mut OrderedAudioEncoderApiV2) -> u32 =
            unsafe { *self.library.get(b"fixture_busy_count_v2\0").unwrap() };
        unsafe { get(self.table) }
    }
    fn offer(&self, generation: u64, fixed_bytes: u32) {
        let set: unsafe extern "C" fn(*mut OrderedAudioEncoderApiV2, u64, u32) =
            unsafe { *self.library.get(b"fixture_set_offer_v2\0").unwrap() };
        // SAFETY: called before preparation, with no native binding retained.
        unsafe { set(self.table, generation, fixed_bytes) };
    }
}
impl Drop for Provider {
    fn drop(&mut self) {
        unsafe { (self.destroy)(self.table) };
        let _ = &self.library;
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
struct Host {
    rtsp: amaterasu_rtsp::test_support::ManagedRtsp,
    session: StreamSession,
    transport: amaterasu_media_transport::test_support::ManagedTransport,
    launcher: RuntimeClient,
    consumer: RuntimeClient,
    provider: RuntimeClient,
    broker: Broker,
    capture: Box<Capture>,
    // Factory addresses are registered with the runtime and must remain stable.
    #[allow(clippy::vec_box)]
    _factories: Vec<Box<Factory>>,
    _video: Box<VideoApi>,
    _audio: Box<AudioCaptureApi>,
    _opus: Box<AudioEncoderApi>,
    _control: Box<ControlTransportApi>,
    _ordered: Box<OrderedAudioCaptureApiV2>,
    encoder: Provider,
    _runtime: RuntimeGuard,
}
impl Host {
    fn tick(&mut self) {
        self.rtsp.tick().unwrap();
        self.session.tick().unwrap();
        self.transport.tick().unwrap();
        for _ in 0..32 {
            let Response::Request {
                id,
                service,
                payload,
                ..
            } = self.provider.send(Command::Poll {}).unwrap()
            else {
                break;
            };
            if service == VIDEO && payload["action"] == "drain" {
                self.capture.video_draining = true;
            }
            let call = Call {
                service,
                payload,
                parent: None,
            };
            self.broker.calls.push(call.clone());
            let result = self.broker.execute(&call);
            self.provider
                .require_success(Command::Complete {
                    request: id,
                    payload: result,
                })
                .unwrap();
        }
    }
    fn request(&mut self, op: &str, payload: Value) -> Value {
        let Response::Accepted { id } = self
            .launcher
            .send(Command::Request {
                service: RTSP.into(),
                payload,
                operation_id: op.into(),
                timeout_ms: 8000,
                parent: None,
            })
            .unwrap()
        else {
            panic!("request rejected");
        };
        let deadline = Instant::now() + Duration::from_secs(7);
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
                Response::Pending => (),
                result => panic!("unexpected {result:?}"),
            }
            assert!(Instant::now() < deadline, "request {op} timed out");
            std::thread::sleep(Duration::from_millis(1));
        }
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
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut bytes = Vec::new();
        loop {
            self.tick();
            let mut buffer = [0; 8192];
            match client.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => bytes.extend_from_slice(&buffer[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => (),
                Err(e) => panic!("{e}"),
            }
            assert!(Instant::now() < deadline, "RTSP timeout");
            std::thread::sleep(Duration::from_millis(1));
        }
        String::from_utf8(opener.open(&bytes).unwrap().plaintext).unwrap()
    }
}
fn fixture(path: &str, codec: &str) -> Host {
    let services = [
        RTSP, STREAM, SESSION, STATE, MEDIA, CONTROL, VIDEO, AUDIO, OPUS, ENCODER,
    ];
    let native: serde_json::Map<String, Value> = [
        (VIDEO, amaterasu_module_sdk::video::CONTRACT, "provider"),
        (
            AUDIO,
            amaterasu_module_sdk::audio::CAPTURE_CONTRACT,
            "provider",
        ),
        (
            OPUS,
            amaterasu_module_sdk::audio::ENCODER_CONTRACT,
            "provider",
        ),
        (
            CONTROL,
            amaterasu_module_sdk::transport::CONTROL_CONTRACT,
            "provider",
        ),
        (
            ENCODER,
            amaterasu_module_sdk::audio::ORDERED_ENCODER_CONTRACT,
            "provider",
        ),
        (
            CAPTURE,
            amaterasu_module_sdk::audio::ORDERED_CAPTURE_CONTRACT,
            "provider",
        ),
        (
            MEDIA,
            amaterasu_module_sdk::transport::MEDIA_CONTRACT,
            "transport",
        ),
        (
            TRANSPORT,
            amaterasu_module_sdk::audio::ORDERED_TRANSPORT_CONTRACT,
            "transport",
        ),
    ]
    .iter()
    .map(|(name, contract, owner)| {
        (
            (*name).into(),
            json!({"provider":owner,"contract":contract}),
        )
    })
    .collect();
    let config = json!({"principals":{
        "provider":{"provide":[SESSION,STATE,CONTROL,VIDEO,AUDIO,OPUS,ENCODER],"native_provide":[VIDEO,AUDIO,OPUS,CONTROL,ENCODER,CAPTURE]},
        "transport":{"provide":[MEDIA],"native_provide":[MEDIA,TRANSPORT]},
        "consumer":{"provide":[STREAM],"call":services,"native_use":[VIDEO,AUDIO,OPUS,CONTROL,ENCODER,CAPTURE,MEDIA,TRANSPORT]},
        "amaterasu.rtsp":{"provide":[RTSP],"call":services},"launcher":{"call":services}},
        "settings":{"transport":{"enabled":true,"allowed_principals":["consumer"],"ordered_audio_service":TRANSPORT},
        "consumer":{"enabled":true,"allowed_principals":["amaterasu.rtsp"],"audio_extension_route":{"codec":codec,"encoder_control_service":ENCODER,"encoder_service":ENCODER,"capture_service":CAPTURE,"transport_service":TRANSPORT}},
        "amaterasu.rtsp":{"enabled":true,"bind":"127.0.0.1:0","allowed_principals":["launcher"]}},"service_selections":{"native":native}});
    let api = unsafe { &*amaterasu_runtime_v1() };
    let mut runtime = std::ptr::null_mut();
    assert_eq!(
        unsafe {
            (api.create)(
                BorrowedBytes::new(&serde_json::to_vec(&config).unwrap()),
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
    for service in [SESSION, STATE, CONTROL, VIDEO, AUDIO, OPUS, ENCODER] {
        provider
            .require_success(Command::Register {
                service: service.into(),
            })
            .unwrap();
    }
    let mut capture_state = Box::new(Capture {
        samples: vec![0; 1536 * 2 * 4],
        sent: 0,
        busy: 0,
        reordered: false,
        gap: false,
        video_draining: false,
        video_ended: false,
        opus_draining: false,
        opus_ended: false,
    });
    let mut video = Box::new(VideoApi {
        base: prefix::<VideoApi>((&mut *capture_state as *mut Capture).cast(), 1),
        receive: no_video,
    });
    let mut audio = Box::new(AudioCaptureApi {
        base: prefix::<AudioCaptureApi>(std::ptr::null_mut(), 1),
        read_audio: no_capture,
    });
    let mut opus = Box::new(AudioEncoderApi {
        base: prefix::<AudioEncoderApi>((&mut *capture_state as *mut Capture).cast(), 1),
        submit: no_pcm,
        receive: no_audio,
        drain: done,
    });
    let mut control = Box::new(ControlTransportApi {
        base: prefix::<ControlTransportApi>(std::ptr::null_mut(), 1),
        send: send_control,
        receive: no_control,
    });
    let mut ordered = Box::new(OrderedAudioCaptureApiV2 {
        base: prefix::<OrderedAudioCaptureApiV2>((&mut *capture_state as *mut Capture).cast(), 2),
        read_audio: capture,
    });
    let encoder = Provider::load(path);
    let entries = [
        (
            VIDEO,
            amaterasu_module_sdk::video::CONTRACT,
            &mut video.base as *mut NativeInterface,
        ),
        (
            AUDIO,
            amaterasu_module_sdk::audio::CAPTURE_CONTRACT,
            &mut audio.base,
        ),
        (
            OPUS,
            amaterasu_module_sdk::audio::ENCODER_CONTRACT,
            &mut opus.base,
        ),
        (
            CONTROL,
            amaterasu_module_sdk::transport::CONTROL_CONTRACT,
            &mut control.base,
        ),
        (
            ENCODER,
            amaterasu_module_sdk::audio::ORDERED_ENCODER_CONTRACT,
            unsafe { &mut (*encoder.table).base },
        ),
        (
            CAPTURE,
            amaterasu_module_sdk::audio::ORDERED_CAPTURE_CONTRACT,
            &mut ordered.base,
        ),
    ];
    let mut factories = Vec::new();
    for (service, contract, table) in entries {
        let factory = Factory::new(table);
        unsafe {
            provider
                .register_native_typed(service, contract, &factory.api)
                .unwrap();
        }
        factories.push(factory);
    }
    let transport =
        amaterasu_media_transport::test_support::ManagedTransport::start(endpoint("transport"))
            .unwrap();
    let session = StreamSession::start(endpoint("consumer")).unwrap();
    let rtsp =
        amaterasu_rtsp::test_support::ManagedRtsp::start(endpoint("amaterasu.rtsp")).unwrap();
    let mut broker = Broker::new();
    for (service, id) in [
        (ENCODER, 19),
        (AUDIO, 20),
        (OPUS, 30),
        (VIDEO, 10),
        (CONTROL, 50),
    ] {
        broker.forced_stream_ids.insert(service.into(), id);
    }
    Host {
        rtsp,
        session,
        transport,
        launcher: endpoint("launcher"),
        consumer: endpoint("consumer"),
        provider,
        broker,
        capture: capture_state,
        _factories: factories,
        _video: video,
        _audio: audio,
        _opus: opus,
        _control: control,
        _ordered: ordered,
        encoder,
        _runtime: RuntimeGuard { api, runtime },
    }
}
fn exchange(
    host: &mut Host,
    codec: &str,
    marker: &[u8],
    accept: bool,
    launch: &str,
    stale: Option<u64>,
) {
    let video = UdpSocket::bind("127.0.0.1:0").unwrap();
    let audio = UdpSocket::bind("127.0.0.1:0").unwrap();
    audio.set_nonblocking(true).unwrap();
    for _ in 0..100 {
        host.tick();
        std::thread::sleep(Duration::from_millis(1));
    }
    let body = include_str!("../../../../../crates/gamestream/src/sdp/fixtures/moonlight-h264.sdp")
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
        );
    let parsed = amaterasu_gamestream::sdp::parse_announce(body.as_bytes()).unwrap();
    let key_hex = if launch == "second" {
        "10112233445566778899aabbccddeeff"
    } else {
        "00112233445566778899aabbccddeeff"
    };
    let prepared = host.request(launch, json!({"action":"open","operation_id":launch,"peer_ip":"127.0.0.1","video_port":video.local_addr().unwrap().port(),"audio_port":audio.local_addr().unwrap().port(),"key_hex":key_hex,"key_id":1,"connect_data":2,"stream_audio":true,
        "capture":{"capture_provider":"dxgi","output":"display","encoder":0,"audio_endpoint":"default"},
        "profile":{"width":parsed.video.width,"height":parsed.video.height,"fps":parsed.video.fps,"bitrate_kbps":parsed.video.bitrate_kbps,"max_reference_frames":parsed.video.max_reference_frames,"packet_size":1024,"fec_percentage":0,"min_parity_shards":0,"max_fec_blocks":1,"packet_ms":parsed.audio.packet_duration_ms,"encrypt_video":false,"encrypt_audio":false}}));
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
        if launch == "second" { 0x10 } else { 0x00 },
        0x11,
        0x22,
        0x33,
        0x44,
        0x55,
        0x66,
        0x77,
        0x88,
        0x99,
        0xaa,
        0xbb,
        0xcc,
        0xdd,
        0xee,
        0xff,
    ];
    let mut sealer = RtspSealer::new(&key, Direction::ClientToHost).unwrap();
    let mut opener = RtspOpener::new(&key, Direction::HostToClient).unwrap();
    let send = |host: &mut Host, sealer: &mut RtspSealer, opener: &mut RtspOpener, request| {
        let response = host.transaction(&address, sealer, opener, request);
        assert!(response.starts_with("RTSP/1.0 200"), "{response}");
        response
    };
    send(
        host,
        &mut sealer,
        &mut opener,
        format!("OPTIONS {url} RTSP/1.0\r\nCSeq: 1\r\n\r\n"),
    );
    let description = send(
        host,
        &mut sealer,
        &mut opener,
        format!("DESCRIBE {url} RTSP/1.0\r\nCSeq: 2\r\nAccept: application/sdp\r\n\r\n"),
    );
    assert!(
        description.contains(&format!("a=x-ss-audio-codec-offer:{codec}")),
        "{description}"
    );
    let mut token = String::new();
    for (seq, target) in [
        (3, "streamid=audio/0/0"),
        (4, "streamid=video/0/0"),
        (5, "streamid=control/13/0"),
    ] {
        let session = if token.is_empty() {
            String::new()
        } else {
            format!("Session: {token}\r\n")
        };
        let response = send(
            host,
            &mut sealer,
            &mut opener,
            format!(
                "SETUP {target} RTSP/1.0\r\nCSeq: {seq}\r\n{session}Transport: unicast;X-GS-ClientPort=50000-50001\r\n\r\n"
            ),
        );
        if token.is_empty() {
            token = response
                .lines()
                .find_map(|line| line.strip_prefix("Session: "))
                .unwrap()
                .trim()
                .into();
        }
    }
    let body = if accept {
        format!("{body}a=x-ss-audio-codec-accept:{codec}\r\n")
    } else {
        body
    };
    let response = send(
        host,
        &mut sealer,
        &mut opener,
        format!(
            "ANNOUNCE streamid=control/13/0 RTSP/1.0\r\nCSeq: 6\r\nSession: {token}\r\nContent-Type: application/sdp\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        ),
    );
    assert_eq!(
        response.contains(&format!("X-SS-Audio-Codec: {codec}")),
        accept
    );
    send(
        host,
        &mut sealer,
        &mut opener,
        format!("PLAY / RTSP/1.0\r\nCSeq: 7\r\nSession: {token}\r\n\r\n"),
    );
    if accept {
        let deadline = Instant::now() + Duration::from_secs(4);
        let expected_times = if host.capture.gap {
            [0u32, 32, 80, 112]
        } else {
            [0u32, 32, 64, 96]
        };
        let mut received = 0;
        while received < expected_times.len() {
            host.tick();
            let mut packet = [0; 2048];
            match audio.recv(&mut packet) {
                Ok(n) => {
                    assert_eq!(&packet[12..n], marker);
                    assert_eq!(
                        u32::from_be_bytes(packet[4..8].try_into().unwrap()),
                        expected_times[received]
                    );
                    received += 1;
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => (),
                Err(error) => panic!("{error}"),
            }
            assert!(Instant::now() < deadline, "no marker packet after PLAY");
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(host.capture.busy, 1);
        assert_eq!(host.capture.sent, 4);
        assert_eq!(host.encoder.busy_count(), 1);
        if let Some(generation) = stale {
            let record = host
                .session
                .pipeline
                .records
                .values()
                .find(|record| record.state == State::Active)
                .unwrap();
            let stream = record.resources[&Resource::Media];
            // SAFETY: the runtime and typed binding remain live through the synchronous call.
            let binding = unsafe {
                amaterasu_module_sdk::audio::bind_ordered_transport(&host.consumer, TRANSPORT)
            }
            .unwrap();
            let api = unsafe { binding.table::<OrderedAudioTransportApiV2>() }.unwrap();
            let packet = NegotiatedAudioPacket {
                stream: 19,
                generation,
                offer_id: if codec == "AC3" { 11 } else { 22 },
                flags: 0,
                frames: 1536,
                timestamp_us: 100_000,
                sequence: 0,
                data: BorrowedBytes::new(marker),
            };
            assert_eq!(
                unsafe { (api.submit)(binding.context(), stream, &packet) },
                STATUS_INVALID_ARGUMENT
            );
        }
    } else {
        for _ in 0..30 {
            host.tick();
        }
        assert_eq!(host.capture.sent, 0);
    }
    let media_stream = host
        .session
        .pipeline
        .records
        .values()
        .find(|record| record.state == State::Active)
        .unwrap()
        .resources[&Resource::Media];
    send(
        host,
        &mut sealer,
        &mut opener,
        format!("TEARDOWN / RTSP/1.0\r\nCSeq: 8\r\nSession: {token}\r\n\r\n"),
    );
    assert!(
        host.broker
            .calls
            .iter()
            .any(|call| call.service == ENCODER && call.payload["action"] == "close")
    );
    let binding =
        unsafe { amaterasu_module_sdk::audio::bind_ordered_transport(&host.consumer, TRANSPORT) }
            .unwrap();
    let api = unsafe { binding.table::<OrderedAudioTransportApiV2>() }.unwrap();
    let packet = NegotiatedAudioPacket {
        stream: 19,
        generation: 1,
        offer_id: 1,
        flags: 0,
        frames: 1536,
        timestamp_us: 100_000,
        sequence: 0,
        data: BorrowedBytes::new(marker),
    };
    let retired = unsafe { (api.submit)(binding.context(), media_stream, &packet) };
    assert!(
        matches!(retired, MEDIA_CLOSED | STATUS_INVALID_ARGUMENT),
        "retired transport returned {retired}"
    );
}
fn rejected_offer(host: &mut Host, unsupported_layout: bool) {
    let video = UdpSocket::bind("127.0.0.1:0").unwrap();
    let audio = UdpSocket::bind("127.0.0.1:0").unwrap();
    for _ in 0..100 {
        host.tick();
        std::thread::sleep(Duration::from_millis(1));
    }
    let body = include_str!("../../../../../crates/gamestream/src/sdp/fixtures/moonlight-h264.sdp")
        .replace(
            "x-ss-general.encryptionEnabled:0",
            "x-ss-general.encryptionEnabled:1",
        );
    let parsed = amaterasu_gamestream::sdp::parse_announce(body.as_bytes()).unwrap();
    let mut request = json!({"action":"open","operation_id":"rejected","peer_ip":"127.0.0.1","video_port":video.local_addr().unwrap().port(),"audio_port":audio.local_addr().unwrap().port(),"key_hex":"00112233445566778899aabbccddeeff","key_id":1,"connect_data":2,"stream_audio":true,
        "capture":{"capture_provider":"dxgi","output":"display","encoder":0,"audio_endpoint":"default"},
        "profile":{"width":parsed.video.width,"height":parsed.video.height,"fps":parsed.video.fps,"bitrate_kbps":parsed.video.bitrate_kbps,"max_reference_frames":parsed.video.max_reference_frames,"packet_size":1024,"fec_percentage":0,"min_parity_shards":0,"max_fec_blocks":1,"packet_ms":parsed.audio.packet_duration_ms,"encrypt_video":false,"encrypt_audio":false}});
    if unsupported_layout {
        request["channels"] = json!(8);
        request["virtual_sink"] = json!("fixture");
        request["capture"]["audio_endpoint"] = json!("");
        request["profile"]["audio_bitrate"] = json!(512000);
    }
    let prepared = host.request("rejected", request);
    assert_eq!(prepared["state"], "ready", "{prepared}");
    let record = host
        .session
        .pipeline
        .records
        .values()
        .find(|record| record.state == State::Prepared)
        .unwrap();
    assert!(record.ordered_audio_candidate.is_none());
    assert_eq!(host.capture.sent, 0);
    let opened = host
        .broker
        .calls
        .iter()
        .any(|call| call.service == ENCODER && call.payload["action"] == "open");
    assert_ne!(opened, unsupported_layout);
    let closed = host.request(
        "close-rejected",
        json!({"action":"close","ticket":prepared["ticket"]}),
    );
    assert_eq!(closed["state"], "closed", "{closed}");
}
#[test]
#[ignore = "requires separately built synthetic encoder libraries and loopback sockets"]
fn broker_managed_rtsp_audio_route_uses_independent_providers() {
    for (variable, codec, marker) in [
        ("AUDIO_FIXTURE_AC3", "AC3", &[0xa1, 0x11, 0x33, 0x44][..]),
        ("AUDIO_FIXTURE_EAC3", "EAC3", &[0xe2, 0x22, 0x66, 0x77][..]),
    ] {
        let path = std::env::var(variable).unwrap();
        let generation = if codec == "AC3" { 101 } else { 202 };
        let mut host = fixture(&path, codec);
        host.encoder.busy_once();
        exchange(&mut host, codec, marker, true, "first", None);
        // A restarted encoder gets a new offer generation; the old owned object
        // is retired only after TEARDOWN has released its bindings.
        let replacement = Provider::load(&path);
        replacement.offer(generation + 1, 4);
        replacement.busy_once();
        host._factories[4].table = unsafe { &(*replacement.table).base };
        host.encoder = replacement;
        host.capture.sent = 0;
        host.capture.busy = 0;
        host.capture.video_draining = false;
        host.capture.video_ended = false;
        exchange(&mut host, codec, marker, true, "second", Some(generation));
        assert_eq!(
            host.broker
                .calls
                .iter()
                .filter(|call| call.service == ENCODER && call.payload["action"] == "open")
                .count(),
            2
        );
        assert_eq!(
            host.broker
                .calls
                .iter()
                .filter(|call| call.service == ENCODER && call.payload["action"] == "close")
                .count(),
            2
        );
        let mut gap = fixture(&path, codec);
        gap.capture.gap = true;
        gap.encoder.busy_once();
        exchange(&mut gap, codec, marker, true, "gap", None);
        let mut fallback = fixture(&path, codec);
        exchange(&mut fallback, codec, marker, false, "fallback", None);
        let mut layout = fixture(&path, codec);
        rejected_offer(&mut layout, true);
        let mut variable = fixture(&path, codec);
        variable.encoder.offer(generation, 0);
        rejected_offer(&mut variable, false);
    }
}
