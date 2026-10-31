use super::*;
use amaterasu_contracts::RuntimeConfig;
use amaterasu_module_sdk::{
    RuntimeClient,
    encode::{
        ControlRequest, ControlResponse, EncodedPacket, EncoderProvider, FormatId, OutputQueue,
        ProviderAdapter, SessionConfig, SessionMode,
    },
    media_abi::*,
};
use std::{
    ffi::c_void,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

const SERVICE: &str = "encoder/1";
const SESSION: u64 = 9;

struct Provider {
    queue: OutputQueue,
    releases: Arc<AtomicUsize>,
}
impl Provider {
    fn authorize(session: u64) -> Result<(), Status> {
        if session == SESSION {
            Ok(())
        } else {
            Err(MEDIA_CLOSED)
        }
    }
}
impl EncoderProvider for Provider {
    fn probe(&self, config: SessionConfig) -> Result<ControlResponse, Status> {
        assert_eq!(config.options, b"config");
        Ok(ControlResponse {
            kind: 1,
            request_id: 0,
            effective_generation: 1,
            effective_submission_id: 0,
            outcome: ENCODER_OUTCOME_APPLIED,
            schema: FormatId::default(),
            data: b"supported".to_vec(),
        })
    }
    fn configure(&self, session: u64, _: SessionConfig) -> Result<(), Status> {
        Self::authorize(session)
    }
    fn output(&self, session: u64) -> Result<OutputQueue, Status> {
        Self::authorize(session)?;
        Ok(self.queue.clone())
    }
    fn control(&self, session: u64, _: ControlRequest) -> Result<ControlResponse, Status> {
        Self::authorize(session)?;
        Err(MEDIA_UNSUPPORTED)
    }
    fn drain(&self, session: u64) -> Result<(), Status> {
        Self::authorize(session)?;
        self.queue.drain().map_err(|_| MEDIA_CLOSED)
    }
    fn destroy(&self, session: u64) {
        if Self::authorize(session).is_ok() {
            self.queue.unsubscribe_native();
            self.queue.cancel();
        }
    }
    fn release(&self) -> Result<(), Status> {
        self.queue.unsubscribe_native();
        self.queue.cancel();
        self.releases.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}
struct Factory {
    queue: OutputQueue,
    releases: Arc<AtomicUsize>,
    calls: AtomicUsize,
}
unsafe extern "C" fn bind(
    context: *mut c_void,
    caller: BorrowedBytes,
    _: *const NativeInterfaceQuery,
    output: *mut *const NativeInterface,
) -> Status {
    // SAFETY: runtime calls with a live factory, readable principal and writable output.
    let factory = unsafe { &*context.cast::<Factory>() };
    factory.calls.fetch_add(1, Ordering::SeqCst);
    if unsafe { caller.read() }.ok() != Some(b"consumer".as_slice()) {
        return STATUS_INVALID_ARGUMENT;
    }
    let table = ProviderAdapter::new(Provider {
        queue: factory.queue.clone(),
        releases: factory.releases.clone(),
    })
    .into_binding();
    unsafe { *output = table.cast() };
    STATUS_OK
}
unsafe extern "C" fn control_sink(
    context: *mut c_void,
    response: *const EncoderControlResponse,
) -> Status {
    // SAFETY: descriptors and destination live for the synchronous callback.
    unsafe { *context.cast::<Vec<u8>>() = (*response).data.read().unwrap().to_vec() };
    STATUS_OK
}
unsafe extern "C" fn output_sink(context: *mut c_void, chunk: *const EncoderOutputChunk) -> Status {
    // SAFETY: descriptors and destination live for the synchronous callback.
    unsafe { *context.cast::<Vec<u8>>() = (*chunk).bytes.read().unwrap().to_vec() };
    STATUS_OK
}
unsafe extern "C" fn event_sink(context: *mut c_void, _: u64, _: u32) {
    // SAFETY: the atomic lives until after synchronous unsubscribe.
    unsafe { &*context.cast::<AtomicUsize>() }.fetch_add(1, Ordering::SeqCst);
}
fn packet() -> EncodedPacket {
    EncodedPacket {
        flags: 0,
        session_id: SESSION,
        access_unit_id: 1,
        submission_id: 0,
        source_id: 1,
        config_generation: 1,
        timestamp_us: 1,
        codec: FormatId::default(),
        framing: FormatId::default(),
        bytes: vec![1, 2, 3],
    }
}

#[test]
fn sdk_encoder_binding_through_authenticated_endpoints() {
    let mut config = RuntimeConfig::default();
    config.principals.insert(
        "provider".into(),
        Grants {
            native_provide: [SERVICE.into()].into(),
            ..Default::default()
        },
    );
    for name in ["consumer", "outsider"] {
        config.principals.insert(
            name.into(),
            Grants {
                native_use: [SERVICE.into()].into(),
                ..Default::default()
            },
        );
    }
    let api = unsafe { &*crate::ffi::amaterasu_runtime_v1() };
    let mut runtime = std::ptr::null_mut();
    let json = serde_json::to_vec(&config).unwrap();
    // SAFETY: configuration and output pointers are live; runtime is destroyed after clients.
    assert_eq!(
        unsafe { (api.create)(BorrowedBytes::new(&json), &mut runtime) },
        STATUS_OK
    );
    let endpoint = |name: &str| {
        let mut host = std::mem::MaybeUninit::<HostApi>::uninit();
        // SAFETY: runtime is live and output points to sufficient storage.
        assert_eq!(
            unsafe {
                (api.endpoint)(
                    runtime,
                    BorrowedBytes::new(name.as_bytes()),
                    host.as_mut_ptr(),
                )
            },
            STATUS_OK
        );
        unsafe { host.assume_init() }
    };
    let provider = unsafe { RuntimeClient::from_endpoint(endpoint("provider")) };
    let consumer = unsafe { RuntimeClient::from_endpoint(endpoint("consumer")) };
    let outsider = unsafe { RuntimeClient::from_endpoint(endpoint("outsider")) };
    let queue = OutputQueue::new(SessionMode::Source, 4, 100).unwrap();
    let releases = Arc::new(AtomicUsize::new(0));
    let mut factory = Factory {
        queue: queue.clone(),
        releases: releases.clone(),
        calls: AtomicUsize::new(0),
    };
    let descriptor = NativeServiceFactory {
        header: AbiHeader::of::<NativeServiceFactory>(),
        context: (&mut factory as *mut Factory).cast(),
        bind: Some(bind),
    };
    // SAFETY: factory and runtime outlive every binding and endpoint call.
    unsafe { provider.register_native(SERVICE, &descriptor).unwrap() };
    assert!(
        unsafe {
            outsider.bind_native(
                SERVICE,
                ENCODER_PROVIDER_INTERFACE_MAJOR,
                ENCODER_PROVIDER_INTERFACE_MINOR,
                ENCODER_PROVIDER_API_V1_0_SIZE,
            )
        }
        .is_err()
    );
    let binding = unsafe {
        consumer
            .bind_native(
                SERVICE,
                ENCODER_PROVIDER_INTERFACE_MAJOR,
                ENCODER_PROVIDER_INTERFACE_MINOR,
                ENCODER_PROVIDER_API_V1_0_SIZE,
            )
            .unwrap()
    };
    assert_eq!(factory.calls.load(Ordering::SeqCst), 2);
    // SAFETY: exact table requested by service/version and binding remains live below.
    let table = unsafe { binding.table::<EncoderProviderApi>().unwrap() };
    let format = EncoderFormatId {
        namespace_id: 0,
        value: 0,
        schema: 0,
    };
    let config = EncoderSessionConfig {
        size: size_of::<EncoderSessionConfig>() as u32,
        mode: ENCODER_SESSION_SOURCE,
        codec: format,
        framing: format,
        source_id: 1,
        config_generation: 1,
        options_schema: format,
        options: BorrowedBytes::new(b"config"),
    };
    let mut reply = Vec::new();
    let control = EncoderControlSink {
        context: (&mut reply as *mut Vec<u8>).cast(),
        write: control_sink,
    };
    let mut received = Vec::<u8>::new();
    let sink = EncoderOutputSink {
        context: (&mut received as *mut Vec<u8>).cast(),
        write: output_sink,
    };
    let events = AtomicUsize::new(0);
    let event = EncoderEventSink {
        context: (&events as *const AtomicUsize).cast_mut().cast(),
        notify: event_sink,
    };
    // SAFETY: all table, config and sink storage stays live through synchronous calls;
    // the event context stays live until unsubscribe completes.
    unsafe {
        assert_eq!(
            (table.probe)(table.base.context, &config, control),
            STATUS_OK
        );
        assert_eq!(reply, b"supported");
        assert_eq!(
            (table.configure_session)(table.base.context, SESSION, &config),
            STATUS_OK
        );
        assert_eq!(
            (table.configure_session)(table.base.context, SESSION + 1, &config),
            MEDIA_CLOSED
        );
        assert_eq!(
            (table.receive)(table.base.context, SESSION + 1, 100, sink),
            MEDIA_CLOSED
        );
        assert_eq!(
            (table.subscribe.unwrap())(table.base.context, SESSION, event),
            STATUS_OK
        );
        queue.push(packet()).unwrap();
        assert_eq!(
            (table.receive)(table.base.context, SESSION, 100, sink),
            STATUS_OK
        );
        assert_eq!(received, [1, 2, 3]);
        (table.unsubscribe.unwrap())(table.base.context, SESSION);
        let observed = events.load(Ordering::SeqCst);
        assert!(observed > 0);
        queue.push(packet()).unwrap();
        assert_eq!((table.drain)(table.base.context, SESSION), STATUS_OK);
        assert_eq!(events.load(Ordering::SeqCst), observed);
    }
    drop(binding);
    assert_eq!(releases.load(Ordering::SeqCst), 1);
    // SAFETY: binding and all endpoint users have stopped.
    unsafe { (api.destroy)(runtime) };
}
