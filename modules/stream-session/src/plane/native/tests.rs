use super::*;
use amaterasu_module_sdk::{Command, Response};

#[test]
fn closed_control_lane_is_distinct_from_other_native_closures() {
    assert!(matches!(
        Delivery::<()>::default().finish(MEDIA_CLOSED, DeliveryLane::Control),
        Err("native_control_closed")
    ));
    assert!(matches!(
        Delivery::<()>::default().finish(MEDIA_CLOSED, DeliveryLane::Video),
        Err("native_stream_closed")
    ));
}

#[derive(Default)]
struct Provider {
    duplicate: bool,
    invalid_status: bool,
    audio_failed: bool,
    send_busy: bool,
    sent_control: Vec<(u64, u32, u32, u32, Vec<u8>)>,
    control_failed: bool,
    pcm_failed: bool,
    alternate_audio: bool,
    submitted_pcm: bool,
    /// Test-only pause inside the synchronous native transport send.
    blocked_send: Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>)>,
}

struct Host {
    services: crate::providers::ProviderServices,
    provider: Box<Provider>,
    video: VideoApi,
    capture: AudioCaptureApi,
    encoder: AudioEncoderApi,
    media: MediaTransportApi,
    control: ControlTransportApi,
    released: Vec<u64>,
    bindings: Vec<String>,
    reject_capture: bool,
    typed_contracts: Vec<(String, String)>,
}

impl Host {
    fn new() -> Self {
        let mut provider = Box::<Provider>::default();
        let context = (&mut *provider as *mut Provider).cast();
        Self {
            services: Default::default(),
            provider,
            video: VideoApi {
                base: prefix::<VideoApi>(context),
                receive: video,
            },
            capture: AudioCaptureApi {
                base: prefix::<AudioCaptureApi>(context),
                read_audio: pcm,
            },
            encoder: AudioEncoderApi {
                base: prefix::<AudioEncoderApi>(context),
                submit: submit_pcm,
                receive: audio,
                drain,
            },
            media: MediaTransportApi {
                base: prefix::<MediaTransportApi>(context),
                submit_video,
                submit_audio,
                drain,
            },
            control: ControlTransportApi {
                base: prefix::<ControlTransportApi>(context),
                send: send_control,
                receive: control,
            },
            released: Vec::new(),
            bindings: Vec::new(),
            reject_capture: false,
            typed_contracts: Vec::new(),
        }
    }

    fn runtime(&mut self) -> RuntimeClient {
        // SAFETY: tests keep this host and all stable table addresses live until
        // backend bindings are dropped, with no concurrent host invocation.
        unsafe {
            RuntimeClient::from_endpoint(HostApi {
                header: AbiHeader::of::<HostApi>(),
                context: (self as *mut Self).cast(),
                invoke,
            })
        }
    }
}

fn prefix<T>(context: *mut c_void) -> NativeInterface {
    NativeInterface {
        header: AbiHeader::of::<T>(),
        context,
        release: None,
    }
}

unsafe extern "C" fn invoke(context: *mut c_void, input: BorrowedBytes, sink: ReplySink) -> Status {
    // SAFETY: the test endpoint lends exclusive host state and initialized bytes.
    let host = unsafe { &mut *context.cast::<Host>() };
    let command: Command = serde_json::from_slice(unsafe { input.read().unwrap() }).unwrap();
    let response = match command {
        Command::NativeBindTyped {
            service, contract, ..
        } => {
            let expected = if service == host.services.video {
                amaterasu_module_sdk::video::CONTRACT
            } else if service == host.services.audio_capture {
                amaterasu_module_sdk::audio::CAPTURE_CONTRACT
            } else if service == host.services.audio_encoder {
                amaterasu_module_sdk::audio::ENCODER_CONTRACT
            } else if service == host.services.media_transport {
                amaterasu_module_sdk::transport::MEDIA_CONTRACT
            } else if service == host.services.control_transport {
                amaterasu_module_sdk::transport::CONTROL_CONTRACT
            } else {
                panic!("unexpected typed service {service}");
            };
            assert_eq!(contract, expected);
            host.typed_contracts.push((service.clone(), contract));
            bind_service(host, service)
        }
        Command::NativeRelease { binding } => {
            host.released.push(binding);
            Response::Ok
        }
        _ => panic!("unexpected command"),
    };
    unsafe { sink.send(&serde_json::to_vec(&response).unwrap()) };
    STATUS_OK
}

fn bind_service(host: &mut Host, service: String) -> Response {
    host.bindings.push(service.clone());
    if host.reject_capture && service == host.services.audio_capture {
        Response::Error {
            code: "denied".into(),
            message: "denied".into(),
        }
    } else {
        let (binding, address) = if service == host.services.video {
            (1, &host.video.base as *const _ as usize)
        } else if service == host.services.audio_capture {
            (2, &host.capture.base as *const _ as usize)
        } else if service == host.services.audio_encoder {
            (3, &host.encoder.base as *const _ as usize)
        } else if service == host.services.media_transport {
            (4, &host.media.base as *const _ as usize)
        } else if service == host.services.control_transport {
            (5, &host.control.base as *const _ as usize)
        } else {
            panic!("unexpected service {service}");
        };
        Response::NativeBound { binding, address }
    }
}

unsafe extern "C" fn video(context: *mut c_void, stream: u64, sink: PacketSink) -> Status {
    assert_eq!(stream, 10);
    // SAFETY: the stable boxed provider is exclusively used by this synchronous test.
    let provider = unsafe { &mut *context.cast::<Provider>() };
    let mut bytes = [0, 0, 1, 0x65, 9];
    let packet = EncodedPacket {
        codec: CODEC_H264,
        flags: FLAG_END_OF_ACCESS_UNIT,
        data: bytes.as_ptr(),
        len: bytes.len(),
        timestamp_us: 7000,
        sequence: 11,
    };
    // SAFETY: packet data and callback state remain live until each call returns.
    let status = unsafe { sink.send(&packet) };
    if provider.duplicate {
        unsafe { sink.send(&packet) };
    }
    bytes.fill(0);
    if provider.invalid_status {
        MEDIA_PENDING
    } else {
        status
    }
}

unsafe extern "C" fn pcm(context: *mut c_void, _: u64, sink: AudioSink) -> Status {
    // SAFETY: the fake provider and callback are live for this synchronous call.
    let provider = unsafe { &*context.cast::<Provider>() };
    if provider.pcm_failed {
        return MEDIA_BACKEND_ERROR;
    }
    if !provider.alternate_audio {
        return MEDIA_PENDING;
    }
    let samples = [7u8; 960];
    let frame = AudioFrame {
        sample_rate: 48_000,
        channels: 2,
        format: PCM_I16,
        frames: 240,
        flags: 0,
        data: samples.as_ptr(),
        len: samples.len(),
        timestamp_us: 10,
        sequence: 1,
    };
    unsafe { sink.send(&frame) }
}
unsafe extern "C" fn audio(context: *mut c_void, _: u64, sink: AudioPacketSink) -> Status {
    // SAFETY: the fake provider and callback are live for this synchronous call.
    let provider = unsafe { &*context.cast::<Provider>() };
    if provider.audio_failed {
        return MEDIA_BACKEND_ERROR;
    }
    if !provider.submitted_pcm {
        return MEDIA_PENDING;
    }
    let bytes = [1u8, 2, 3];
    let packet = EncodedAudioPacket {
        codec: CODEC_OPUS,
        flags: 0,
        sample_rate: 48_000,
        channels: 2,
        frames: 240,
        data: bytes.as_ptr(),
        len: bytes.len(),
        timestamp_us: 10,
        sequence: 1,
    };
    unsafe { sink.send(&packet) }
}
unsafe extern "C" fn control(context: *mut c_void, _: u64, _: ControlMessageSink) -> Status {
    if unsafe { &*context.cast::<Provider>() }.control_failed {
        MEDIA_BACKEND_ERROR
    } else {
        MEDIA_PENDING
    }
}
unsafe extern "C" fn submit_pcm(context: *mut c_void, _: u64, frame: *const AudioFrame) -> Status {
    // SAFETY: the test host supplies a live provider and a synchronous frame borrow.
    let provider = unsafe { &mut *context.cast::<Provider>() };
    if !provider.alternate_audio {
        return MEDIA_BUSY;
    }
    assert_eq!(unsafe { (&*frame).bytes().unwrap() }, &[7u8; 960]);
    provider.submitted_pcm = true;
    STATUS_OK
}
unsafe extern "C" fn submit_video(_: *mut c_void, _: u64, _: *const EncodedPacket) -> Status {
    MEDIA_BUSY
}
unsafe extern "C" fn submit_audio(_: *mut c_void, _: u64, _: *const EncodedAudioPacket) -> Status {
    MEDIA_BUSY
}
unsafe extern "C" fn send_control(
    context: *mut c_void,
    stream: u64,
    message: *const ControlMessage,
) -> Status {
    if context.is_null() || message.is_null() {
        return STATUS_INVALID_ARGUMENT;
    }
    // SAFETY: the backend lends a live message and the boxed test provider stays
    // allocated until all native calls have returned.
    let provider = unsafe { &mut *context.cast::<Provider>() };
    let message = unsafe { &*message };
    let Ok(bytes) = (unsafe { message.bytes() }) else {
        return STATUS_INVALID_ARGUMENT;
    };
    if let Some((entered, resume)) = provider.blocked_send.take()
        && (entered.send(()).is_err()
            || resume
                .recv_timeout(std::time::Duration::from_secs(5))
                .is_err())
    {
        return MEDIA_BACKEND_ERROR;
    }
    provider.sent_control.push((
        stream,
        message.message_type,
        message.channel,
        message.delivery,
        bytes.to_vec(),
    ));
    if provider.send_busy {
        MEDIA_BUSY
    } else {
        STATUS_OK
    }
}
unsafe extern "C" fn drain(_: *mut c_void, _: u64) -> Status {
    STATUS_OK
}

fn handles() -> Handles {
    Handles {
        hdr: false,
        video: 10,
        audio_capture: Some(20),
        audio_encoder: Some(30),
        media_transport: 40,
        control_transport: 50,
        stream_audio: true,
        audio_channels: 2,
        video_codec: CODEC_H264,
    }
}

fn disabled_audio_handles() -> Handles {
    Handles {
        hdr: false,
        video: 10,
        audio_capture: None,
        audio_encoder: None,
        media_transport: 40,
        control_transport: 50,
        stream_audio: false,
        audio_channels: 2,
        video_codec: CODEC_H264,
    }
}

#[test]
fn actual_native_tables_copy_borrowed_bytes_and_release_all_bindings() {
    let mut host = Host::new();
    let mut backend = NativeBackend::new(
        &host.runtime(),
        handles(),
        &crate::providers::ProviderServices::default(),
    )
    .unwrap();
    let packet = backend.video().unwrap().unwrap();
    let mut assembly = VideoAssembly::new(CODEC_H264);
    assembly.push(packet).unwrap();
    let header = assembly.header().unwrap();
    // SAFETY: assembly owns the copied bytes despite provider overwriting its source.
    assert_eq!(unsafe { header.bytes().unwrap() }, [0, 0, 1, 0x65, 9]);
    assert!(!backend.submit_video(&header).unwrap());
    assert_eq!(host.typed_contracts.len(), 5);
    drop(backend);
    assert_eq!(host.released, [1, 2, 3, 4, 5]);
}

#[test]
fn feedback_uses_the_bound_authenticated_stream_and_keeps_enet_routing_metadata() {
    let mut host = Host::new();
    let mut backend = NativeBackend::new(
        &host.runtime(),
        handles(),
        &crate::providers::ProviderServices::default(),
    )
    .unwrap();
    let event = HostFeedback::Rumble {
        controller: 7,
        low_frequency: 0x1234,
        high_frequency: 0xabcd,
    };
    let message = encode_host_feedback(event).unwrap();
    assert!(backend.send_feedback(&message).unwrap());
    assert_eq!(
        host.provider.sent_control,
        [(
            50,
            0x010b,
            0,
            CONTROL_RELIABLE,
            vec![0xee, 0xff, 0xc0, 0, 7, 0, 0x34, 0x12, 0xcd, 0xab],
        )]
    );

    let pcm = encode_host_feedback(HostFeedback::HapticsPcm {
        controller: 7,
        sequence: 1,
        frames: 1,
        samples_s16le: vec![0; 4],
    })
    .unwrap();
    host.provider.send_busy = true;
    assert!(!backend.send_feedback(&pcm).unwrap());
    assert_eq!(host.provider.sent_control[1].0, 50);
    assert_eq!(host.provider.sent_control[1].1, 0x5601);
    assert_eq!(host.provider.sent_control[1].2, 8);
    assert_eq!(host.provider.sent_control[1].3, CONTROL_UNRELIABLE);
    assert_eq!(host.provider.sent_control[1].4, pcm.payload);
}

#[test]
fn partial_native_bind_failure_releases_previously_acquired_table() {
    let mut host = Host::new();
    host.reject_capture = true;
    assert_eq!(
        NativeBackend::new(
            &host.runtime(),
            handles(),
            &crate::providers::ProviderServices::default()
        )
        .err(),
        Some("native_binding_failed")
    );
    assert_eq!(host.released, [1]);
}

#[test]
fn disabled_audio_binds_no_capture_or_encoder_tables() {
    let mut host = Host::new();
    host.reject_capture = true;
    let backend = NativeBackend::new(
        &host.runtime(),
        disabled_audio_handles(),
        &crate::providers::ProviderServices::default(),
    )
    .unwrap();
    assert_eq!(
        host.bindings,
        [
            "amaterasu.video/1",
            "amaterasu.media-transport/1",
            "amaterasu.control-transport/1"
        ]
    );
    drop(backend);
    assert_eq!(host.released, [1, 4, 5]);
}

#[test]
fn repeated_callbacks_and_output_with_pending_status_are_protocol_errors() {
    let mut host = Host::new();
    host.provider.duplicate = true;
    let mut backend = NativeBackend::new(
        &host.runtime(),
        handles(),
        &crate::providers::ProviderServices::default(),
    )
    .unwrap();
    assert_eq!(backend.video().err(), Some("native_multiple_callbacks"));
    host.provider.duplicate = false;
    host.provider.invalid_status = true;
    assert_eq!(backend.video().err(), Some("native_delivery_failed_video"));
    drop(backend);
}

#[test]
fn native_delivery_failures_identify_the_control_and_pcm_lane() {
    let mut host = Host::new();
    let mut backend = NativeBackend::new(
        &host.runtime(),
        handles(),
        &crate::providers::ProviderServices::default(),
    )
    .unwrap();
    host.provider.control_failed = true;
    assert_eq!(
        backend.control().err(),
        Some("native_delivery_failed_control")
    );
    host.provider.control_failed = false;
    host.provider.pcm_failed = true;
    assert_eq!(backend.pcm().err(), Some("native_delivery_failed_pcm"));
}

#[test]
fn encoder_backend_status_is_distinct_from_other_native_failures() {
    let mut host = Host::new();
    host.provider.audio_failed = true;
    let mut backend = NativeBackend::new(
        &host.runtime(),
        handles(),
        &crate::providers::ProviderServices::default(),
    )
    .unwrap();
    assert_eq!(backend.audio().err(), Some("audio_encoder_failed"));
    assert_eq!(
        encoder_accepted(MEDIA_BACKEND_ERROR),
        Err("audio_encoder_failed")
    );
    assert_eq!(
        accepted(MEDIA_BACKEND_ERROR, "native_video_submit_failed"),
        Err("native_video_submit_failed")
    );
}

#[test]
fn alternative_provider_roles_bind_and_release_the_exact_selected_interfaces() {
    let mut host = Host::new();
    let services = crate::providers::ProviderServices {
        video: "acme.frames/1".into(),
        audio_capture: "acme.microphone/1".into(),
        audio_encoder: "acme.opus/1".into(),
        media_transport: "acme.delivery/1".into(),
        control_transport: "acme.control/1".into(),
        ..Default::default()
    };
    host.services = services.clone();
    host.provider.alternate_audio = true;
    let mut backend = NativeBackend::new(&host.runtime(), handles(), &services).unwrap();
    assert!(backend.video().unwrap().is_some());
    let pcm = backend.pcm().unwrap().unwrap();
    assert!(backend.submit_pcm(&pcm.header()).unwrap());
    let encoded = backend.audio().unwrap().unwrap();
    let encoded_header = encoded.header();
    assert_eq!(unsafe { encoded_header.bytes().unwrap() }, &[1, 2, 3]);
    assert_eq!(
        host.bindings,
        [
            services.video,
            services.audio_capture,
            services.audio_encoder,
            services.media_transport,
            services.control_transport,
        ]
    );
    drop(backend);
    assert_eq!(host.released, [1, 2, 3, 4, 5]);
}

#[test]
fn test_ticket_harness_holds_drain_until_native_feedback_send_returns() {
    use std::sync::{Arc, RwLock, mpsc};
    use std::time::Duration;
    // This lock only models the future protocol-owned gate. It does not install
    // a source route or authorize production feedback.
    let gate = Arc::new(RwLock::new(true));
    let worker_gate = Arc::clone(&gate);
    let (entered_tx, entered_rx) = mpsc::channel();
    let (resume_tx, resume_rx) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        let mut host = Host::new();
        host.provider.blocked_send = Some((entered_tx, resume_rx));
        let mut backend = NativeBackend::new(
            &host.runtime(),
            handles(),
            &crate::providers::ProviderServices::default(),
        )
        .unwrap();
        let message = encode_host_feedback(HostFeedback::Rumble {
            controller: 7,
            low_frequency: 1,
            high_frequency: 2,
        })
        .unwrap();
        let ticket = worker_gate.read().unwrap();
        assert!(*ticket);
        let accepted = backend.send_feedback(&message).unwrap();
        drop(ticket);
        drop(backend);
        (accepted, host.provider.sent_control.len())
    });
    entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(
        gate.try_write().is_err(),
        "drain must not complete inside native send"
    );
    let revoke_gate = Arc::clone(&gate);
    let (started_tx, started_rx) = mpsc::channel();
    let (drained_tx, drained_rx) = mpsc::channel();
    let revoke = std::thread::spawn(move || {
        started_tx.send(()).unwrap();
        let mut state = revoke_gate.write().unwrap();
        *state = false;
        drained_tx.send(()).unwrap();
    });
    started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(drained_rx.try_recv().is_err());
    resume_tx.send(()).unwrap();
    let (accepted, deliveries) = worker.join().unwrap();
    drained_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    revoke.join().unwrap();
    assert!(accepted);
    assert_eq!(deliveries, 1);
    assert!(
        !*gate.read().unwrap(),
        "future admission must fail after drain"
    );
}
