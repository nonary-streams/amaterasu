//! Public source preflight through the real runtime's authenticated native
//! registry. Fixtures allocate binding metadata only; hardware is not involved.
use amaterasu_abi::*;
use amaterasu_contracts::{Grants, RuntimeConfig};
use amaterasu_module_sdk::{
    RuntimeClient,
    capture::{
        SourceClass, SourceMode, SourceModeCapabilities, SourcePreflightConnection,
        SourcePreflightFactory, SourcePreflightProvider, SourcePreflightRequest,
        SourcePreflightResponse, SourceReadiness, source_preflight,
    },
    media::native_contracts,
    media_abi::{
        CAPTURE_API_V1_0_SIZE, CAPTURE_API_V1_11_SIZE, CaptureApiPrefix, CpuFrame, FrameSink,
        MEDIA_UNSUPPORTED, PIXEL_BGRA8,
    },
};
use std::{
    ffi::c_void,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

const SERVICE: &str = "example.opaque-source/1";
const CONSUMER: &str = "example.capture-consumer";
const UNGRANTED: &str = "example.ungranted";
const UNSELECTED: &str = "example.unselected-provider";
const FIRST_PROVIDER: &str = "vendor.ready-capture";
const REPLACEMENT_PROVIDER: &str = "vendor.sdr-capture";

struct TestRuntime {
    api: &'static RuntimeApi,
    handle: *mut c_void,
}
impl TestRuntime {
    fn new(selected_provider: &str) -> Self {
        let config = RuntimeConfig {
            principals: [
                (
                    selected_provider.into(),
                    Grants {
                        native_provide: [SERVICE.into()].into(),
                        ..Default::default()
                    },
                ),
                (
                    CONSUMER.into(),
                    Grants {
                        native_use: [SERVICE.into()].into(),
                        ..Default::default()
                    },
                ),
                (UNGRANTED.into(), Grants::default()),
                (UNSELECTED.into(), Grants::default()),
            ]
            .into(),
            ..Default::default()
        };
        // SAFETY: the runtime export table has static storage.
        let api = unsafe { &*crate::ffi::amaterasu_runtime_v1() };
        let bytes = serde_json::to_vec(&config).unwrap();
        let mut handle = std::ptr::null_mut();
        // SAFETY: configuration bytes and writable handle live through create.
        assert_eq!(
            unsafe { (api.create)(BorrowedBytes::new(&bytes), &mut handle) },
            STATUS_OK
        );
        Self { api, handle }
    }
    fn client(&self, principal: &str) -> RuntimeClient {
        let mut endpoint = std::mem::MaybeUninit::<HostApi>::uninit();
        // SAFETY: this live runtime authenticates the configured principal and
        // writes a complete endpoint. Every test drops bindings before runtime.
        assert_eq!(
            unsafe {
                (self.api.endpoint)(
                    self.handle,
                    BorrowedBytes::new(principal.as_bytes()),
                    endpoint.as_mut_ptr(),
                )
            },
            STATUS_OK
        );
        // SAFETY: endpoint was initialized; tests stop all calls before runtime Drop.
        unsafe { RuntimeClient::from_endpoint(endpoint.assume_init()) }
    }
}
impl Drop for TestRuntime {
    fn drop(&mut self) {
        // SAFETY: tests drop all bindings and finish endpoint calls first; this
        // fixture uniquely owns the runtime handle and destroys it exactly once.
        unsafe { (self.api.destroy)(self.handle) };
    }
}

#[derive(Default)]
struct Counts {
    factory_calls: AtomicUsize,
    metadata_binds: AtomicUsize,
    releases: AtomicUsize,
    reads: AtomicUsize,
    source_opens: AtomicUsize,
    preflights: AtomicUsize,
    factory_callers: Mutex<Vec<String>>,
    metadata_callers: Mutex<Vec<String>>,
    preflight_callers: Mutex<Vec<String>>,
    requested_versions: Mutex<Vec<(u32, u32, u32)>>,
}

/// Observes the exact real-runtime callback before forwarding to either an old
/// Capture implementation or the SDK adapter; it never supplies a host stub.
struct ProbedFactory {
    api: NativeServiceFactory,
    inner: NativeServiceFactory,
    counts: Arc<Counts>,
}
impl ProbedFactory {
    fn new(inner: NativeServiceFactory, counts: Arc<Counts>) -> Box<Self> {
        let mut factory = Box::new(Self {
            api: NativeServiceFactory {
                header: AbiHeader::of::<NativeServiceFactory>(),
                context: std::ptr::null_mut(),
                bind: Some(bind_probed),
            },
            inner,
            counts,
        });
        factory.api.context = (&mut *factory as *mut Self).cast();
        factory
    }
}
unsafe extern "C" fn bind_probed(
    context: *mut c_void,
    caller: BorrowedBytes,
    query: *const NativeInterfaceQuery,
    output: *mut *const NativeInterface,
) -> Status {
    // SAFETY: the test retains the boxed factory, and the real runtime borrows
    // readable caller/query storage and writable output through this callback.
    let factory = unsafe { &*context.cast::<ProbedFactory>() };
    let query_value = unsafe { &*query };
    factory.counts.factory_calls.fetch_add(1, Ordering::SeqCst);
    factory
        .counts
        .factory_callers
        .lock()
        .unwrap()
        .push(String::from_utf8_lossy(unsafe { caller.read() }.unwrap()).into_owned());
    factory.counts.requested_versions.lock().unwrap().push((
        query_value.interface_major,
        query_value.min_minor,
        query_value.min_size,
    ));
    // SAFETY: retained inner factory implements the same exact Capture ABI and
    // preserves callback borrows; returned binding ownership passes to runtime.
    unsafe { factory.inner.bind.unwrap()(factory.inner.context, caller, query, output) }
}

struct PrefixFactory {
    api: NativeServiceFactory,
    counts: Arc<Counts>,
}
struct PrefixBinding {
    api: CaptureApiPrefix,
    counts: Arc<Counts>,
}
impl PrefixFactory {
    fn new(counts: Arc<Counts>) -> Box<Self> {
        let mut factory = Box::new(Self {
            api: NativeServiceFactory {
                header: AbiHeader::of::<NativeServiceFactory>(),
                context: std::ptr::null_mut(),
                bind: Some(bind_prefix),
            },
            counts,
        });
        factory.api.context = (&mut *factory as *mut Self).cast();
        factory
    }
}
unsafe extern "C" fn bind_prefix(
    context: *mut c_void,
    caller: BorrowedBytes,
    _: *const NativeInterfaceQuery,
    output: *mut *const NativeInterface,
) -> Status {
    // SAFETY: the test retains this boxed factory and runtime borrows input and
    // writable output until return. Binding allocates metadata, never a source.
    let factory = unsafe { &*context.cast::<PrefixFactory>() };
    factory.counts.metadata_binds.fetch_add(1, Ordering::SeqCst);
    factory
        .counts
        .metadata_callers
        .lock()
        .unwrap()
        .push(String::from_utf8_lossy(unsafe { caller.read() }.unwrap()).into_owned());
    let mut binding = Box::new(PrefixBinding {
        api: CaptureApiPrefix {
            base: NativeInterface {
                header: AbiHeader {
                    size: CAPTURE_API_V1_0_SIZE,
                    major: 1,
                    minor: 0,
                },
                context: std::ptr::null_mut(),
                release: Some(release_prefix),
            },
            read_frame: read_prefix,
        },
        counts: factory.counts.clone(),
    });
    binding.api.base.context = (&mut *binding as *mut PrefixBinding).cast();
    let pointer = std::ptr::from_ref(&binding.api.base);
    let _ = Box::into_raw(binding);
    // SAFETY: output is writable; unique binding ownership transfers to runtime.
    unsafe { output.write(pointer) };
    STATUS_OK
}
unsafe extern "C" fn release_prefix(context: *mut c_void) {
    // SAFETY: runtime or SDK wrapper uniquely releases this original allocation
    // after all callbacks; it cannot release the same binding twice.
    let binding = unsafe { Box::from_raw(context.cast::<PrefixBinding>()) };
    binding.counts.releases.fetch_add(1, Ordering::SeqCst);
}
unsafe extern "C" fn read_prefix(context: *mut c_void, _: u64, sink: FrameSink) -> Status {
    // SAFETY: binding stays live through synchronous read and sink callback.
    let binding = unsafe { &*context.cast::<PrefixBinding>() };
    binding.counts.reads.fetch_add(1, Ordering::SeqCst);
    // This fixture models source acquisition on read, never during metadata bind.
    binding.counts.source_opens.fetch_add(1, Ordering::SeqCst);
    let pixels = [7_u8; 16];
    let frame = CpuFrame {
        width: 2,
        height: 2,
        format: PIXEL_BGRA8,
        stride: 8,
        data: pixels.as_ptr(),
        len: pixels.len(),
        timestamp_us: 1,
        sequence: 1,
    };
    // SAFETY: frame and initialized pixels remain borrowed until sink returns.
    unsafe { sink.send(&frame) }
}

fn request() -> SourcePreflightRequest {
    SourcePreflightRequest {
        source_class: SourceClass::ManagedOutput,
        route: Some("opaque-output-selector".into()),
        mode: SourceMode {
            width: 1920,
            height: 1080,
            refresh_millihz: 60_000,
            hdr: true,
        },
    }
}
fn capability(max_width: u32, hdr: bool) -> SourceModeCapabilities {
    SourceModeCapabilities {
        source_class: SourceClass::ManagedOutput,
        min_width: 1,
        max_width,
        min_height: 1,
        max_height: 4320,
        min_refresh_millihz: 1_000,
        max_refresh_millihz: 240_000,
        sdr: true,
        hdr,
    }
}
fn observe_query(counts: &Counts, caller: &str) {
    counts.preflights.fetch_add(1, Ordering::SeqCst);
    counts.preflight_callers.lock().unwrap().push(caller.into());
}
struct ReadyProvider(Arc<Counts>);
impl SourcePreflightProvider for ReadyProvider {
    fn query(
        &self,
        caller: &str,
        request: &SourcePreflightRequest,
    ) -> Result<SourcePreflightResponse, Status> {
        observe_query(&self.0, caller);
        Ok(SourcePreflightResponse::ready(
            request,
            vec![capability(8192, true)],
        ))
    }
}
struct SdrOnlyProvider(Arc<Counts>);
impl SourcePreflightProvider for SdrOnlyProvider {
    fn query(
        &self,
        caller: &str,
        request: &SourcePreflightRequest,
    ) -> Result<SourcePreflightResponse, Status> {
        observe_query(&self.0, caller);
        Ok(SourcePreflightResponse {
            capabilities: vec![capability(3840, false)],
            ..SourcePreflightResponse::unsupported(request, "hdr_unavailable")
        })
    }
}

/// Unchanged for both independently selected implementations: consumer knows
/// only the public service alias and exact typed SDK, never provider identity.
fn unchanged_consumer(client: &RuntimeClient) -> Result<SourcePreflightResponse, String> {
    // SAFETY: signed test composition selects the Capture family and keeps the
    // real runtime/provider factories alive until this connection is dropped.
    let connection = unsafe { SourcePreflightConnection::bind(client, SERVICE) }?;
    connection
        .query(&request())
        .map_err(|status| format!("preflight status {status}"))
}

fn query_selected<P: SourcePreflightProvider + 'static>(
    provider_name: &str,
    implementation: P,
    counts: Arc<Counts>,
) -> SourcePreflightResponse {
    let runtime = TestRuntime::new(provider_name);
    let prefix = PrefixFactory::new(counts.clone());
    // SAFETY: original boxed Capture factory allocates metadata only; its code
    // and context outlive the adapter and every resulting binding.
    let adapted = unsafe { SourcePreflightFactory::new(&prefix.api, Arc::new(implementation)) };
    let factory = ProbedFactory::new(adapted.api, counts.clone());
    let provider = runtime.client(provider_name);
    let unselected = runtime.client(UNSELECTED);
    let consumer = runtime.client(CONSUMER);
    // SAFETY: descriptor and all retained factories remain live through every
    // binding and endpoint call; wrong principal must fail before using it.
    let denied = unsafe {
        unselected.register_native_typed(SERVICE, native_contracts::CAPTURE, &factory.api)
    };
    assert!(denied.unwrap_err().contains("permission_denied"));
    // SAFETY: selected provider implements Capture and retains the factory.
    unsafe { provider.register_native_typed(SERVICE, native_contracts::CAPTURE, &factory.api) }
        .unwrap();
    let response = unchanged_consumer(&consumer).unwrap();
    assert_eq!(counts.factory_calls.load(Ordering::SeqCst), 1);
    assert_eq!(counts.metadata_binds.load(Ordering::SeqCst), 1);
    assert_eq!(counts.preflights.load(Ordering::SeqCst), 1);
    assert_eq!(counts.reads.load(Ordering::SeqCst), 0);
    assert_eq!(counts.source_opens.load(Ordering::SeqCst), 0);
    assert_eq!(counts.releases.load(Ordering::SeqCst), 1);
    for callers in [
        &counts.factory_callers,
        &counts.metadata_callers,
        &counts.preflight_callers,
    ] {
        assert_eq!(*callers.lock().unwrap(), [CONSUMER]);
    }
    assert_eq!(
        *counts.requested_versions.lock().unwrap(),
        [(1, 11, CAPTURE_API_V1_11_SIZE)]
    );
    // Consumer connection has ended. Runtime teardown cannot release it again.
    drop(runtime);
    assert_eq!(counts.releases.load(Ordering::SeqCst), 1);
    response
}

#[test]
fn capture_preflight_replaces_selected_providers_through_authenticated_runtime() {
    let first = Arc::new(Counts::default());
    let replacement = Arc::new(Counts::default());
    let ready = query_selected(FIRST_PROVIDER, ReadyProvider(first.clone()), first);
    let unsupported = query_selected(
        REPLACEMENT_PROVIDER,
        SdrOnlyProvider(replacement.clone()),
        replacement,
    );
    assert_eq!(ready.state, SourceReadiness::Ready);
    assert!(ready.capabilities[0].hdr);
    assert_eq!(ready.capabilities[0].max_width, 8192);
    assert_eq!(unsupported.state, SourceReadiness::Unsupported);
    assert_eq!(unsupported.reason.as_deref(), Some("hdr_unavailable"));
    assert!(!unsupported.capabilities[0].hdr);
    assert_eq!(unsupported.capabilities[0].max_width, 3840);
    assert_eq!(ready.request, unsupported.request);
}

#[test]
fn capture_preflight_rejects_wrong_family_before_a_size_compatible_factory_runs() {
    let counts = Arc::new(Counts::default());
    let runtime = TestRuntime::new(FIRST_PROVIDER);
    let prefix = PrefixFactory::new(counts.clone());
    // SAFETY: authentic retained Capture prefix implements the adapter's lifetime rules.
    let adapted = unsafe {
        SourcePreflightFactory::new(&prefix.api, Arc::new(ReadyProvider(counts.clone())))
    };
    let factory = ProbedFactory::new(adapted.api, counts.clone());
    // A custom unrelated family can advertise the same full Capture-shaped
    // table. Size/version compatibility must never confer Capture identity.
    // SAFETY: this isolated negative fixture registers a retained descriptor
    // whose callback must not be invoked for the mismatched requested family.
    unsafe {
        runtime.client(FIRST_PROVIDER).register_native_typed(
            SERVICE,
            "example.unrelated-native-family/1",
            &factory.api,
        )
    }
    .unwrap();
    assert!(
        unchanged_consumer(&runtime.client(CONSUMER))
            .unwrap_err()
            .contains("native_contract_mismatch")
    );
    assert_eq!(counts.factory_calls.load(Ordering::SeqCst), 0);
    assert_eq!(counts.metadata_binds.load(Ordering::SeqCst), 0);
    assert_eq!(counts.preflights.load(Ordering::SeqCst), 0);
    assert_eq!(counts.reads.load(Ordering::SeqCst), 0);
    assert_eq!(counts.source_opens.load(Ordering::SeqCst), 0);
    assert_eq!(counts.releases.load(Ordering::SeqCst), 0);
}

#[test]
fn capture_preflight_missing_grants_absent_and_old_providers_have_no_fallback() {
    let counts = Arc::new(Counts::default());
    let runtime = TestRuntime::new(FIRST_PROVIDER);
    assert!(
        unchanged_consumer(&runtime.client(CONSUMER))
            .unwrap_err()
            .contains("not_found")
    );
    let prefix = PrefixFactory::new(counts.clone());
    let factory = ProbedFactory::new(prefix.api, counts.clone());
    // SAFETY: retained factory implements exactly the complete old Capture prefix.
    unsafe {
        runtime.client(FIRST_PROVIDER).register_native_typed(
            SERVICE,
            native_contracts::CAPTURE,
            &factory.api,
        )
    }
    .unwrap();
    assert!(
        unchanged_consumer(&runtime.client(UNGRANTED))
            .unwrap_err()
            .contains("permission_denied")
    );
    assert_eq!(counts.factory_calls.load(Ordering::SeqCst), 0);
    assert!(
        unchanged_consumer(&runtime.client(CONSUMER))
            .unwrap_err()
            .contains("native_bind_failed")
    );
    assert_eq!(
        counts.factory_calls.load(Ordering::SeqCst),
        1,
        "no untyped retry or default provider"
    );
    assert_eq!(
        *counts.requested_versions.lock().unwrap(),
        [(1, 11, CAPTURE_API_V1_11_SIZE)]
    );
    assert_eq!(counts.metadata_binds.load(Ordering::SeqCst), 1);
    assert_eq!(
        counts.releases.load(Ordering::SeqCst),
        1,
        "incompatible old prefix is released safely"
    );
    assert_eq!(counts.preflights.load(Ordering::SeqCst), 0);
    assert_eq!(counts.reads.load(Ordering::SeqCst), 0);
    assert_eq!(counts.source_opens.load(Ordering::SeqCst), 0);
    drop(runtime);
    assert_eq!(counts.releases.load(Ordering::SeqCst), 1);
}

unsafe extern "C" fn receive_pixels(context: *mut c_void, frame: *const CpuFrame) -> Status {
    // SAFETY: original read borrows complete frame/pixels and this exclusive Vec
    // through the synchronous sink. Neither reference escapes the callback.
    let pixels = unsafe { (&*frame).bytes() }.unwrap();
    unsafe { &mut *context.cast::<Vec<u8>>() }.extend_from_slice(pixels);
    STATUS_OK
}

#[test]
fn capture_preflight_adapter_preserves_ordinary_old_capture_prefix_through_runtime() {
    let counts = Arc::new(Counts::default());
    let runtime = TestRuntime::new(FIRST_PROVIDER);
    let prefix = PrefixFactory::new(counts.clone());
    // SAFETY: prefix allocates only metadata and outlives every adapted binding.
    let adapted = unsafe {
        SourcePreflightFactory::new(&prefix.api, Arc::new(ReadyProvider(counts.clone())))
    };
    let factory = ProbedFactory::new(adapted.api, counts.clone());
    let provider = runtime.client(FIRST_PROVIDER);
    let consumer = runtime.client(CONSUMER);
    // SAFETY: selected retained factory implements the exact Capture native family.
    unsafe { provider.register_native_typed(SERVICE, native_contracts::CAPTURE, &factory.api) }
        .unwrap();
    // SAFETY: runtime and provider outlive this ordinary Capture-family binding.
    let binding = unsafe {
        consumer.bind_native_typed(
            SERVICE,
            native_contracts::CAPTURE,
            1,
            0,
            CAPTURE_API_V1_0_SIZE,
        )
    }
    .unwrap();
    // SAFETY: requested exact authentic Capture prefix; binding remains live.
    let table = unsafe { binding.table::<CaptureApiPrefix>() }.unwrap();
    assert_eq!(
        (
            table.base.header.major,
            table.base.header.minor,
            table.base.header.size
        ),
        (1, 0, CAPTURE_API_V1_0_SIZE)
    );
    // SAFETY: complete advertised old prefix has no preflight tail; helper must
    // reject it without dereferencing or invoking an absent optional callback.
    assert_eq!(
        unsafe { source_preflight(table, &request()) },
        Err(MEDIA_UNSUPPORTED)
    );
    let mut pixels = Vec::<u8>::new();
    // SAFETY: table/binding and exclusive sink storage live through the call.
    assert_eq!(
        unsafe {
            (table.read_frame)(
                table.base.context,
                1,
                FrameSink {
                    context: (&mut pixels as *mut Vec<u8>).cast(),
                    write: receive_pixels,
                },
            )
        },
        STATUS_OK
    );
    assert_eq!(pixels, [7_u8; 16]);
    assert_eq!(counts.preflights.load(Ordering::SeqCst), 0);
    assert_eq!(counts.reads.load(Ordering::SeqCst), 1);
    assert_eq!(counts.source_opens.load(Ordering::SeqCst), 1);
    assert_eq!(
        *counts.requested_versions.lock().unwrap(),
        [(1, 0, CAPTURE_API_V1_0_SIZE)]
    );
    drop(binding);
    assert_eq!(counts.releases.load(Ordering::SeqCst), 1);
    drop(runtime);
    assert_eq!(counts.releases.load(Ordering::SeqCst), 1);
}
