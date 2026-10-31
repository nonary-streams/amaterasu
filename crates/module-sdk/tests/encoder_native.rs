use amaterasu_module_sdk::{
    abi::{BorrowedBytes, STATUS_INTERNAL_ERROR, STATUS_OK},
    encode::{
        ControlPayload, ControlRequest, ControlResponse, EncodedPacket, EncoderProvider, FormatId,
        OutputQueue, ProviderAdapter, SessionConfig, SessionMode,
    },
    media_abi::*,
};
use std::{
    ffi::c_void,
    sync::{Mutex, mpsc},
    time::Duration,
};

fn packet() -> EncodedPacket {
    EncodedPacket {
        flags: 0,
        session_id: 9,
        access_unit_id: 2,
        submission_id: 0,
        source_id: 1,
        config_generation: 1,
        timestamp_us: 3,
        codec: FormatId::default(),
        framing: FormatId::default(),
        bytes: vec![1, 2, 3],
    }
}
struct Provider {
    queue: OutputQueue,
}
impl EncoderProvider for Provider {
    fn probe(&self, config: SessionConfig) -> Result<ControlResponse, i32> {
        assert_eq!(config.options, b"config");
        Ok(response(0))
    }
    fn configure(&self, session: u64, _: SessionConfig) -> Result<(), i32> {
        if session == 9 {
            Ok(())
        } else {
            Err(MEDIA_CLOSED)
        }
    }
    fn output(&self, session: u64) -> Result<OutputQueue, i32> {
        if session == 9 {
            Ok(self.queue.clone())
        } else {
            Err(MEDIA_CLOSED)
        }
    }
    fn notification_queue(&self, session: u64) -> Result<Option<OutputQueue>, i32> {
        Ok(if session == 9 {
            Some(self.queue.clone())
        } else {
            None
        })
    }
    fn control(&self, _: u64, request: ControlRequest) -> Result<ControlResponse, i32> {
        assert_eq!(request.payload, ControlPayload::Opaque(b"control".to_vec()));
        Ok(response(request.request_id))
    }
    fn drain(&self, _: u64) -> Result<(), i32> {
        self.queue.drain().unwrap();
        Ok(())
    }
    fn destroy(&self, session: u64) {
        if session == 9 {
            self.queue.cancel();
        }
    }
    fn release(&self) -> Result<(), i32> {
        self.queue.cancel();
        Ok(())
    }
}
fn response(id: u64) -> ControlResponse {
    ControlResponse {
        kind: 1,
        request_id: id,
        effective_generation: 1,
        effective_submission_id: 0,
        outcome: ENCODER_OUTCOME_APPLIED,
        schema: FormatId::default(),
        data: b"reply".to_vec(),
    }
}
unsafe extern "C" fn output_sink(ctx: *mut c_void, chunk: *const EncoderOutputChunk) -> i32 {
    // SAFETY: test passes a live vector and descriptor for the duration of this call.
    let output = unsafe { &mut *ctx.cast::<Vec<Vec<u8>>>() };
    let raw = unsafe { &*chunk };
    output.push(unsafe { raw.bytes.read() }.unwrap().to_vec());
    if output.len() == 1 {
        MEDIA_BUSY
    } else {
        STATUS_OK
    }
}
unsafe extern "C" fn control_sink(
    ctx: *mut c_void,
    response: *const EncoderControlResponse,
) -> i32 {
    // SAFETY: test owns these pointers through the synchronous callback.
    let result = unsafe { &mut *ctx.cast::<Vec<u8>>() };
    *result = unsafe { (*response).data.read() }.unwrap().to_vec();
    STATUS_OK
}
#[test]
fn table_copies_inputs_and_retains_rejected_output() {
    let queue = OutputQueue::new(SessionMode::Source, 2, 100).unwrap();
    queue.push(packet()).unwrap();
    let adapter = ProviderAdapter::new(Provider {
        queue: queue.clone(),
    });
    // SAFETY: adapter and table remain live through all calls.
    let pointer = adapter.into_binding();
    // SAFETY: binding remains live until its explicit release below.
    let api = unsafe { &*pointer };
    let id = EncoderFormatId {
        namespace_id: 0,
        value: 0,
        schema: 0,
    };
    let config = EncoderSessionConfig {
        size: std::mem::size_of::<EncoderSessionConfig>() as u32,
        mode: ENCODER_SESSION_SOURCE,
        codec: id,
        framing: id,
        source_id: 1,
        config_generation: 1,
        options_schema: id,
        options: BorrowedBytes::new(b"config"),
    };
    let mut reply = Vec::<u8>::new();
    let reply_sink = EncoderControlSink {
        context: (&mut reply as *mut Vec<u8>).cast(),
        write: control_sink,
    };
    // SAFETY: every descriptor and sink is live for each synchronous call.
    unsafe {
        assert_eq!(
            (api.probe)(api.base.context, &config, reply_sink),
            STATUS_OK
        );
        assert_eq!(reply, b"reply");
        assert_eq!(
            (api.configure_session)(api.base.context, 9, &config),
            STATUS_OK
        );
        assert_eq!(
            (api.configure_session)(api.base.context, 10, &config),
            MEDIA_CLOSED
        );
        let request = EncoderControlRequest {
            size: std::mem::size_of::<EncoderControlRequest>() as u32,
            kind: 1,
            request_id: 5,
            expected_generation: 1,
            schema: id,
            data: BorrowedBytes::new(b"control"),
        };
        assert_eq!(
            (api.control)(api.base.context, 9, &request, reply_sink),
            STATUS_OK
        );
        let mut seen = Vec::<Vec<u8>>::new();
        let sink = EncoderOutputSink {
            context: (&mut seen as *mut Vec<Vec<u8>>).cast(),
            write: output_sink,
        };
        assert_eq!((api.receive)(api.base.context, 9, 2, sink), MEDIA_BUSY);
        assert!(seen.is_empty());
        assert_eq!((api.receive)(api.base.context, 9, 3, sink), MEDIA_BUSY);
        assert_eq!((api.receive)(api.base.context, 9, 3, sink), STATUS_OK);
        assert_eq!(seen, [vec![1, 2, 3], vec![1, 2, 3]]);
        assert_eq!((api.receive)(api.base.context, 9, 3, sink), MEDIA_PENDING);
        assert_eq!((api.drain)(api.base.context, 9), STATUS_OK);
        (api.unsubscribe.unwrap())(api.base.context, 10);
        (api.destroy_session)(api.base.context, 10);
        assert!(!queue.snapshot().cancelled);
        (api.destroy_session)(api.base.context, 9);
        (api.base.release.unwrap())(api.base.context);
    }
}
struct Gate {
    entered: mpsc::Sender<()>,
    release: Mutex<mpsc::Receiver<()>>,
}
unsafe extern "C" fn notify(ctx: *mut c_void, _: u64, _: u32) {
    // SAFETY: the gate lives through the synchronous unsubscribe barrier.
    let gate = unsafe { &*ctx.cast::<Gate>() };
    gate.entered.send(()).unwrap();
    gate.release.lock().unwrap().recv().unwrap();
}
#[test]
fn unsubscribe_waits_for_notification_and_prevents_later_callbacks() {
    let queue = OutputQueue::new(SessionMode::Source, 2, 100).unwrap();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let gate = Box::new(Gate {
        entered: entered_tx,
        release: Mutex::new(release_rx),
    });
    let sink = EncoderEventSink {
        context: (&*gate as *const Gate).cast_mut().cast(),
        notify,
    };
    // SAFETY: gate outlives unsubscribe, callback is non-reentrant and thread-safe.
    assert_eq!(unsafe { queue.subscribe_native(9, sink) }, STATUS_OK);
    let worker_queue = queue.clone();
    let worker = std::thread::spawn(move || worker_queue.push(packet()).unwrap());
    entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let other = queue.clone();
    let (done_tx, done_rx) = mpsc::channel();
    let unsubscribe = std::thread::spawn(move || {
        other.unsubscribe_native();
        done_tx.send(()).unwrap();
    });
    assert!(done_rx.recv_timeout(Duration::from_millis(50)).is_err());
    release_tx.send(()).unwrap();
    worker.join().unwrap();
    done_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    unsubscribe.join().unwrap();
    queue.cancel();
    assert!(entered_rx.try_recv().is_err());
}
#[test]
fn provider_panics_do_not_cross_native_boundary() {
    struct Panics;
    impl EncoderProvider for Panics {
        fn probe(&self, _: SessionConfig) -> Result<ControlResponse, i32> {
            panic!("probe")
        }
        fn configure(&self, _: u64, _: SessionConfig) -> Result<(), i32> {
            panic!("configure")
        }
        fn output(&self, _: u64) -> Result<OutputQueue, i32> {
            panic!("output")
        }
        fn control(&self, _: u64, _: ControlRequest) -> Result<ControlResponse, i32> {
            panic!("control")
        }
        fn drain(&self, _: u64) -> Result<(), i32> {
            panic!("drain")
        }
        fn destroy(&self, _: u64) {
            panic!("destroy")
        }
        fn release(&self) -> Result<(), i32> {
            Ok(())
        }
    }
    let adapter = ProviderAdapter::new(Panics);
    // SAFETY: adapter remains live through the call.
    let pointer = adapter.into_binding();
    // SAFETY: binding remains live until its explicit release below.
    let api = unsafe { &*pointer };
    assert_eq!(
        unsafe { (api.drain)(api.base.context, 1) },
        STATUS_INTERNAL_ERROR
    );
    unsafe { (api.base.release.unwrap())(api.base.context) };
}

#[test]
fn frame_options_copy_nested_bytes_and_compare_by_value() {
    let shared = std::sync::Arc::new(Mutex::new(Vec::<ControlRequest>::new()));
    struct SharedCapture(std::sync::Arc<Mutex<Vec<ControlRequest>>>);
    impl EncoderProvider for SharedCapture {
        fn probe(&self, _: SessionConfig) -> Result<ControlResponse, i32> {
            unreachable!()
        }
        fn configure(&self, _: u64, _: SessionConfig) -> Result<(), i32> {
            unreachable!()
        }
        fn output(&self, _: u64) -> Result<OutputQueue, i32> {
            unreachable!()
        }
        fn control(&self, _: u64, request: ControlRequest) -> Result<ControlResponse, i32> {
            self.0.lock().unwrap().push(request);
            Ok(response(4))
        }
        fn drain(&self, _: u64) -> Result<(), i32> {
            unreachable!()
        }
        fn destroy(&self, _: u64) {
            unreachable!()
        }
        fn release(&self) -> Result<(), i32> {
            Ok(())
        }
    }
    let adapter = ProviderAdapter::new(SharedCapture(shared.clone()));
    let pointer = adapter.into_binding();
    // SAFETY: binding remains live until its explicit release below.
    let api = unsafe { &*pointer };
    let id = EncoderFormatId {
        namespace_id: ENCODER_FORMAT_NAMESPACE_STANDARD,
        value: ENCODER_SCHEMA_FRAME_OPTIONS,
        schema: ENCODER_FORMAT_SCHEMA_V1,
    };
    let mut reply = Vec::<u8>::new();
    let sink = EncoderControlSink {
        context: (&mut reply as *mut Vec<u8>).cast(),
        write: control_sink,
    };
    let mut original = vec![7, 8, 9];
    let other = vec![7, 8, 9];
    let mut options = EncoderFrameOptionsV1 {
        version: 1,
        size: std::mem::size_of::<EncoderFrameOptionsV1>() as u32,
        submission_id: 10,
        source_id: 2,
        source_generation: 3,
        timestamp_us: 4,
        clock: FrameClockMetadata::default(),
        side_data_schema: id,
        side_data: BorrowedBytes::new(&original),
    };
    let call = |options: &EncoderFrameOptionsV1, extension: &[u8]| {
        let prefix = std::mem::size_of::<EncoderFrameOptionsV1>();
        let mut data = vec![0_u8; prefix + extension.len()];
        let mut descriptor = *options;
        descriptor.size = data.len() as u32;
        // SAFETY: allocation has space for the v1 descriptor; unaligned writes
        // are permitted and its nested bytes remain live during the call.
        unsafe {
            data.as_mut_ptr()
                .cast::<EncoderFrameOptionsV1>()
                .write_unaligned(descriptor)
        };
        data[prefix..].copy_from_slice(extension);
        let request = EncoderControlRequest {
            size: std::mem::size_of::<EncoderControlRequest>() as u32,
            kind: ENCODER_CONTROL_FRAME_OPTIONS,
            request_id: 4,
            expected_generation: 1,
            schema: id,
            data: BorrowedBytes::new(&data),
        };
        unsafe { (api.control)(api.base.context, 9, &request, sink) }
    };
    assert_eq!(call(&options, b"ext"), STATUS_OK);
    original.fill(0);
    options.side_data = BorrowedBytes::new(&other);
    assert_eq!(call(&options, b"ext"), STATUS_OK);
    assert_eq!(call(&options, b"other"), STATUS_OK);
    drop(original);
    drop(other);
    let captured = shared.lock().unwrap();
    assert_eq!(captured[0], captured[1]);
    assert_ne!(captured[0], captured[2]);
    match &captured[0].payload {
        ControlPayload::FrameOptions(frame) => {
            assert_eq!(frame.side_data, [7, 8, 9]);
            assert_eq!(frame.extension, b"ext");
        }
        _ => panic!("expected frame options"),
    }
    drop(captured);
    options.version = 2;
    assert_eq!(
        call(&options, b""),
        amaterasu_module_sdk::abi::STATUS_INVALID_ARGUMENT
    );
    unsafe { (api.base.release.unwrap())(api.base.context) };
}

#[test]
fn late_subscriber_observes_stopped_after_eos_is_consumed() {
    unsafe extern "C" fn record(ctx: *mut c_void, _: u64, event: u32) {
        // SAFETY: this test retains the vector until unsubscribe.
        unsafe { &mut *ctx.cast::<Vec<u32>>() }.push(event);
    }
    let queue = OutputQueue::new(SessionMode::Source, 1, 100).unwrap();
    queue.drain().unwrap();
    let mut eos = packet();
    eos.flags = ENCODER_CHUNK_END_OF_STREAM;
    eos.bytes.clear();
    queue.finish(eos).unwrap();
    queue.acknowledge(&queue.front().unwrap()).unwrap();
    let mut events = Vec::new();
    let sink = EncoderEventSink {
        context: (&mut events as *mut Vec<u32>).cast(),
        notify: record,
    };
    // SAFETY: callback context stays alive and never reenters the queue.
    assert_eq!(unsafe { queue.subscribe_native(9, sink) }, STATUS_OK);
    queue.unsubscribe_native();
    assert_eq!(events, [ENCODER_EVENT_STOPPED]);
}

#[cfg(unix)]
#[test]
fn void_barrier_failures_abort_the_worker() {
    use std::{os::unix::process::ExitStatusExt, process::Command};
    struct Failing {
        mode: String,
        queue: OutputQueue,
    }
    impl EncoderProvider for Failing {
        fn probe(&self, _: SessionConfig) -> Result<ControlResponse, i32> {
            unreachable!()
        }
        fn configure(&self, _: u64, _: SessionConfig) -> Result<(), i32> {
            unreachable!()
        }
        fn output(&self, _: u64) -> Result<OutputQueue, i32> {
            if self.mode == "unsubscribe_panic" {
                panic!("unsubscribe failure");
            }
            if self.mode == "unsubscribe_error" {
                return Err(MEDIA_CLOSED);
            }
            Ok(self.queue.clone())
        }
        fn control(&self, _: u64, _: ControlRequest) -> Result<ControlResponse, i32> {
            unreachable!()
        }
        fn drain(&self, _: u64) -> Result<(), i32> {
            unreachable!()
        }
        fn destroy(&self, _: u64) {
            if self.mode == "destroy_panic" {
                panic!("destroy failure");
            }
        }
        fn release(&self) -> Result<(), i32> {
            if self.mode == "release_panic" {
                panic!("release failure");
            }
            if self.mode == "release_error" {
                return Err(MEDIA_BUSY);
            }
            Ok(())
        }
    }
    const ENV: &str = "AMATERASU_ENCODER_ABORT_TEST";
    if let Ok(mode) = std::env::var(ENV) {
        let adapter = ProviderAdapter::new(Failing {
            mode: mode.clone(),
            queue: OutputQueue::new(SessionMode::Source, 1, 16).unwrap(),
        });
        // SAFETY: runtime-style binding stays live until the tested callback.
        let api = unsafe { &*adapter.into_binding() };
        unsafe {
            if mode.starts_with("unsubscribe") {
                (api.unsubscribe.unwrap())(api.base.context, 1);
            } else if mode == "destroy_panic" {
                (api.destroy_session)(api.base.context, 1);
            } else {
                (api.base.release.unwrap())(api.base.context);
            }
        }
        panic!("failed lifetime barrier returned");
    }
    for mode in [
        "unsubscribe_panic",
        "unsubscribe_error",
        "destroy_panic",
        "release_panic",
        "release_error",
    ] {
        let status = Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg("void_barrier_failures_abort_the_worker")
            .env(ENV, mode)
            .current_dir("/tmp")
            .output()
            .unwrap()
            .status;
        assert_eq!(
            status.signal(),
            Some(6),
            "{mode} must abort the worker: {status:?}"
        );
    }
}
