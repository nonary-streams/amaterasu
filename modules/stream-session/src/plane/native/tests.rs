use super::*;
use amaterasu_module_sdk::{Command, Response};

#[derive(Default)]
struct Provider {
    duplicate: bool,
    invalid_status: bool,
    audio_failed: bool,
    control_failed: bool,
    pcm_failed: bool,
}

struct Host {
    provider: Box<Provider>,
    video: VideoApi,
    capture: AudioCaptureApi,
    encoder: AudioEncoderApi,
    media: MediaTransportApi,
    control: ControlTransportApi,
    released: Vec<u64>,
    bindings: Vec<String>,
    reject_capture: bool,
}

impl Host {
    fn new() -> Self {
        let mut provider = Box::<Provider>::default();
        let context = (&mut *provider as *mut Provider).cast();
        Self {
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
        Command::NativeBind { service, .. } => {
            host.bindings.push(service.clone());
            if host.reject_capture && service == "amaterasu.audio/1" {
                Response::Error {
                    code: "denied".into(),
                    message: "denied".into(),
                }
            } else {
                let (binding, address) = match service.as_str() {
                    "amaterasu.video/1" => (1, &host.video.base as *const _ as usize),
                    "amaterasu.audio/1" => (2, &host.capture.base as *const _ as usize),
                    "amaterasu.audio-encode/1" => (3, &host.encoder.base as *const _ as usize),
                    "amaterasu.media-transport/1" => (4, &host.media.base as *const _ as usize),
                    "amaterasu.control-transport/1" => (5, &host.control.base as *const _ as usize),
                    _ => panic!("unexpected service"),
                };
                Response::NativeBound { binding, address }
            }
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

unsafe extern "C" fn pcm(context: *mut c_void, _: u64, _: AudioSink) -> Status {
    if unsafe { &*context.cast::<Provider>() }.pcm_failed {
        MEDIA_BACKEND_ERROR
    } else {
        MEDIA_PENDING
    }
}
unsafe extern "C" fn audio(context: *mut c_void, _: u64, _: AudioPacketSink) -> Status {
    // SAFETY: the test keeps the boxed provider live for the binding lifetime.
    if unsafe { &*context.cast::<Provider>() }.audio_failed {
        MEDIA_BACKEND_ERROR
    } else {
        MEDIA_PENDING
    }
}
unsafe extern "C" fn control(context: *mut c_void, _: u64, _: ControlMessageSink) -> Status {
    if unsafe { &*context.cast::<Provider>() }.control_failed {
        MEDIA_BACKEND_ERROR
    } else {
        MEDIA_PENDING
    }
}
unsafe extern "C" fn submit_pcm(_: *mut c_void, _: u64, _: *const AudioFrame) -> Status {
    MEDIA_BUSY
}
unsafe extern "C" fn submit_video(_: *mut c_void, _: u64, _: *const EncodedPacket) -> Status {
    MEDIA_BUSY
}
unsafe extern "C" fn submit_audio(_: *mut c_void, _: u64, _: *const EncodedAudioPacket) -> Status {
    MEDIA_BUSY
}
unsafe extern "C" fn send_control(_: *mut c_void, _: u64, _: *const ControlMessage) -> Status {
    MEDIA_BUSY
}
unsafe extern "C" fn drain(_: *mut c_void, _: u64) -> Status {
    STATUS_OK
}

fn handles() -> Handles {
    Handles {
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
    let mut backend = NativeBackend::new(&host.runtime(), handles()).unwrap();
    let packet = backend.video().unwrap().unwrap();
    let mut assembly = VideoAssembly::new(CODEC_H264);
    assembly.push(packet).unwrap();
    let header = assembly.header().unwrap();
    // SAFETY: assembly owns the copied bytes despite provider overwriting its source.
    assert_eq!(unsafe { header.bytes().unwrap() }, [0, 0, 1, 0x65, 9]);
    assert!(!backend.submit_video(&header).unwrap());
    drop(backend);
    assert_eq!(host.released, [1, 2, 3, 4, 5]);
}

#[test]
fn partial_native_bind_failure_releases_previously_acquired_table() {
    let mut host = Host::new();
    host.reject_capture = true;
    assert_eq!(
        NativeBackend::new(&host.runtime(), handles()).err(),
        Some("native_binding_failed")
    );
    assert_eq!(host.released, [1]);
}

#[test]
fn disabled_audio_binds_no_capture_or_encoder_tables() {
    let mut host = Host::new();
    host.reject_capture = true;
    let backend = NativeBackend::new(&host.runtime(), disabled_audio_handles()).unwrap();
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
    let mut backend = NativeBackend::new(&host.runtime(), handles()).unwrap();
    assert_eq!(backend.video().err(), Some("native_multiple_callbacks"));
    host.provider.duplicate = false;
    host.provider.invalid_status = true;
    assert_eq!(backend.video().err(), Some("native_delivery_failed_video"));
    drop(backend);
}

#[test]
fn native_delivery_failures_identify_the_control_and_pcm_lane() {
    let mut host = Host::new();
    let mut backend = NativeBackend::new(&host.runtime(), handles()).unwrap();
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
    let mut backend = NativeBackend::new(&host.runtime(), handles()).unwrap();
    assert_eq!(backend.audio().err(), Some("audio_encoder_failed"));
    assert_eq!(
        encoder_accepted(MEDIA_BACKEND_ERROR),
        Err("audio_encoder_failed")
    );
    assert_eq!(
        accepted(MEDIA_BACKEND_ERROR),
        Err("native_operation_failed")
    );
}
