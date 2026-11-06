use super::*;
use amaterasu_media::*;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

#[derive(Default)]
struct Counts {
    binds: AtomicUsize,
    releases: AtomicUsize,
    reads: AtomicUsize,
    queries: AtomicUsize,
}
struct OldFactory {
    api: NativeServiceFactory,
    counts: Arc<Counts>,
}
struct OldBinding {
    api: CaptureApiPrefix,
    counts: Arc<Counts>,
}
impl OldFactory {
    fn new(counts: Arc<Counts>) -> Box<Self> {
        let mut factory = Box::new(Self {
            api: NativeServiceFactory {
                header: AbiHeader::of::<NativeServiceFactory>(),
                context: std::ptr::null_mut(),
                bind: Some(bind_old),
            },
            counts,
        });
        factory.api.context = (&mut *factory as *mut Self).cast();
        factory
    }
}
unsafe extern "C" fn bind_old(
    context: *mut c_void,
    _: BorrowedBytes,
    _: *const NativeInterfaceQuery,
    output: *mut *const NativeInterface,
) -> Status {
    // SAFETY: fixture retains the boxed factory and writable result through bind.
    let factory = unsafe { &*context.cast::<OldFactory>() };
    factory.counts.binds.fetch_add(1, Ordering::SeqCst);
    let mut binding = Box::new(OldBinding {
        api: CaptureApiPrefix {
            base: NativeInterface {
                header: AbiHeader {
                    size: CAPTURE_API_V1_0_SIZE,
                    major: 1,
                    minor: 0,
                },
                context: std::ptr::null_mut(),
                release: Some(release_old),
            },
            read_frame: read_old,
        },
        counts: factory.counts.clone(),
    });
    binding.api.base.context = (&mut *binding as *mut OldBinding).cast();
    let pointer = (&binding.api.base) as *const NativeInterface;
    let _ = Box::into_raw(binding);
    unsafe { output.write(pointer) };
    STATUS_OK
}
unsafe extern "C" fn release_old(context: *mut c_void) {
    // SAFETY: fixture wrapper uniquely releases this boxed original binding.
    let binding = unsafe { Box::from_raw(context.cast::<OldBinding>()) };
    binding.counts.releases.fetch_add(1, Ordering::SeqCst);
}
unsafe extern "C" fn read_old(context: *mut c_void, _: u64, sink: FrameSink) -> Status {
    // SAFETY: fixture's original binding remains live through forwarding.
    let binding = unsafe { &*context.cast::<OldBinding>() };
    binding.counts.reads.fetch_add(1, Ordering::SeqCst);
    let pixels = [0u8; 16];
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
    // SAFETY: initialized pixels and sink remain borrowed for this callback only.
    unsafe { sink.send(&frame) }
}

struct OwnedBinding(*const NativeInterface);
impl OwnedBinding {
    fn bind(factory: &NativeServiceFactory) -> Self {
        let query = NativeInterfaceQuery {
            header: AbiHeader::of::<NativeInterfaceQuery>(),
            interface_major: 1,
            min_minor: 11,
            min_size: CAPTURE_API_V1_11_SIZE,
        };
        let mut output = std::ptr::null();
        // SAFETY: fixture factory, query, caller and result remain live throughout bind.
        assert_eq!(
            unsafe {
                factory.bind.unwrap()(
                    factory.context,
                    BorrowedBytes::new(b"capture-consumer"),
                    &query,
                    &mut output,
                )
            },
            STATUS_OK
        );
        Self(output)
    }
    fn prefix(&self) -> &CaptureApiPrefix {
        // SAFETY: wrapper advertises and retains the complete Capture prefix.
        unsafe { &*self.0.cast::<CaptureApiPrefix>() }
    }
}
impl Drop for OwnedBinding {
    fn drop(&mut self) {
        // SAFETY: fixture uniquely releases its binding after synchronous queries end.
        let base = unsafe { &*self.0 };
        unsafe { base.release.unwrap()(base.context) };
    }
}

fn request() -> SourcePreflightRequest {
    SourcePreflightRequest {
        source_class: SourceClass::ManagedOutput,
        route: None,
        mode: SourceMode {
            width: 1920,
            height: 1080,
            refresh_millihz: 60_000,
            hdr: false,
        },
    }
}
fn ready(request: &SourcePreflightRequest) -> SourcePreflightResponse {
    SourcePreflightResponse::ready(
        request,
        vec![SourceModeCapabilities {
            source_class: request.source_class,
            min_width: 1,
            max_width: 8192,
            min_height: 1,
            max_height: 8192,
            min_refresh_millihz: 1_000,
            max_refresh_millihz: 240_000,
            sdr: true,
            hdr: true,
        }],
    )
}
struct FirstProvider(Arc<Counts>);
impl SourcePreflightProvider for FirstProvider {
    fn query(
        &self,
        caller: &str,
        request: &SourcePreflightRequest,
    ) -> Result<SourcePreflightResponse, Status> {
        assert_eq!(caller, "capture-consumer");
        self.0.queries.fetch_add(1, Ordering::SeqCst);
        Ok(ready(request))
    }
}
struct ReplacementProvider(Arc<Counts>);
impl SourcePreflightProvider for ReplacementProvider {
    fn query(
        &self,
        caller: &str,
        request: &SourcePreflightRequest,
    ) -> Result<SourcePreflightResponse, Status> {
        self.0.queries.fetch_add(1, Ordering::SeqCst);
        if caller != "capture-consumer" {
            return Ok(SourcePreflightResponse::unavailable(
                request,
                "caller_denied",
            ));
        }
        Ok(ready(request))
    }
}
fn unchanged_consumer<P: SourcePreflightProvider + 'static>(
    provider: P,
    counts: Arc<Counts>,
) -> SourcePreflightResponse {
    let old = OldFactory::new(counts.clone());
    // SAFETY: fixture's old factory allocates binding metadata only, remains
    // retained, and authenticates the same Capture family as the wrapper.
    let factory = unsafe { SourcePreflightFactory::new(&old.api, Arc::new(provider)) };
    let binding = OwnedBinding::bind(&factory.api);
    let request = request();
    // SAFETY: fixture owns this exact Capture v1.11 binding through the query.
    let response = unsafe { source_preflight(binding.prefix(), &request) }.unwrap();
    assert!(response.is_ready());
    assert_eq!(
        counts.reads.load(Ordering::SeqCst),
        0,
        "preflight cannot start a capture read"
    );
    drop(binding);
    assert_eq!(counts.binds.load(Ordering::SeqCst), 1);
    assert_eq!(counts.releases.load(Ordering::SeqCst), 1);
    response
}

#[test]
fn unchanged_consumer_queries_two_independent_providers_without_capture_effects() {
    let first = Arc::new(Counts::default());
    let second = Arc::new(Counts::default());
    let original = unchanged_consumer(FirstProvider(first.clone()), first.clone());
    let replacement = unchanged_consumer(ReplacementProvider(second.clone()), second.clone());
    assert_eq!(original, replacement);
    assert_eq!(first.queries.load(Ordering::SeqCst), 1);
    assert_eq!(second.queries.load(Ordering::SeqCst), 1);
}

#[test]
fn wrapping_an_old_provider_preserves_cpu_prefix_and_reports_missing_optional_tails() {
    let counts = Arc::new(Counts::default());
    let old = OldFactory::new(counts.clone());
    let factory =
        unsafe { SourcePreflightFactory::new(&old.api, Arc::new(FirstProvider(counts.clone()))) };
    let binding = OwnedBinding::bind(&factory.api);
    unsafe extern "C" fn pixels(context: *mut c_void, frame: *const CpuFrame) -> Status {
        // SAFETY: original provider borrows complete initialized pixels through this callback.
        assert_eq!(unsafe { (&*frame).bytes() }.unwrap(), &[0u8; 16]);
        unsafe { &*context.cast::<AtomicUsize>() }.fetch_add(1, Ordering::SeqCst);
        STATUS_OK
    }
    let accepted = AtomicUsize::new(0);
    let prefix = binding.prefix();
    assert_eq!(
        unsafe {
            (prefix.read_frame)(
                prefix.base.context,
                1,
                FrameSink {
                    context: (&accepted as *const AtomicUsize).cast_mut().cast(),
                    write: pixels,
                },
            )
        },
        STATUS_OK
    );
    let api = unsafe { &*binding.0.cast::<CaptureApiV1_11>() };
    let rectangle = DesktopRect {
        left: 0,
        top: 0,
        width: 2,
        height: 2,
    };
    let mut geometry = CaptureGeometry::new(rectangle, rectangle).unwrap();
    assert_eq!(
        unsafe { (api.base.base.base.base.geometry)(prefix.base.context, 1, &mut geometry) },
        MEDIA_UNSUPPORTED
    );
    assert_eq!(counts.reads.load(Ordering::SeqCst), 1);
    assert_eq!(accepted.load(Ordering::SeqCst), 1);
}

#[test]
fn ordinary_capture_bind_keeps_original_header_instead_of_advertising_new_tails() {
    let counts = Arc::new(Counts::default());
    let old = OldFactory::new(counts.clone());
    let factory =
        unsafe { SourcePreflightFactory::new(&old.api, Arc::new(FirstProvider(counts.clone()))) };
    let query = NativeInterfaceQuery {
        header: AbiHeader::of::<NativeInterfaceQuery>(),
        interface_major: 1,
        min_minor: 0,
        min_size: CAPTURE_API_V1_0_SIZE,
    };
    let mut output = std::ptr::null();
    // SAFETY: factory, query, identity and output live through this synchronous bind.
    assert_eq!(
        unsafe {
            factory.api.bind.unwrap()(
                factory.api.context,
                BorrowedBytes::new(b"capture-consumer"),
                &query,
                &mut output,
            )
        },
        STATUS_OK
    );
    let ordinary = OwnedBinding(output);
    assert_eq!(ordinary.prefix().base.header.minor, 0);
    assert_eq!(ordinary.prefix().base.header.size, CAPTURE_API_V1_0_SIZE);
    // SAFETY: this is exactly the original complete prefix; the SDK must stop
    // before accessing a tail that this old provider does not allocate.
    assert_eq!(
        unsafe { source_preflight(ordinary.prefix(), &request()) },
        Err(MEDIA_UNSUPPORTED)
    );
    assert_eq!(counts.queries.load(Ordering::SeqCst), 0);
    drop(ordinary);
    assert_eq!(counts.releases.load(Ordering::SeqCst), 1);
}

#[test]
fn old_or_incomplete_tables_fail_before_reading_the_optional_tail() {
    let old = CaptureApiPrefix {
        base: NativeInterface {
            header: AbiHeader {
                size: CAPTURE_API_V1_0_SIZE,
                major: 1,
                minor: 0,
            },
            context: std::ptr::null_mut(),
            release: None,
        },
        read_frame: read_old,
    };
    // SAFETY: this prefix is its complete advertised Capture table; no v1.11 tail exists.
    assert_eq!(
        unsafe { source_preflight(&old, &request()) },
        Err(MEDIA_UNSUPPORTED)
    );
    let counts = Arc::new(Counts::default());
    let factory = OldFactory::new(counts.clone());
    let wrapped = unsafe {
        SourcePreflightFactory::new(&factory.api, Arc::new(FirstProvider(counts.clone())))
    };
    let binding = OwnedBinding::bind(&wrapped.api);
    let mut_pointer = binding.0.cast_mut();
    for header in [
        AbiHeader {
            size: CAPTURE_API_V1_11_SIZE,
            major: 2,
            minor: 11,
        },
        AbiHeader {
            size: CAPTURE_API_V1_11_SIZE,
            major: 1,
            minor: 10,
        },
        AbiHeader {
            size: CAPTURE_API_V1_11_SIZE - 1,
            major: 1,
            minor: 11,
        },
    ] {
        // SAFETY: test uniquely owns the underlying binding and mutates only
        // header metadata between synchronous calls; no query runs concurrently.
        unsafe { (*mut_pointer).header = header };
        assert_eq!(
            unsafe { source_preflight(binding.prefix(), &request()) },
            Err(MEDIA_UNSUPPORTED)
        );
    }
    assert_eq!(counts.queries.load(Ordering::SeqCst), 0);
}

#[test]
fn payload_and_response_helpers_preserve_opaque_routes_and_reject_false_ready_facts() {
    let request = request();
    assert!(source_preflight_payload("third-party.capture-route/2", &request).is_ok());
    let response = ready(&request);
    let payload = json!({"ok":true,"preflight":response});
    assert_eq!(
        decode_video_source_preflight(&payload, &request).unwrap(),
        response
    );
    let mut changed = request.clone();
    changed.mode.hdr = true;
    assert!(decode_video_source_preflight(&payload, &changed).is_err());
    let mut wrong = ready(&changed);
    wrong.capabilities[0].hdr = false;
    assert!(wrong.validate(&changed).is_err());
    let mut missing = ready(&request);
    missing.capabilities.clear();
    assert!(missing.validate(&request).is_err());
    let mut oversized = request.clone();
    oversized.route = Some("x".repeat(513));
    assert!(oversized.validate().is_err());
}

#[test]
fn native_binding_requires_exact_capture_family_and_never_retries_untyped() {
    unsafe extern "C" fn host(
        context: *mut c_void,
        input: BorrowedBytes,
        sink: ReplySink,
    ) -> Status {
        // SAFETY: test endpoint retains counter and JSON bytes through invoke.
        let count = unsafe { &*context.cast::<AtomicUsize>() };
        count.fetch_add(1, Ordering::SeqCst);
        let command: crate::Command =
            serde_json::from_slice(unsafe { input.read() }.unwrap()).unwrap();
        match command {
            crate::Command::NativeBindTyped {
                service,
                contract,
                interface_major,
                min_minor,
                min_size,
            } => {
                assert_eq!(service, "third-party.capture/1");
                assert_eq!(contract, native_contracts::CAPTURE);
                assert_eq!(
                    (interface_major, min_minor, min_size),
                    (1, 11, CAPTURE_API_V1_11_SIZE)
                );
            }
            _ => panic!("unexpected untyped fallback"),
        }
        let response = crate::Response::Error {
            code: "native_contract_mismatch".into(),
            message: "wrong interface family".into(),
        };
        unsafe { sink.send(&serde_json::to_vec(&response).unwrap()) };
        STATUS_OK
    }
    let calls = AtomicUsize::new(0);
    // SAFETY: endpoint and counter remain live until the client/query are dropped.
    let runtime = unsafe {
        RuntimeClient::from_endpoint(HostApi {
            header: AbiHeader::of::<HostApi>(),
            context: (&calls as *const AtomicUsize).cast_mut().cast(),
            invoke: host,
        })
    };
    assert!(unsafe { SourcePreflightConnection::bind(&runtime, "third-party.capture/1") }.is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}
