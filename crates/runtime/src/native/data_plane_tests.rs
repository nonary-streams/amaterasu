use amaterasu_abi::*;
use amaterasu_contracts::{Grants, RuntimeConfig};
use amaterasu_module_sdk::{
    RuntimeClient,
    data_plane::{abi::*, *},
};

const SERVICE: &str = "example.binary/1";

struct Runtime {
    api: &'static RuntimeApi,
    context: *mut std::ffi::c_void,
}
impl Runtime {
    fn new() -> Self {
        let mut config = RuntimeConfig::default();
        config.principals.insert(
            "provider".into(),
            Grants {
                provide: [SERVICE.into()].into(),
                native_provide: [SERVICE.into()].into(),
                ..Default::default()
            },
        );
        for owner in ["consumer", "outsider"] {
            config.principals.insert(
                owner.into(),
                Grants {
                    call: [SERVICE.into()].into(),
                    native_use: [SERVICE.into()].into(),
                    ..Default::default()
                },
            );
        }
        config
            .principals
            .insert("ungranted".into(), Grants::default());
        // SAFETY: exported immutable runtime table has static lifetime.
        let api = unsafe { &*crate::ffi::amaterasu_runtime_v1() };
        let mut context = std::ptr::null_mut();
        let json = serde_json::to_vec(&config).unwrap();
        // SAFETY: JSON/output live for call; Drop runs after clients/factories.
        assert_eq!(
            unsafe { (api.create)(BorrowedBytes::new(&json), &mut context) },
            STATUS_OK
        );
        Self { api, context }
    }
    fn client(&self, name: &str) -> RuntimeClient {
        // SAFETY: tests retain runtime past every client.
        unsafe { RuntimeClient::from_endpoint(self.endpoint(name)) }
    }
    fn endpoint(&self, name: &str) -> HostApi {
        let mut endpoint = std::mem::MaybeUninit::uninit();
        // SAFETY: runtime live, writable output; tests retain runtime past client.
        assert_eq!(
            unsafe {
                (self.api.endpoint)(
                    self.context,
                    BorrowedBytes::new(name.as_bytes()),
                    endpoint.as_mut_ptr(),
                )
            },
            STATUS_OK
        );
        unsafe { endpoint.assume_init() }
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        // SAFETY: tests destroy all clients/bindings/factories first.
        unsafe { (self.api.destroy)(self.context) };
    }
}
fn config(direction: u32, bytes: u64) -> Description {
    let unknown_format = Schema {
        namespace: 0xdeadbeef,
        value: 499,
        version: 3,
    };
    description(
        direction,
        unknown_format,
        Schema {
            namespace: 78,
            value: 9,
            version: 1,
        },
        Limits {
            max_chunk_bytes: bytes,
            max_queued_bytes: bytes,
            max_record_bytes: u64::MAX,
            max_queued_chunks: 4,
            max_segments: 8,
            max_metadata_bytes: 4096,
            reserved: 0,
        },
    )
    .unwrap()
}

unsafe extern "C" fn count_unexpected_video_bind(
    context: *mut std::ffi::c_void,
    _: BorrowedBytes,
    _: *const NativeInterfaceQuery,
    _: *mut *const NativeInterface,
) -> Status {
    // SAFETY: test keeps the counter alive for the runtime and factory lifetime.
    unsafe {
        (*(context.cast::<std::sync::atomic::AtomicUsize>()))
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    };
    STATUS_INVALID_ARGUMENT
}

#[test]
fn video_rejects_wrong_family_before_factory_callback() {
    let runtime = Runtime::new();
    let provider = runtime.client("provider");
    let consumer = runtime.client("consumer");
    let calls = std::sync::atomic::AtomicUsize::new(0);
    let factory = NativeServiceFactory {
        header: AbiHeader::of::<NativeServiceFactory>(),
        context: (&calls as *const std::sync::atomic::AtomicUsize)
            .cast_mut()
            .cast(),
        bind: Some(count_unexpected_video_bind),
    };
    // SAFETY: the descriptor and counter live through the attempted binding.
    unsafe {
        provider
            .register_native_typed(SERVICE, "example.unrelated/1", &factory)
            .unwrap()
    };
    assert!(unsafe { amaterasu_module_sdk::video::bind(&consumer, SERVICE) }.is_err());
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[test]
fn transport_adapters_reject_a_different_interface_family() {
    let runtime = Runtime::new();
    let provider = runtime.client("provider");
    let consumer = runtime.client("consumer");
    let hub = Hub::new(4, 1024).unwrap();
    let factory = hub.factory();
    // Same service and version, but a different promised family.
    unsafe { register_transfer(&provider, SERVICE, &factory.api).unwrap() };
    assert!(unsafe { amaterasu_module_sdk::transport::bind_media(&consumer, SERVICE) }.is_err());
    assert!(unsafe { amaterasu_module_sdk::transport::bind_control(&consumer, SERVICE) }.is_err());
}

#[test]
fn binary_transfer_rejects_an_unrelated_interface_family() {
    let runtime = Runtime::new();
    let provider = runtime.client("provider");
    let consumer = runtime.client("consumer");
    let hub = Hub::new(4, 1024).unwrap();
    let factory = hub.factory();
    // A matching table layout and version do not establish interface identity.
    unsafe {
        provider
            .register_native_typed(SERVICE, "example.unrelated/1", &factory.api)
            .unwrap()
    };
    let expected = config(SOURCE, 512);
    let (id, _) = hub.create("consumer", expected).unwrap();
    assert!(unsafe { TransferClient::bind(&consumer, SERVICE, id, expected) }.is_err());
}

#[test]
fn binary_channel_through_real_runtime_enforces_identity_negotiation_and_retirement() {
    let runtime = Runtime::new();
    let provider = runtime.client("provider");
    let consumer = runtime.client("consumer");
    let outsider = runtime.client("outsider");
    let ungranted = runtime.client("ungranted");
    let hub = Hub::new(4, 128 * 1024 * 1024).unwrap();
    let factory = hub.factory();
    // SAFETY: factory/runtime stay live until all bindings are dropped below.
    unsafe {
        amaterasu_module_sdk::data_plane::register_transfer(&provider, SERVICE, &factory.api)
            .unwrap()
    };
    let expected = config(SOURCE, 40 * 1024 * 1024);
    let (id, channel) = hub.create("consumer", expected).unwrap();
    assert!(unsafe { TransferClient::bind(&ungranted, SERVICE, id, expected) }.is_err());
    assert!(unsafe { TransferClient::bind(&outsider, SERVICE, id, expected) }.is_err());
    assert!(unsafe { consumer.bind_native(SERVICE, 99, 0, API_SIZE) }.is_err());
    assert!(unsafe { consumer.bind_native(SERVICE, 1, 99, API_SIZE) }.is_err());
    assert!(unsafe { consumer.bind_native(SERVICE, 1, 0, API_SIZE + 8) }.is_err());
    let client = unsafe { TransferClient::bind(&consumer, SERVICE, id, expected).unwrap() };
    // Even another binding belonging to the same principal cannot steal a channel.
    assert!(unsafe { TransferClient::bind(&consumer, SERVICE, id, expected) }.is_err());
    let bytes = vec![0xa5; 32 * 1024 * 1024 + 1];
    let pointer = bytes.as_ptr();
    channel.push(OwnedChunk::with_bytes(1, bytes)).unwrap();
    assert_eq!(client.submit(&OwnedChunk::with_bytes(2, vec![1])), DENIED);
    let before = client.snapshot().unwrap();
    assert_eq!(
        client.receive(|view| {
            assert_eq!(view.segments[0].as_ptr(), pointer);
            assert!(view.segments[0].iter().all(|b| *b == 0xa5));
            BUSY
        }),
        BUSY
    );
    assert_eq!(client.snapshot().unwrap().queued_bytes, before.queued_bytes);
    assert_eq!(client.receive(|_| STATUS_OK), STATUS_OK);
    channel.finish().unwrap();
    assert_eq!(client.snapshot().unwrap().phase, ENDED);
    drop(client);
    assert_eq!(channel.snapshot().phase, CANCELLED);
    assert!(unsafe { TransferClient::bind(&consumer, SERVICE, id, expected) }.is_err());
    // Budget returned by binding release permits a new channel.
    let (id, channel) = hub.create("consumer", expected).unwrap();
    let mismatched = Description {
        format: Schema {
            version: 4,
            ..expected.format
        },
        ..expected
    };
    assert!(unsafe { TransferClient::bind(&consumer, SERVICE, id, mismatched) }.is_err());
    assert_eq!(channel.snapshot().phase, CANCELLED);
}

#[test]
fn native_sink_acceptance_is_local_atomic_and_source_shutdown_cancels_queued_input() {
    let runtime = Runtime::new();
    let provider = runtime.client("provider");
    let consumer = runtime.client("consumer");
    let hub = Hub::new(2, 8192).unwrap();
    let factory = hub.factory();
    unsafe {
        amaterasu_module_sdk::data_plane::register_transfer(&provider, SERVICE, &factory.api)
            .unwrap()
    };
    let expected = config(SINK, 4096);
    let (id, channel) = hub.create("consumer", expected).unwrap();
    let client = unsafe { TransferClient::bind(&consumer, SERVICE, id, expected).unwrap() };
    assert_eq!(client.receive(|_| STATUS_OK), DENIED);
    assert_eq!(
        client.submit(&OwnedChunk::with_bytes(1, vec![5; 4096])),
        STATUS_OK
    );
    assert_eq!(
        client.submit(&OwnedChunk::with_bytes(2, vec![5; 4096])),
        BUSY
    );
    assert_eq!(client.finish(), STATUS_OK);
    assert_eq!(
        client.submit(&OwnedChunk::with_bytes(2, vec![5; 4096])),
        CLOSED
    );
    // SAFETY: sink inspects only the callback-lifetime borrowed chunk.
    assert_eq!(
        unsafe {
            channel.receive(|chunk| {
                let segment = &*chunk.segments;
                assert_eq!(segment.len, 4096);
                assert_eq!(*segment.data, 5);
                STATUS_OK
            })
        },
        STATUS_OK
    );
    assert_eq!(client.snapshot().unwrap().phase, ENDED);
    assert_eq!(client.cancel(), STATUS_OK);
    assert_eq!(channel.snapshot().queued_bytes, 0);
    drop(client);
}

#[test]
#[ignore = "build amaterasu-data-plane-fixture and set AMATERASU_DATA_PLANE_FIXTURE to its cdylib path"]
fn independently_built_binary_provider_transfers_large_assets_and_retained_frames() {
    use amaterasu_module_sdk::{Command, Response};
    use serde_json::{Value, json};
    let runtime = Runtime::new();
    let consumer = runtime.client("consumer");
    let outsider = runtime.client("outsider");
    let path = std::env::var_os("AMATERASU_DATA_PLANE_FIXTURE").expect("fixture library path");
    // SAFETY: explicitly supplied trusted test plugin; library is retained until
    // all bindings, fixture workers, contexts and callbacks have been retired.
    let library = unsafe { libloading::Library::new(path).unwrap() };
    let entry: libloading::Symbol<ModuleEntry> =
        unsafe { library.get(b"amaterasu_module_v1").unwrap() };
    let api = unsafe { &*entry() };
    let mut module = std::ptr::null_mut();
    assert_eq!(
        unsafe { (api.create)(&runtime.endpoint("provider"), &mut module) },
        STATUS_OK
    );
    let mut operation = 0;
    let mut control = |payload: Value| {
        operation += 1;
        let Response::Accepted { id } = consumer
            .send(Command::Request {
                service: SERVICE.into(),
                payload,
                timeout_ms: 30_000,
                operation_id: format!("binary.fixture.{operation}"),
                parent: None,
            })
            .unwrap()
        else {
            panic!("request rejected")
        };
        assert_eq!(unsafe { (api.tick)(module) }, STATUS_OK);
        let Response::Result { payload } =
            consumer.send(Command::TakeResult { request: id }).unwrap()
        else {
            panic!("missing reply")
        };
        consumer
            .require_success(Command::ReleaseResult { request: id })
            .unwrap();
        assert_eq!(payload["ok"], true, "{payload}");
        payload
    };
    for bytes in [985_696usize, 32 * 1024 * 1024 + 17] {
        let reply = control(json!({"action":"open_source","bytes":bytes}));
        let id: ChannelId = serde_json::from_value(reply["channel"].clone()).unwrap();
        let description: Description =
            serde_json::from_value(reply["description"].clone()).unwrap();
        assert!(unsafe { TransferClient::bind(&outsider, SERVICE, id, description) }.is_err());
        let client = unsafe { TransferClient::bind(&consumer, SERVICE, id, description).unwrap() };
        let mut calls = 0;
        let started = std::time::Instant::now();
        assert_eq!(
            client.receive(|chunk| {
                calls += 1;
                assert_eq!(chunk.total_bytes, bytes as u64);
                assert_eq!(
                    chunk
                        .segments
                        .iter()
                        .map(|segment| segment.len())
                        .sum::<usize>(),
                    bytes
                );
                assert!(
                    chunk
                        .segments
                        .iter()
                        .all(|segment| segment.iter().all(|byte| *byte == 0xa5))
                );
                STATUS_OK
            }),
            STATUS_OK
        );
        println!(
            "independent source: {bytes} bytes, {calls} native callback, validation included: {:?}",
            started.elapsed()
        );
        assert_eq!(calls, 1);
        assert_eq!(client.snapshot().unwrap().phase, ENDED);
        drop(client);
    }
    let reply = control(json!({"action":"open_sink"}));
    let id = serde_json::from_value(reply["channel"].clone()).unwrap();
    let description = serde_json::from_value(reply["description"].clone()).unwrap();
    let client = unsafe { TransferClient::bind(&consumer, SERVICE, id, description).unwrap() };
    for record in 1..=3 {
        let chunk = OwnedChunk::with_bytes(record, vec![0xa5; 32 * 1024 * 1024]);
        let started = std::time::Instant::now();
        client.submit_owned(chunk).unwrap();
        println!(
            "independent sink: 32 MiB retained admission (not network delivery): {:?}",
            started.elapsed()
        );
        let original = OwnedChunk::with_bytes(record + 1, vec![0xa5; 16 * 1024 * 1024]);
        let pointer = original.segments[0].as_ptr();
        let (status, returned) = client.submit_owned(original).unwrap_err();
        assert_eq!(status, BUSY);
        assert_eq!(returned.segments[0].as_ptr(), pointer);
        let reply = control(json!({"action":"consume","channel":id}));
        assert_eq!(reply["bytes"], 32 * 1024 * 1024);
        assert_eq!(reply["valid"], true);
    }
    // Leave a producer-owned allocation queued: dropping the binding must
    // synchronously call back into this binary to release it before unloading.
    client
        .submit_owned(OwnedChunk::with_bytes(4, vec![0xa5; 1024]))
        .unwrap();
    drop(client);
    // SAFETY: all bindings and callbacks have been retired before DLL unload.
    unsafe { (api.destroy)(module) };
    drop(library);
}

// Exercise the real connection adapter with runtime-attested native consumers.
// This is not a TLS connection or a separately loaded consumer DLL.
#[test]
fn duplex_owner_uses_runtime_attested_consumers_and_retires_on_denial() {
    use amaterasu_nvhttp::duplex_owner::{Binding, BufferLimits, DuplexOwner};
    use std::io::Write;
    let runtime = Runtime::new();
    let provider = runtime.client("provider");
    let consumer = runtime.client("consumer");
    let outsider = runtime.client("outsider");
    let hub = Hub::new(2, 8).unwrap();
    let factory = hub.factory();
    // SAFETY: runtime and factory outlive all native bindings and the owner.
    unsafe { register_transfer(&provider, SERVICE, &factory.api).unwrap() };
    let mut input = config(SOURCE, 4);
    input.limits.max_metadata_bytes = 0;
    let mut output = config(SINK, 4);
    output.limits.max_metadata_bytes = 0;
    for (principal, chosen, rejected) in [
        ("consumer", &consumer, &outsider),
        ("outsider", &outsider, &consumer),
    ] {
        let mut owner = DuplexOwner::new(
            &hub,
            Binding {
                connection_id: 1,
                peer_certificate: vec![1],
                consumer_principal: principal.into(),
            },
            input,
            output,
            b"OK".to_vec(),
            b"pre".to_vec(),
            BufferLimits {
                response: 4,
                staging: 4,
            },
        )
        .unwrap();
        let input_id = owner.pair().input_id();
        let output_id = owner.pair().output_id();
        // SAFETY: factory and runtime outlive every binding in this iteration.
        assert!(unsafe { TransferClient::bind(rejected, SERVICE, input_id, input) }.is_err());
        let source = unsafe { TransferClient::bind(chosen, SERVICE, input_id, input).unwrap() };
        let sink = unsafe { TransferClient::bind(chosen, SERVICE, output_id, output).unwrap() };
        owner.flush_input().unwrap();
        let mut received = Vec::new();
        assert_eq!(
            source.receive(|view| {
                for segment in view.segments {
                    received.extend_from_slice(segment);
                }
                STATUS_OK
            }),
            STATUS_OK
        );
        assert_eq!(received, b"pre");
        assert_eq!(
            sink.submit(&OwnedChunk::with_bytes(1, b"data".to_vec())),
            STATUS_OK
        );
        let mut wire = Vec::new();
        owner
            .write_step(&mut wire, |binding| binding.consumer_principal == principal)
            .unwrap();
        owner.response_flushed().unwrap();
        owner.write_step(&mut wire, |_| true).unwrap();
        wire.flush().unwrap();
        assert_eq!(wire, b"OKdata");
        assert_eq!(
            owner.write_step(&mut wire, |_| false).unwrap_err().kind(),
            std::io::ErrorKind::PermissionDenied
        );
        assert_eq!(
            sink.submit(&OwnedChunk::with_bytes(2, b"x".to_vec())),
            CLOSED
        );
        assert_eq!(source.receive(|_| STATUS_OK), CLOSED);
        drop(sink);
        drop(source);
        drop(owner);
    }
    let mut owner = DuplexOwner::new(
        &hub,
        Binding {
            connection_id: 2,
            peer_certificate: vec![1],
            consumer_principal: "consumer".into(),
        },
        input,
        output,
        b"OK".to_vec(),
        vec![],
        BufferLimits {
            response: 4,
            staging: 4,
        },
    )
    .unwrap();
    // SAFETY: runtime and factory outlive the two bindings.
    let source = unsafe {
        TransferClient::bind(&consumer, SERVICE, owner.pair().input_id(), input).unwrap()
    };
    let sink = unsafe {
        TransferClient::bind(&consumer, SERVICE, owner.pair().output_id(), output).unwrap()
    };
    drop(source); // independently released native input binding
    assert_eq!(owner.check_channels(), Err(CLOSED));
    assert_eq!(
        sink.submit(&OwnedChunk::with_bytes(1, b"x".to_vec())),
        CLOSED
    );
    drop(sink);
    drop(owner);
}

// Two separately compiled C consumers use the documented public table against
// the same adapter. This does not exercise broker dispatch or real TLS I/O.
#[test]
#[ignore = "requires independently compiled neutral consumer libraries"]
fn independently_built_neutral_duplex_consumers_use_the_same_owner() {
    use amaterasu_nvhttp::duplex_owner::{Binding, BufferLimits, DuplexOwner};
    type Exchange = unsafe extern "C" fn(*const TransferApi, ChannelId, ChannelId) -> Status;
    let runtime = Runtime::new();
    let provider = runtime.client("provider");
    let consumer = runtime.client("consumer");
    let hub = Hub::new(2, 8).unwrap();
    let factory = hub.factory();
    // SAFETY: the runtime, factory and hub remain alive until all bindings drop.
    unsafe { register_transfer(&provider, SERVICE, &factory.api).unwrap() };
    let mut input = config(SOURCE, 4);
    input.limits.max_metadata_bytes = 0;
    let mut output = config(SINK, 4);
    output.limits.max_metadata_bytes = 0;
    for (variable, expected) in [
        ("AMATERASU_DUPLEX_IDENTITY", b"OKpre".as_slice()),
        ("AMATERASU_DUPLEX_REVERSE", b"OKerp".as_slice()),
    ] {
        let path = std::env::var(variable).expect("fixture library path");
        // SAFETY: test fixture is built from the repository and stays loaded
        // until all its synchronous callbacks and native bindings have retired.
        let library = unsafe { libloading::Library::new(path).unwrap() };
        let exchange: libloading::Symbol<Exchange> =
            unsafe { library.get(b"neutral_exchange\0").unwrap() };
        let mut owner = DuplexOwner::new(
            &hub,
            Binding {
                connection_id: 1,
                peer_certificate: vec![1],
                consumer_principal: "consumer".into(),
            },
            input,
            output,
            b"OK".to_vec(),
            b"pre".to_vec(),
            BufferLimits {
                response: 4,
                staging: 4,
            },
        )
        .unwrap();
        // SAFETY: runtime and provider factory outlive this typed native binding.
        let binding = unsafe {
            consumer
                .bind_native_typed(
                    SERVICE,
                    TRANSFER_CONTRACT,
                    INTERFACE_MAJOR,
                    INTERFACE_MINOR,
                    API_SIZE,
                )
                .unwrap()
        };
        // SAFETY: registration promises this exact transfer contract and layout.
        let api = unsafe { binding.table::<TransferApi>().unwrap() };
        owner.flush_input().unwrap();
        // SAFETY: native table, channel IDs and fixture code remain live for the
        // call; the C consumer uses only synchronous borrows and copies input.
        assert_eq!(
            unsafe { exchange(api, owner.pair().input_id(), owner.pair().output_id()) },
            STATUS_OK
        );
        let mut wire = Vec::new();
        owner.write_step(&mut wire, |_| true).unwrap();
        owner.response_flushed().unwrap();
        owner.write_step(&mut wire, |_| true).unwrap();
        assert_eq!(wire, expected);
        owner.signal_cancel();
        drop(binding);
        drop(owner);
        drop(library);
    }
}
