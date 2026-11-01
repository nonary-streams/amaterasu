//! Independently built synthetic encoders through the unchanged production pump
//! and production media transport. Fixture bytes are not Dolby bitstreams.
use super::*;
use amaterasu_module_sdk::{Command, Response, abi::*};
use libloading::Library;
use std::{ffi::c_void, net::UdpSocket};

const ORDERED_CAPTURE: &str = "fixture.capture-ordered/1";
const ORDERED_ENCODER: &str = "fixture.encoder-ordered/1";
const ORDERED_TRANSPORT: &str = "fixture.transport-ordered/1";

struct Host {
    video: VideoApi,
    capture: AudioCaptureApi,
    encoder: AudioEncoderApi,
    media: MediaTransportApi,
    control: ControlTransportApi,
    ordered_capture: OrderedAudioCaptureApiV2,
    ordered_transport: OrderedAudioTransportApiV2,
    external_encoder: *const OrderedAudioEncoderApiV2,
    transport: amaterasu_media_transport::test_support::DirectTransport,
    samples: Vec<u8>,
    channels: u32,
    reordered: bool,
    capture_packets: u32,
    first_audio_busy: bool,
    releases: usize,
}

fn prefix<T>(context: *mut c_void, major: u32) -> NativeInterface {
    NativeInterface {
        header: AbiHeader {
            size: size_of::<T>() as u32,
            major,
            minor: 0,
        },
        context,
        release: None,
    }
}

unsafe extern "C" fn invoke(context: *mut c_void, input: BorrowedBytes, sink: ReplySink) -> Status {
    // SAFETY: the test retains host and callback state through this invocation.
    let host = unsafe { &mut *context.cast::<Host>() };
    let command: Command = serde_json::from_slice(unsafe { input.read().unwrap() }).unwrap();
    let response = match command {
        Command::NativeBindTyped {
            service, contract, ..
        } => {
            let (expected, address) = match service.as_str() {
                "amaterasu.video/1" => (
                    amaterasu_module_sdk::video::CONTRACT,
                    &host.video.base as *const _ as usize,
                ),
                "amaterasu.audio/1" => (
                    amaterasu_module_sdk::audio::CAPTURE_CONTRACT,
                    &host.capture.base as *const _ as usize,
                ),
                "amaterasu.audio-encode/1" => (
                    amaterasu_module_sdk::audio::ENCODER_CONTRACT,
                    &host.encoder.base as *const _ as usize,
                ),
                "amaterasu.media-transport/1" => (
                    amaterasu_module_sdk::transport::MEDIA_CONTRACT,
                    &host.media.base as *const _ as usize,
                ),
                "amaterasu.control-transport/1" => (
                    amaterasu_module_sdk::transport::CONTROL_CONTRACT,
                    &host.control.base as *const _ as usize,
                ),
                ORDERED_CAPTURE => (
                    amaterasu_module_sdk::audio::ORDERED_CAPTURE_CONTRACT,
                    &host.ordered_capture.base as *const _ as usize,
                ),
                ORDERED_ENCODER => (
                    amaterasu_module_sdk::audio::ORDERED_ENCODER_CONTRACT,
                    host.external_encoder as usize,
                ),
                ORDERED_TRANSPORT => (
                    amaterasu_module_sdk::audio::ORDERED_TRANSPORT_CONTRACT,
                    &host.ordered_transport.base as *const _ as usize,
                ),
                _ => panic!("unexpected service {service}"),
            };
            assert_eq!(contract, expected);
            Response::NativeBound {
                binding: address as u64,
                address,
            }
        }
        Command::NativeRelease { .. } => {
            host.releases += 1;
            Response::Ok
        }
        _ => panic!("unexpected command"),
    };
    unsafe { sink.send(&serde_json::to_vec(&response).unwrap()) };
    STATUS_OK
}

unsafe extern "C" fn no_video(_: *mut c_void, _: u64, _: PacketSink) -> Status {
    MEDIA_PENDING
}
unsafe extern "C" fn no_capture(_: *mut c_void, _: u64, _: AudioSink) -> Status {
    MEDIA_PENDING
}
unsafe extern "C" fn no_pcm(_: *mut c_void, _: u64, _: *const AudioFrame) -> Status {
    STATUS_INVALID_ARGUMENT
}
unsafe extern "C" fn no_audio(_: *mut c_void, _: u64, _: AudioPacketSink) -> Status {
    MEDIA_PENDING
}
unsafe extern "C" fn done(_: *mut c_void, _: u64) -> Status {
    STATUS_OK
}
unsafe extern "C" fn no_control(_: *mut c_void, _: u64, _: ControlMessageSink) -> Status {
    MEDIA_PENDING
}
unsafe extern "C" fn control_send(_: *mut c_void, _: u64, _: *const ControlMessage) -> Status {
    STATUS_OK
}
unsafe extern "C" fn legacy_video(_: *mut c_void, _: u64, _: *const EncodedPacket) -> Status {
    STATUS_INVALID_ARGUMENT
}
unsafe extern "C" fn legacy_audio(_: *mut c_void, _: u64, _: *const EncodedAudioPacket) -> Status {
    STATUS_INVALID_ARGUMENT
}

unsafe extern "C" fn capture(
    context: *mut c_void,
    stream: u64,
    sink: OrderedAudioSinkV2,
) -> Status {
    // SAFETY: the pump calls the live test-owned capture binding synchronously.
    let host = unsafe { &mut *context.cast::<Host>() };
    if stream != 20 {
        return STATUS_INVALID_ARGUMENT;
    }
    if host.capture_packets == 4 {
        return MEDIA_PENDING;
    }
    let mut actual_layout = layout(host.channels);
    if host.reordered {
        actual_layout.lanes.swap(0, 1);
    }
    let frame = OrderedAudioFrameV2 {
        size: size_of::<OrderedAudioFrameV2>() as u32,
        version: AUDIO_ORDERED_VERSION,
        stream,
        generation: 1,
        frame: AudioFrame {
            sample_rate: 48_000,
            channels: host.channels,
            format: PCM_F32,
            frames: 480,
            flags: 0,
            data: host.samples.as_ptr(),
            len: host.samples.len(),
            timestamp_us: 100_000 + u64::from(host.capture_packets) * 10_000,
            sequence: u64::from(host.capture_packets),
        },
        layout: actual_layout,
    };
    let status = unsafe { (sink.write)(sink.context, &frame) };
    if status == STATUS_OK {
        host.capture_packets += 1;
    }
    status
}
unsafe extern "C" fn probe(
    context: *mut c_void,
    stream: u64,
    selection: *const AudioTransportSelectionV2,
) -> Status {
    if context.is_null() || selection.is_null() {
        return STATUS_INVALID_ARGUMENT;
    }
    let host = unsafe { &mut *context.cast::<Host>() };
    host.transport.probe(stream, unsafe { &*selection })
}
unsafe extern "C" fn prepare(
    context: *mut c_void,
    stream: u64,
    selection: *const AudioTransportSelectionV2,
) -> Status {
    if context.is_null() || selection.is_null() {
        return STATUS_INVALID_ARGUMENT;
    }
    let host = unsafe { &mut *context.cast::<Host>() };
    host.transport.prepare(stream, unsafe { &*selection })
}
unsafe extern "C" fn submit(
    context: *mut c_void,
    stream: u64,
    packet: *const NegotiatedAudioPacket,
) -> Status {
    if context.is_null() || packet.is_null() {
        return STATUS_INVALID_ARGUMENT;
    }
    let host = unsafe { &mut *context.cast::<Host>() };
    if host.first_audio_busy {
        host.first_audio_busy = false;
        return MEDIA_BUSY;
    }
    // SAFETY: pump owns the packet and its bytes until this call returns.
    unsafe { host.transport.submit(stream, &*packet) }
}
fn layout(channels: u32) -> AudioChannelLayoutV2 {
    AudioChannelLayoutV2 {
        size: size_of::<AudioChannelLayoutV2>() as u32,
        version: AUDIO_ORDERED_VERSION,
        channels,
        reserved: 0,
        mask: if channels == 2 { 3 } else { 63 },
        lanes: if channels == 2 {
            [1, 2, 0, 0, 0, 0, 0, 0]
        } else {
            [1, 2, 4, 8, 16, 32, 0, 0]
        },
    }
}

fn client_accepts(codec: amaterasu_gamestream::sdp::AudioCodec, channels: u32) {
    use amaterasu_gamestream::{
        rtsp::{
            Method, Request,
            negotiation::{Action, Capabilities, Negotiation, Step, TrustedLaunch},
        },
        sdp::{ColorMode, TransportSupport},
    };
    let target = "rtspenc://127.0.0.1:48010/fixture";
    let mut negotiation = Negotiation::new(
        TrustedLaunch {
            epoch: 7,
            rtsp_target: target.into(),
            session_token: "fixture-session".into(),
            encrypted_rtsp: true,
            audio_port: 49000,
            video_port: 49002,
            control_port: 49004,
            control_connect_data: 123,
            udp_discovery: None,
        },
        Capabilities {
            transport: TransportSupport {
                stereo_audio: true,
                enet_control: true,
                encrypted_control: true,
                control_v2: true,
                encrypted_audio: true,
                encrypted_video: true,
                fec: true,
                ..Default::default()
            },
            supported_colors: vec![ColorMode::Bt709Limited],
            max_width: 1920,
            max_height: 1080,
            max_fps: 60,
            max_bitrate_kbps: 20000,
            require_video_encryption: false,
            require_audio_encryption: false,
        },
    )
    .unwrap()
    .with_audio_offer(codec)
    .unwrap();
    let request = |method, target: &str, cseq, session: bool| Request {
        method,
        target: target.into(),
        cseq,
        headers: if session {
            vec![("session".into(), "fixture-session".into())]
        } else {
            vec![]
        },
        body: vec![],
    };
    negotiation
        .handle(7, &request(Method::Options, target, 1, false))
        .unwrap();
    let mut describe = request(Method::Describe, target, 2, false);
    describe
        .headers
        .push(("accept".into(), "application/sdp".into()));
    let Step::Reply(description) = negotiation.handle(7, &describe).unwrap() else {
        panic!()
    };
    assert!(
        String::from_utf8(description)
            .unwrap()
            .contains(&format!("a=x-ss-audio-codec-offer:{}", codec.wire_name()))
    );
    for (sequence, stream) in [
        (3, "streamid=audio/0/0"),
        (4, "streamid=video/0/0"),
        (5, "streamid=control/13/0"),
    ] {
        let mut setup = request(Method::Setup, stream, sequence, sequence > 3);
        setup.headers.push((
            "transport".into(),
            "unicast;X-GS-ClientPort=50000-50001".into(),
        ));
        assert!(matches!(negotiation.handle(7, &setup), Ok(Step::Reply(_))));
    }
    let mut announce = request(Method::Announce, "streamid=control/13/0", 6, true);
    announce
        .headers
        .push(("content-type".into(), "application/sdp".into()));
    announce.body =
        include_str!("../../../../crates/gamestream/src/sdp/fixtures/moonlight-h264.sdp")
            .replace(
                "x-ss-general.encryptionEnabled:0",
                "x-ss-general.encryptionEnabled:1",
            )
            .into_bytes();
    if channels == 6 {
        announce.body = String::from_utf8(announce.body)
            .unwrap()
            .replace("numChannels:2", "numChannels:6")
            .replace("channelMask:3", "channelMask:63")
            .replace("surround.enable:0", "surround.enable:1")
            .into_bytes();
    }
    announce.body.extend_from_slice(
        format!("a=x-ss-audio-codec-accept:{}\r\n", codec.wire_name()).as_bytes(),
    );
    let Step::Action {
        ticket,
        action: Action::Announce(announce),
    } = negotiation.handle(7, &announce).unwrap()
    else {
        panic!()
    };
    assert_eq!(announce.audio.codec_accept, Some(codec));
    assert!(
        String::from_utf8(negotiation.complete(7, ticket, true).unwrap())
            .unwrap()
            .contains(&format!("X-SS-Audio-Codec: {}", codec.wire_name()))
    );
}

#[test]
#[ignore = "requires separately built ordered encoder fixture libraries and loopback UDP"]
fn production_pump_accepts_two_independent_ordered_encoder_artifacts() {
    for (path_var, codec, framing, expected) in [
        (
            "AUDIO_FIXTURE_AC3",
            AUDIO_CODEC_AC3,
            AUDIO_FRAMING_AC3_SYNCFRAME,
            [0xa1, 0x11, 0x33, 0x44],
        ),
        (
            "AUDIO_FIXTURE_EAC3",
            AUDIO_CODEC_EAC3,
            AUDIO_FRAMING_EAC3_SYNCFRAME,
            [0xe2, 0x22, 0x66, 0x77],
        ),
    ] {
        for channels in [2, 6] {
            for reordered in [false, true] {
                let video_peer = UdpSocket::bind("127.0.0.1:0").unwrap();
                let audio_peer = UdpSocket::bind("127.0.0.1:0").unwrap();
                audio_peer
                    .set_read_timeout(Some(std::time::Duration::from_secs(1)))
                    .unwrap();
                // SAFETY: test loads its separately built fixture and keeps it live through destruction.
                let library = unsafe { Library::new(std::env::var(path_var).unwrap()).unwrap() };
                let create: libloading::Symbol<
                    unsafe extern "C" fn(u32) -> *mut OrderedAudioEncoderApiV2,
                > = unsafe { library.get(b"fixture_create_v2_channels\0").unwrap() };
                let destroy: libloading::Symbol<
                    unsafe extern "C" fn(*mut OrderedAudioEncoderApiV2),
                > = unsafe { library.get(b"fixture_destroy_v2\0").unwrap() };
                let external = unsafe { create(channels) };
                assert!(!external.is_null());
                let transport = amaterasu_media_transport::test_support::DirectTransport::open(serde_json::json!({
            "action":"open","peer_ip":"127.0.0.1","video_port":video_peer.local_addr().unwrap().port(),
            "audio_port":audio_peer.local_addr().unwrap().port(),"stream_audio":true,
            "audio":{"packet_ms":10,"channels":channels,"wire_payload_bytes":4,"ssrc":0}}), "consumer").unwrap();
                let stream = transport.stream();
                let mut host = Box::new(Host {
                    video: VideoApi {
                        base: prefix::<VideoApi>(std::ptr::null_mut(), 1),
                        receive: no_video,
                    },
                    capture: AudioCaptureApi {
                        base: prefix::<AudioCaptureApi>(std::ptr::null_mut(), 1),
                        read_audio: no_capture,
                    },
                    encoder: AudioEncoderApi {
                        base: prefix::<AudioEncoderApi>(std::ptr::null_mut(), 1),
                        submit: no_pcm,
                        receive: no_audio,
                        drain: done,
                    },
                    media: MediaTransportApi {
                        base: prefix::<MediaTransportApi>(std::ptr::null_mut(), 1),
                        submit_video: legacy_video,
                        submit_audio: legacy_audio,
                        drain: done,
                    },
                    control: ControlTransportApi {
                        base: prefix::<ControlTransportApi>(std::ptr::null_mut(), 1),
                        send: control_send,
                        receive: no_control,
                    },
                    ordered_capture: OrderedAudioCaptureApiV2 {
                        base: prefix::<OrderedAudioCaptureApiV2>(std::ptr::null_mut(), 2),
                        read_audio: capture,
                    },
                    ordered_transport: OrderedAudioTransportApiV2 {
                        base: prefix::<OrderedAudioTransportApiV2>(std::ptr::null_mut(), 2),
                        probe,
                        prepare,
                        submit,
                    },
                    external_encoder: external,
                    transport,
                    samples: vec![0; 480 * channels as usize * 4],
                    channels,
                    reordered,
                    capture_packets: 0,
                    first_audio_busy: true,
                    releases: 0,
                });
                let context = (&mut *host as *mut Host).cast();
                host.ordered_capture.base.context = context;
                host.ordered_transport.base.context = context;
                let runtime = unsafe {
                    RuntimeClient::from_endpoint(HostApi {
                        header: AbiHeader::of::<HostApi>(),
                        context,
                        invoke,
                    })
                };
                let codec_name = if codec == AUDIO_CODEC_AC3 {
                    "AC3"
                } else {
                    "EAC3"
                };
                let configured: crate::model::AudioExtensionRoute = serde_json::from_value(serde_json::json!({
            "codec": codec_name, "encoder_control_service": ORDERED_ENCODER, "encoder_service": ORDERED_ENCODER,
            "capture_service": ORDERED_CAPTURE, "transport_service": ORDERED_TRANSPORT,
        })).unwrap();
                let configuration: crate::model::PrepareConfig =
            serde_json::from_value(serde_json::json!({
                "peer_ip":"127.0.0.1", "key_hex":"00000000000000000000000000000000",
                "key_id":0,"connect_data":0,"operation_id":"ordered-fixture",
                "audio":{"channels":channels,"channel_mask":if channels == 2 {3} else {63},
                    "layout":if channels == 2 {"stereo"} else {"5.1"},"packet_ms":10,"bitrate":128000},
            }))
            .unwrap();
                let candidate = probe_ordered_audio_chain(
                    &runtime,
                    &configured,
                    19,
                    stream,
                    &configuration,
                    [1; 16],
                )
                .unwrap();
                assert_eq!(candidate.format().format.framing, framing);
                client_accepts(
                    if codec == AUDIO_CODEC_AC3 {
                        amaterasu_gamestream::sdp::AudioCodec::Ac3
                    } else {
                        amaterasu_gamestream::sdp::AudioCodec::Eac3
                    },
                    channels,
                );
                let route = OrderedAudioRoute {
                    encoder_service: ORDERED_ENCODER.into(),
                    capture_service: ORDERED_CAPTURE.into(),
                    transport_service: ORDERED_TRANSPORT.into(),
                    candidate,
                    clock_epoch: [1; 16],
                };
                let handles = Handles {
                    video: 10,
                    hdr: false,
                    audio_capture: Some(20),
                    audio_encoder: Some(30),
                    media_transport: stream,
                    control_transport: 50,
                    stream_audio: true,
                    audio_channels: channels,
                    video_codec: CODEC_H264,
                };
                let mut plane = Plane::new(&runtime, handles, &Default::default())
                    .unwrap()
                    .with_ordered_audio(&runtime, &route)
                    .unwrap();
                if reordered {
                    assert_eq!(
                        plane.tick(true).unwrap_err().code,
                        "ordered_encoder_configuration_failed"
                    );
                    drop(plane);
                    unsafe { destroy(external) };
                    continue;
                }
                assert_eq!(plane.tick(true).unwrap().accepted_media_payload_bytes, 0);
                assert_eq!(plane.tick(true).unwrap().accepted_media_payload_bytes, 4);
                host.transport.tick();
                let mut datagram = [0u8; 64];
                let bytes = audio_peer.recv(&mut datagram).unwrap();
                assert_eq!(&datagram[2..4], &0u16.to_be_bytes());
                assert_eq!(&datagram[4..8], &0u32.to_be_bytes());
                assert_eq!(&datagram[12..bytes], &expected);
                plane.stop();
                plane.tick(true).unwrap();
                plane.tick(true).unwrap();
                assert!(plane.pump.audio_ended);
                host.transport.tick();
                let second = audio_peer.recv(&mut datagram).unwrap();
                assert_eq!(&datagram[4..8], &32u32.to_be_bytes());
                assert_eq!(&datagram[12..second], &expected);
                drop(plane);
                assert!(host.releases >= 8);
                unsafe { destroy(external) };
            }
        }
    }
}
