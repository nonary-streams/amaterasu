//! Provider adapter that preserves an existing capture factory and adds only
//! the explicitly supplied, nonmutating source-preflight behavior.
use super::*;
use amaterasu_media::*;
use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    sync::Arc,
};

/// Capture providers implement their own capability, authorization and current
/// readiness checks. This operation must not start capture, create/mutate a
/// display, spawn a worker, change permission, or reserve a native resource.
pub trait SourcePreflightProvider: Send + Sync {
    fn query(
        &self,
        caller: &str,
        request: &SourcePreflightRequest,
    ) -> Result<SourcePreflightResponse, Status>;
}
impl<F> SourcePreflightProvider for F
where
    F: Fn(&str, &SourcePreflightRequest) -> Result<SourcePreflightResponse, Status> + Send + Sync,
{
    fn query(
        &self,
        caller: &str,
        request: &SourcePreflightRequest,
    ) -> Result<SourcePreflightResponse, Status> {
        self(caller, request)
    }
}

pub struct SourcePreflightFactory<P: SourcePreflightProvider> {
    pub api: NativeServiceFactory,
    inner: NativeServiceFactory,
    provider: Arc<P>,
}

impl<P: SourcePreflightProvider + 'static> SourcePreflightFactory<P> {
    /// Extend the exact Capture family with the optional v1.11 query.
    ///
    /// # Safety
    /// `inner` and its context/code must implement the Capture native family
    /// and outlive this factory and every binding it creates. Inner bind must
    /// allocate only binding metadata, never a capture source/native device.
    /// Its bindings must honor their advertised complete prefix and unique
    /// release. The provider callback must obey the nonmutating trait contract.
    pub unsafe fn new(inner: &NativeServiceFactory, provider: Arc<P>) -> Box<Self> {
        let mut factory = Box::new(Self {
            api: NativeServiceFactory {
                header: AbiHeader::of::<NativeServiceFactory>(),
                context: std::ptr::null_mut(),
                bind: Some(bind::<P>),
            },
            inner: *inner,
            provider,
        });
        factory.api.context = (&mut *factory as *mut Self).cast();
        factory
    }
}

pub(super) struct Inner(pub(super) *const NativeInterface);
impl Inner {
    fn context(&self) -> *mut c_void {
        // SAFETY: constructor retains the authentic original binding until Drop.
        unsafe { &*self.0 }.context
    }
    fn table<T>(&self, minor: u32) -> Option<&T> {
        // SAFETY: original factory guarantees a live complete advertised table.
        let header = unsafe { &*self.0 }.header;
        if header.major != INTERFACE_MAJOR
            || header.minor < minor
            || header.size < size_of::<T>() as u32
        {
            return None;
        }
        // SAFETY: callers select only exact standard Capture prefix types, and
        // both version and complete prefix size were checked above.
        Some(unsafe { &*self.0.cast::<T>() })
    }
}
impl Drop for Inner {
    fn drop(&mut self) {
        // SAFETY: this wrapper uniquely owns the original binding and drops it
        // after every delegated synchronous callback and consumer reference ends.
        if let Some(release) = unsafe { &*self.0 }.release {
            unsafe { release(self.context()) };
        }
    }
}

pub(super) struct Binding<P: SourcePreflightProvider> {
    pub(super) api: CaptureApiV1_13,
    inner: Inner,
    provider: Arc<P>,
    caller: String,
}
impl<P: SourcePreflightProvider + 'static> Binding<P> {
    pub(super) fn new(inner: Inner, provider: Arc<P>, caller: String) -> Box<Self> {
        let timing = inner
            .table::<CaptureApiV1_12>(CAPTURE_FRAME_TIMING_INTERFACE_MINOR)
            .is_some();
        let metadata = inner
            .table::<CaptureApiV1_13>(CAPTURE_METADATA_INTERFACE_MINOR)
            .is_some();
        let mut binding = Box::new(Self {
            api: CaptureApiV1_13 {
                base: CaptureApiV1_12 {
                    base: CaptureApiV1_11 {
                        base: CaptureApiV1_10 {
                            base: CaptureApiV1_9 {
                                base: CaptureApiV1_7 {
                                    base: CaptureApiV1_6 {
                                        base: NativeInterface {
                                            header: AbiHeader {
                                                size: if metadata {
                                                    CAPTURE_API_V1_13_SIZE
                                                } else if timing {
                                                    CAPTURE_API_V1_12_SIZE
                                                } else {
                                                    CAPTURE_API_V1_11_SIZE
                                                },
                                                major: INTERFACE_MAJOR,
                                                minor: if metadata {
                                                    CAPTURE_METADATA_INTERFACE_MINOR
                                                } else if timing {
                                                    CAPTURE_FRAME_TIMING_INTERFACE_MINOR
                                                } else {
                                                    CAPTURE_SOURCE_PREFLIGHT_INTERFACE_MINOR
                                                },
                                            },
                                            context: std::ptr::null_mut(),
                                            release: Some(release::<P>),
                                        },
                                        read_frame: read_frame::<P>,
                                        geometry: geometry::<P>,
                                        read_frame_with_freshness: read_fresh::<P>,
                                        read_frame_with_capture_timestamp: read_timed::<P>,
                                        read_dmabuf_frame: read_dmabuf::<P>,
                                        latest_presentation: latest_presentation::<P>,
                                        read_dmabuf_frame_leased: read_leased::<P>,
                                    },
                                    subscribe_events: subscribe::<P>,
                                    unsubscribe_events: unsubscribe::<P>,
                                    resolve_swap: resolve_swap::<P>,
                                },
                                enumerate_resources: enumerate_resources::<P>,
                                select_resource: select_resource::<P>,
                                read_resource_frame: read_resource::<P>,
                                hdr_metadata: hdr_metadata::<P>,
                            },
                            color_provenance: color_provenance::<P>,
                        },
                        source_preflight: query::<P>,
                    },
                    frame_timing: frame_timing::<P>,
                },
                read_resource_frame_with_metadata: read_metadata::<P>,
            },
            inner,
            provider,
            caller,
        });
        binding.api.base.base.base.base.base.base.base.context =
            (&mut *binding as *mut Self).cast();
        binding
    }
}

fn guarded(operation: impl FnOnce() -> Status) -> Status {
    catch_unwind(AssertUnwindSafe(operation)).unwrap_or(STATUS_INTERNAL_ERROR)
}

unsafe extern "C" fn bind<P: SourcePreflightProvider + 'static>(
    context: *mut c_void,
    caller: BorrowedBytes,
    query: *const NativeInterfaceQuery,
    output: *mut *const NativeInterface,
) -> Status {
    guarded(|| {
        if output.is_null()
            || !(output as usize).is_multiple_of(align_of::<*const NativeInterface>())
        {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: broker supplies an aligned writable result slot.
        unsafe { output.write(std::ptr::null()) };
        if context.is_null()
            || query.is_null()
            || !(query as usize).is_multiple_of(align_of::<NativeInterfaceQuery>())
        {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: broker borrows query and caller identity through this synchronous call.
        let query = unsafe { &*query };
        if !query.header.supports::<NativeInterfaceQuery>()
            || query.interface_major != INTERFACE_MAJOR
            || query.min_minor > CAPTURE_METADATA_INTERFACE_MINOR
            || query.min_size > CAPTURE_API_V1_13_SIZE
        {
            return MEDIA_UNSUPPORTED;
        }
        let Some(caller_text) = (unsafe { caller.read() })
            .ok()
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
            .filter(|caller| {
                !caller.is_empty() && caller.len() <= 128 && !caller.chars().any(char::is_control)
            })
        else {
            return STATUS_INVALID_ARGUMENT;
        };
        // SAFETY: constructor retains this boxed factory and authentic inner factory.
        let factory = unsafe { &*context.cast::<SourcePreflightFactory<P>>() };
        let Some(bind) = factory.inner.bind else {
            return MEDIA_BACKEND_ERROR;
        };
        if query.min_minor < CAPTURE_SOURCE_PREFLIGHT_INTERFACE_MINOR
            && query.min_size < CAPTURE_API_V1_11_SIZE
        {
            // SAFETY: preserve ordinary capture negotiation exactly. Returning
            // the original table avoids advertising optional resource tails an
            // existing consumer would interpret as supported by the source.
            return unsafe { bind(factory.inner.context, caller, query, output) };
        }
        let inner_query = NativeInterfaceQuery {
            header: AbiHeader::of::<NativeInterfaceQuery>(),
            interface_major: INTERFACE_MAJOR,
            min_minor: 0,
            min_size: size_of::<CaptureApiPrefix>() as u32,
        };
        let mut inner = std::ptr::null();
        // SAFETY: constructor requires a pure Capture-family bind; all input
        // borrows and the writable result slot live through this call.
        let status = unsafe { bind(factory.inner.context, caller, &inner_query, &mut inner) };
        if status != STATUS_OK {
            return status;
        }
        if inner.is_null() || !(inner as usize).is_multiple_of(align_of::<NativeInterface>()) {
            return MEDIA_BACKEND_ERROR;
        }
        let inner = Inner(inner);
        if inner.table::<CaptureApiPrefix>(0).is_none() {
            return MEDIA_BACKEND_ERROR;
        }
        if (query.min_minor > CAPTURE_SOURCE_PREFLIGHT_INTERFACE_MINOR
            || query.min_size > CAPTURE_API_V1_11_SIZE)
            && inner
                .table::<CaptureApiV1_12>(CAPTURE_FRAME_TIMING_INTERFACE_MINOR)
                .is_none()
        {
            return MEDIA_UNSUPPORTED;
        }
        if (query.min_minor > CAPTURE_FRAME_TIMING_INTERFACE_MINOR
            || query.min_size > CAPTURE_API_V1_12_SIZE)
            && inner
                .table::<CaptureApiV1_13>(CAPTURE_METADATA_INTERFACE_MINOR)
                .is_none()
        {
            return MEDIA_UNSUPPORTED;
        }
        let binding = Binding::new(inner, factory.provider.clone(), caller_text.into());
        let pointer = (&binding.api as *const CaptureApiV1_13).cast();
        let _ = Box::into_raw(binding);
        // SAFETY: broker receives exactly one stable wrapper binding for unique release.
        unsafe { output.write(pointer) };
        STATUS_OK
    })
}

unsafe extern "C" fn release<P: SourcePreflightProvider>(context: *mut c_void) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if !context.is_null() {
            // SAFETY: broker uniquely releases the boxed wrapper after callbacks end.
            unsafe { drop(Box::from_raw(context.cast::<Binding<P>>())) };
        }
    }));
}

macro_rules! forward {
    ($function:ident,$table:ty,$minor:expr,$field:ident,$($argument:ident:$kind:ty),* $(,)?)=> {
        unsafe extern "C" fn $function<P:SourcePreflightProvider>(context:*mut c_void,$($argument:$kind),*)->Status {
            guarded(|| {
                if context.is_null() {return STATUS_INVALID_ARGUMENT;}
                // SAFETY: broker retains the wrapper through this callback.
                let binding=unsafe {&*context.cast::<Binding<P>>()};
                let Some(table)=binding.inner.table::<$table>($minor) else {return MEDIA_UNSUPPORTED;};
                // SAFETY: exact authentic inner prefix was checked; original
                // binding and caller-owned arguments remain live through delegation.
                unsafe {(table.$field)(binding.inner.context(),$($argument),*)}
            })
        }
    };
}
forward!(read_frame,CaptureApiPrefix,0,read_frame,stream:u64,sink:FrameSink);
forward!(geometry,CaptureApi,1,geometry,stream:u64,output:*mut CaptureGeometry);
forward!(read_fresh,CaptureApiV1_2,2,read_frame_with_freshness,stream:u64,sink:FreshFrameSinkV1_2);
forward!(read_timed,CaptureApiV1_3,3,read_frame_with_capture_timestamp,stream:u64,sink:FreshFrameSinkV1_3);
forward!(read_dmabuf,CaptureApiV1_4,4,read_dmabuf_frame,stream:u64,sink:DmaBufFrameSink);
forward!(latest_presentation,CaptureApiV1_5,5,latest_presentation,stream:u64,output:*mut PresentationStatus);
forward!(read_leased,CaptureApiV1_6,6,read_dmabuf_frame_leased,stream:u64,sink:DmaBufLeaseSink);
forward!(subscribe,CaptureApiV1_7,7,subscribe_events,stream:u64,sink:CaptureEventSink);
forward!(resolve_swap,CaptureApiV1_7,7,resolve_swap,stream:u64,request:u64,decision:u32,reason:BorrowedBytes);
forward!(enumerate_resources,CaptureApiV1_8,8,enumerate_resources,stream:u64,sink:CaptureResourceOfferSink,generation:*mut u64);
forward!(select_resource,CaptureApiV1_8,8,select_resource,stream:u64,generation:u64,offer:u64);
forward!(read_resource,CaptureApiV1_8,8,read_resource_frame,stream:u64,sink:CaptureResourceFrameSink);
forward!(hdr_metadata,CaptureApiV1_9,9,hdr_metadata,stream:u64,output:*mut HdrMetadata);
forward!(frame_timing,CaptureApiV1_12,12,frame_timing,stream:u64,sequence:u64,output:*mut CaptureFrameTimingV1);
forward!(color_provenance,CaptureApiV1_10,10,color_provenance,stream:u64,output:*mut u32);

unsafe extern "C" fn unsubscribe<P: SourcePreflightProvider>(context: *mut c_void, stream: u64) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if context.is_null() {
            return;
        }
        // SAFETY: broker retains wrapper and original binding throughout callback.
        let binding = unsafe { &*context.cast::<Binding<P>>() };
        if let Some(table) = binding.inner.table::<CaptureApiV1_7>(7) {
            unsafe { (table.unsubscribe_events)(binding.inner.context(), stream) };
        }
    }));
}

unsafe extern "C" fn query<P: SourcePreflightProvider>(
    context: *mut c_void,
    input: BorrowedBytes,
    sink: ReplySink,
) -> Status {
    guarded(|| {
        if context.is_null() || input.len > MAX_SOURCE_PREFLIGHT_BYTES {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: broker retains binding and immutable input through this callback.
        let binding = unsafe { &*context.cast::<Binding<P>>() };
        let input = match unsafe { input.read() } {
            Ok(input) => input,
            Err(status) => return status,
        };
        let request: SourcePreflightRequest = match serde_json::from_slice(input) {
            Ok(request) => request,
            Err(_) => return STATUS_INVALID_ARGUMENT,
        };
        if request.validate().is_err() {
            return STATUS_INVALID_ARGUMENT;
        }
        let response = match binding.provider.query(&binding.caller, &request) {
            Ok(response) => response,
            Err(status) => return status,
        };
        if response.validate(&request).is_err() {
            return MEDIA_BACKEND_ERROR;
        }
        let response = match serde_json::to_vec(&response) {
            Ok(response) if response.len() <= MAX_SOURCE_PREFLIGHT_BYTES => response,
            _ => return MEDIA_BACKEND_ERROR,
        };
        // SAFETY: serialized response and caller's sink remain live through its
        // synchronous copy; the contract permits exactly one bounded response.
        unsafe { sink.send(&response) };
        STATUS_OK
    })
}

forward!(read_metadata,CaptureApiV1_13,13,read_resource_frame_with_metadata,stream:u64,sink:FrameResourceMetadataSinkV1);
