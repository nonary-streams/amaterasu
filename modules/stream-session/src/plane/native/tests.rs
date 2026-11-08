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
    timed_video_calls: usize,
    timed_media_calls: usize,
    timed_media_busy: bool,
    timing_received: u64,
    presentation_received: u64,
    audio_unit_busy: bool,
    audio_units: Vec<(AudioFormatId, Vec<u8>, Vec<u8>)>,
    processor_pending: bool,
    processor_draining: bool,
}

struct Host {
    services: crate::providers::ProviderServices,
    provider: Box<Provider>,
    video: VideoApi,
    timed_video: Option<Box<VideoApiV1_3>>,
    capture: AudioCaptureApi,
    encoded_capture: EncodedAudioSourceApi,
    processor: AudioProcessorApi,
    use_encoded_capture: bool,
    encoder: AudioEncoderApi,
    media: MediaTransportApi,
    timed_media: Option<Box<MediaTransportApiV1_1>>,
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
            timed_video: None,
            timed_media: None,
            capture: AudioCaptureApi {
                base: prefix::<AudioCaptureApi>(context),
                read_audio: pcm,
            },
            encoded_capture: EncodedAudioSourceApi {
                base: prefix::<EncodedAudioSourceApi>(context),
                receive: audio,
                drain,
            },
            processor: AudioProcessorApi {
                base: prefix::<AudioProcessorApi>(context),
                submit: processor_submit,
                receive: processor_receive,
                drain: processor_drain,
            },
            use_encoded_capture: false,
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
                if host.use_encoded_capture {
                    amaterasu_module_sdk::audio::ENCODED_SOURCE_CONTRACT
                } else {
                    amaterasu_module_sdk::audio::CAPTURE_CONTRACT
                }
            } else if service == host.services.audio_encoder {
                amaterasu_module_sdk::audio::ENCODER_CONTRACT
            } else if host.services.audio_processor.as_ref() == Some(&service) {
                amaterasu_module_sdk::audio::PROCESSOR_CONTRACT
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
            (
                1,
                host.timed_video
                    .as_ref()
                    .map_or(&host.video.base as *const _ as usize, |table| {
                        &table.base.base as *const _ as usize
                    }),
            )
        } else if service == host.services.audio_capture {
            (
                2,
                if host.use_encoded_capture {
                    &host.encoded_capture.base as *const _ as usize
                } else {
                    &host.capture.base as *const _ as usize
                },
            )
        } else if service == host.services.audio_encoder {
            (3, &host.encoder.base as *const _ as usize)
        } else if host.services.audio_processor.as_ref() == Some(&service) {
            (6, &host.processor.base as *const _ as usize)
        } else if service == host.services.media_transport {
            (
                4,
                host.timed_media
                    .as_ref()
                    .map_or(&host.media.base as *const _ as usize, |table| {
                        &table.base.base as *const _ as usize
                    }),
            )
        } else if service == host.services.control_transport {
            (5, &host.control.base as *const _ as usize)
        } else {
            panic!("unexpected service {service}");
        };
        Response::NativeBound { binding, address }
    }
}

#[test]
fn encoded_source_binds_its_own_contract_and_bypasses_encoder() {
    let mut host = Host::new();
    host.use_encoded_capture = true;
    host.provider.submitted_pcm = true;
    let mut selected = handles();
    selected.audio_encoder = None;
    selected.encoded_audio_source = true;
    let mut backend = NativeBackend::new(
        &host.runtime(),
        selected,
        &crate::providers::ProviderServices::default(),
    )
    .unwrap();
    let packet = backend.audio().unwrap().unwrap();
    assert_eq!(packet.unit_header().format, AUDIO_FORMAT_OPUS);
    assert!(backend.drain_encoder().unwrap());
    drop(backend);
    assert!(
        host.typed_contracts
            .iter()
            .any(|(_, contract)| contract == amaterasu_module_sdk::audio::ENCODED_SOURCE_CONTRACT)
    );
    assert!(!host.bindings.contains(&host.services.audio_encoder));
}

#[test]
fn encoded_source_rejects_units_from_a_different_configuration_generation() {
    let mut host = Host::new();
    host.use_encoded_capture = true;
    host.provider.submitted_pcm = true;
    let mut selected = handles();
    selected.audio_encoder = None;
    selected.encoded_audio_source = true;
    selected.audio_generation = 2;
    let mut backend = NativeBackend::new(
        &host.runtime(),
        selected,
        &crate::providers::ProviderServices::default(),
    )
    .unwrap();
    assert!(matches!(backend.audio(), Err("audio_format_mismatch")));
}

#[test]
fn selected_processor_accepts_pcm_and_delivers_its_negotiated_planar_output() {
    let mut host = Host::new();
    host.provider.alternate_audio = true;
    host.services.audio_processor = Some("vendor.audio-processor/1".into());
    let providers = host.services.clone();
    let mut selected = handles();
    selected.audio_processor = Some(60);
    selected.processed_audio = Some((96_000, 2, AUDIO_PCM_I24, 3, AUDIO_PACKING_PLANAR));
    let mut backend = NativeBackend::new(&host.runtime(), selected, &providers).unwrap();
    let input = backend.pcm().unwrap().unwrap();
    assert!(backend.submit_processor(&input.header()).unwrap());
    let ProcessorOutput::Frame(output) = backend.processor_output().unwrap() else {
        panic!("missing processed frame")
    };
    let frame = output.header();
    assert_eq!(frame.packing, AUDIO_PACKING_PLANAR);
    assert_eq!(frame.format, AUDIO_PCM_I24);
    // SAFETY: the copied packet retains the output bytes for this borrow.
    assert_eq!(unsafe { frame.bytes().unwrap() }, [9u8; 24]);
    assert!(backend.drain_processor().unwrap());
    assert!(matches!(
        backend.processor_output().unwrap(),
        ProcessorOutput::End
    ));
    drop(backend);
    assert!(
        host.typed_contracts
            .iter()
            .any(|(_, contract)| contract == amaterasu_module_sdk::audio::PROCESSOR_CONTRACT)
    );
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

unsafe extern "C" fn timed_video(context: *mut c_void, _: u64, sink: TimedPacketSink) -> Status {
    // SAFETY: provider and packet are borrowed during this synchronous call.
    let provider = unsafe { &mut *context.cast::<Provider>() };
    provider.timed_video_calls += 1;
    let bytes = [0, 0, 1, 0x65, 9];
    let packet = TimedEncodedPacket {
        base: EncodedPacket {
            codec: CODEC_H264,
            flags: FLAG_END_OF_ACCESS_UNIT,
            data: bytes.as_ptr(),
            len: bytes.len(),
            timestamp_us: 7000,
            sequence: 11,
        },
        processing_started_host_us: 6500,
    };
    unsafe { sink.send(&packet) }
}

unsafe extern "C" fn timed_submit_video(
    context: *mut c_void,
    _: u64,
    packet: *const TimedEncodedPacket,
) -> Status {
    // SAFETY: the test provider and timed header outlive this call.
    let provider = unsafe { &mut *context.cast::<Provider>() };
    let packet = unsafe { &*packet };
    provider.timed_media_calls += 1;
    provider.timing_received = packet.processing_started_host_us;
    provider.presentation_received = packet.base.timestamp_us;
    if provider.timed_media_busy {
        MEDIA_BUSY
    } else {
        STATUS_OK
    }
}

unsafe extern "C" fn geometry_unavailable(
    _: *mut c_void,
    _: u64,
    _: *mut CaptureGeometry,
) -> Status {
    MEDIA_UNSUPPORTED
}
unsafe extern "C" fn hdr_unavailable(_: *mut c_void, _: u64, _: *mut HdrMetadata) -> Status {
    MEDIA_UNSUPPORTED
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
        format: amaterasu_media::AudioFormatId::standard(u64::from(PCM_I16)),
        sample_bytes: 2,
        packing: amaterasu_media::AUDIO_PACKING_INTERLEAVED,
        frames: 240,
        flags: 0,
        data: samples.as_ptr(),
        len: samples.len(),
        timestamp_us: 10,
        sequence: 1,
    };
    unsafe { sink.send(&frame) }
}

unsafe extern "C" fn processor_submit(
    context: *mut c_void,
    _: u64,
    frame: *const AudioFrame,
) -> Status {
    // SAFETY: the test owns the provider and borrows the frame for this call.
    let provider = unsafe { &mut *context.cast::<Provider>() };
    if provider.processor_pending {
        return MEDIA_BUSY;
    }
    assert!(unsafe { &*frame }.validate().is_ok());
    provider.processor_pending = true;
    STATUS_OK
}

unsafe extern "C" fn processor_receive(context: *mut c_void, _: u64, sink: AudioSink) -> Status {
    // SAFETY: test context is a live exclusively called provider.
    let provider = unsafe { &mut *context.cast::<Provider>() };
    if !provider.processor_pending {
        return if provider.processor_draining {
            MEDIA_CLOSED
        } else {
            MEDIA_PENDING
        };
    }
    let samples = [9u8; 24];
    let frame = AudioFrame {
        sample_rate: 96_000,
        channels: 2,
        format: AUDIO_PCM_I24,
        sample_bytes: 3,
        frames: 4,
        flags: 0,
        packing: AUDIO_PACKING_PLANAR,
        data: samples.as_ptr(),
        len: samples.len(),
        timestamp_us: 20,
        sequence: 2,
    };
    // SAFETY: sample storage remains live throughout synchronous delivery.
    let status = unsafe { sink.send(&frame) };
    if status == STATUS_OK {
        provider.processor_pending = false;
    }
    status
}

unsafe extern "C" fn processor_drain(context: *mut c_void, _: u64) -> Status {
    // SAFETY: test context is a live exclusively called provider.
    unsafe { &mut *context.cast::<Provider>() }.processor_draining = true;
    STATUS_OK
}
unsafe extern "C" fn audio(context: *mut c_void, _: u64, sink: AudioUnitSink) -> Status {
    // SAFETY: the fake provider and callback are live for this synchronous call.
    let provider = unsafe { &*context.cast::<Provider>() };
    if provider.audio_failed {
        return MEDIA_BACKEND_ERROR;
    }
    if !provider.submitted_pcm {
        return MEDIA_PENDING;
    }
    let bytes = [1u8, 2, 3];
    let packet = AudioUnit {
        generation: 1,
        format: AUDIO_FORMAT_OPUS,
        metadata_schema: AudioFormatId {
            namespace: 0,
            value: 0,
            version: 0,
        },
        flags: 0,
        duration_us: 5_000,
        payload: BorrowedBytes::new(&bytes),
        metadata: BorrowedBytes::new(&[]),
        timestamp_us: 10,
        sequence: 1,
    };
    unsafe { (sink.write)(sink.context, &packet) }
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
unsafe extern "C" fn submit_audio(_: *mut c_void, _: u64, _: *const AudioUnit) -> Status {
    MEDIA_BUSY
}
unsafe extern "C" fn receive_audio_unit(_: *mut c_void, _: u64, sink: AudioUnitSink) -> Status {
    let format = AudioFormatId {
        namespace: 42,
        value: u64::from(u32::MAX) + 1,
        version: 1,
    };
    let schema = AudioFormatId {
        namespace: 42,
        value: 3,
        version: 1,
    };
    // SAFETY: the host supplies a callback valid for this synchronous invocation.
    let mut output =
        unsafe { amaterasu_module_sdk::audio::AudioOutput::from_sink(sink, format, 1) }.unwrap();
    match output.send_packet(&[1, 2], 100, 10_000, 1, Some((schema, &[7, 8]))) {
        Ok(()) => STATUS_OK,
        Err(error) => error.status(),
    }
}
unsafe extern "C" fn submit_audio_custom(
    context: *mut c_void,
    _: u64,
    unit: *const AudioUnit,
) -> Status {
    // SAFETY: the test binding and borrowed unit remain live for this call.
    let provider = unsafe { &mut *context.cast::<Provider>() };
    if provider.audio_unit_busy {
        return MEDIA_BUSY;
    }
    let unit = unsafe { &*unit };
    provider.audio_units.push((
        unit.format,
        unsafe { unit.payload.read().unwrap() }.to_vec(),
        unsafe { unit.metadata.read().unwrap() }.to_vec(),
    ));
    STATUS_OK
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
        audio_processor: None,
        media_transport: 40,
        control_transport: 50,
        stream_audio: true,
        audio_channels: 2,
        audio_sample_rate: 48_000,
        audio_format: None,
        audio_generation: 1,
        audio_max_unit_bytes: AUDIO_EXTENSIBLE_MAX_PAYLOAD,
        audio_max_metadata_bytes: AUDIO_EXTENSIBLE_MAX_METADATA,
        audio_pcm: None,
        encoded_audio_source: false,
        processed_audio: None,
        video_codec: CODEC_H264,
    }
}

fn disabled_audio_handles() -> Handles {
    Handles {
        hdr: false,
        video: 10,
        audio_capture: None,
        audio_encoder: None,
        audio_processor: None,
        media_transport: 40,
        control_transport: 50,
        stream_audio: false,
        audio_channels: 2,
        audio_sample_rate: 48_000,
        audio_format: None,
        audio_generation: 1,
        audio_max_unit_bytes: AUDIO_EXTENSIBLE_MAX_PAYLOAD,
        audio_max_metadata_bytes: AUDIO_EXTENSIBLE_MAX_METADATA,
        audio_pcm: None,
        encoded_audio_source: false,
        processed_audio: None,
        video_codec: CODEC_H264,
    }
}

#[test]
fn replacement_tables_negotiate_timing_only_when_both_providers_support_it() {
    for (video_timed, media_timed) in [(false, false), (true, false), (false, true), (true, true)] {
        let mut host = Host::new();
        host.provider.timed_media_busy = true;
        let context = (&mut *host.provider as *mut Provider).cast();
        if video_timed {
            let mut base = prefix::<VideoApiV1_3>(context);
            base.header.minor = 3;
            host.timed_video = Some(Box::new(VideoApiV1_3 {
                base: VideoApiV1_2 {
                    base,
                    receive: video,
                    geometry: geometry_unavailable,
                    hdr_metadata: hdr_unavailable,
                },
                receive_timed: timed_video,
            }));
        }
        if media_timed {
            let mut base = prefix::<MediaTransportApiV1_1>(context);
            base.header.minor = 1;
            host.timed_media = Some(Box::new(MediaTransportApiV1_1 {
                base: MediaTransportApi {
                    base,
                    submit_video,
                    submit_audio,
                    drain,
                },
                submit_video_timed: timed_submit_video,
            }));
        }
        let mut backend = NativeBackend::new(
            &host.runtime(),
            disabled_audio_handles(),
            &crate::providers::ProviderServices::default(),
        )
        .unwrap();
        let mut assembly = VideoAssembly::new(CODEC_H264);
        assembly.push(backend.video().unwrap().unwrap()).unwrap();
        let header = assembly.header().unwrap();
        assert_eq!(header.timestamp_us, 7000);
        let start = assembly.processing_started_host_us();
        assert_eq!(start, if video_timed && media_timed { 6500 } else { 0 });
        assert!(!backend.submit_video(&header, start).unwrap());
        assert_eq!(
            assembly.processing_started_host_us(),
            start,
            "BUSY retains timing"
        );
        if video_timed && media_timed {
            assert_eq!(host.provider.timed_video_calls, 1);
            assert_eq!(host.provider.timed_media_calls, 1);
            assert_eq!(host.provider.timing_received, 6500);
            assert_eq!(host.provider.presentation_received, 7000);
            host.provider.timed_media_busy = false;
            assert!(backend.submit_video(&header, start).unwrap());
            assert_eq!(host.provider.timed_media_calls, 2);
            assert_eq!(host.provider.timing_received, 6500);
            assert_eq!(host.provider.presentation_received, 7000);
        } else {
            assert_eq!(host.provider.timed_video_calls, 0);
            assert_eq!(host.provider.timed_media_calls, 0);
        }
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
    assert!(!backend.submit_video(&header, 0).unwrap());
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
    let encoded_header = encoded.unit_header();
    assert_eq!(
        unsafe { encoded_header.payload.read().unwrap() },
        &[1, 2, 3]
    );
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
fn negotiated_audio_units_use_typed_providers_and_retain_metadata_on_busy() {
    let mut host = Host::new();
    host.encoder.receive = receive_audio_unit;
    host.media.submit_audio = submit_audio_custom;
    let mut requested = handles();
    let format = AudioFormatId {
        namespace: 42,
        value: u64::from(u32::MAX) + 1,
        version: 1,
    };
    requested.audio_format = Some(format);
    let services = host.services.clone();
    let mut backend = NativeBackend::new(&host.runtime(), requested, &services).unwrap();
    let packet = backend.audio().unwrap().unwrap();
    let unit = packet.unit_header();
    assert_eq!(unit.format, format);
    host.provider.audio_unit_busy = true;
    assert!(!backend.submit_audio(&unit).unwrap());
    host.provider.audio_unit_busy = false;
    assert!(backend.submit_audio(&unit).unwrap());
    assert_eq!(
        host.provider.audio_units,
        [(format, vec![1u8, 2], vec![7u8, 8])]
    );
    backend.handles.audio_format = Some(AudioFormatId {
        namespace: 42,
        value: 1,
        version: 1,
    });
    assert_eq!(backend.audio().err(), Some("audio_format_mismatch"));
    drop(backend);
    assert_eq!(host.released, [1, 2, 3, 4, 5]);
}
