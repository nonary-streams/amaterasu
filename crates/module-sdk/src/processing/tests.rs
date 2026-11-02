use super::*;
use std::{cell::Cell, rc::Rc};

#[derive(Default)]
struct State {
    outstanding: Cell<u32>,
    cancelled: Cell<bool>,
    shutdown: Cell<bool>,
}
struct Output {
    references: Cell<u32>,
    state: Rc<State>,
}
unsafe extern "C" fn retain(context: *mut c_void) -> Status {
    // SAFETY: fixture owns its boxed output through the final reference.
    let output = unsafe { &*context.cast::<Output>() };
    output.references.set(output.references.get() + 1);
    STATUS_OK
}
unsafe extern "C" fn release_output(context: *mut c_void, fence: i32) {
    assert_eq!(fence, -1);
    let output = unsafe { &*context.cast::<Output>() };
    if output.references.get() == 1 {
        let output = unsafe { Box::from_raw(context.cast::<Output>()) };
        output
            .state
            .outstanding
            .set(output.state.outstanding.get() - 1);
    } else {
        output.references.set(output.references.get() - 1);
    }
}
fn frame() -> DmaBufFrame {
    let mut planes = [DmaBufPlane::UNUSED; DMABUF_MAX_PLANES];
    planes[0] = DmaBufPlane {
        fd: 0,
        acquire_fence_fd: -1,
        offset: 0,
        pitch: 64,
    };
    DmaBufFrame {
        width: 16,
        height: 16,
        fourcc: u32::from_le_bytes(*b"XR24"),
        plane_count: 1,
        modifier: 0,
        planes,
        color: DMABUF_COLOR_SRGB,
        reserved: 0,
        timestamp_us: 99,
        sequence: 7,
    }
}
unsafe extern "C" fn input_retain(_: *mut c_void) -> Status {
    STATUS_OK
}
unsafe extern "C" fn input_release(_: *mut c_void, _: i32) {}
fn input() -> DmaBufFrameLease {
    DmaBufFrameLease {
        frame: frame(),
        context: std::ptr::dangling_mut::<u8>().cast(),
        retain: Some(input_retain),
        release: Some(input_release),
    }
}
fn output(state: &Rc<State>, frame: DmaBufFrame) -> GpuOutputFrame {
    state.outstanding.set(state.outstanding.get() + 1);
    let context = Box::into_raw(Box::new(Output {
        references: Cell::new(1),
        state: state.clone(),
    }))
    .cast();
    // SAFETY: output owns one initial reference and immutable test descriptors.
    unsafe {
        GpuOutputFrame::from_owned_lease(DmaBufFrameLease {
            frame,
            context,
            retain: Some(retain),
            release: Some(release_output),
        })
    }
    .unwrap()
}

struct FirstEffect {
    state: Rc<State>,
    mode: u32,
}
impl GpuFrameProcessor for FirstEffect {
    fn process(&mut self, _: u64, input: GpuFrame<'_>) -> Result<GpuOutputFrame, Status> {
        if self.state.cancelled.get() {
            return Err(MEDIA_CLOSED);
        }
        if self.mode == 1 {
            return Err(MEDIA_BUSY);
        }
        if self.mode == 2 {
            return Err(MEDIA_BACKEND_ERROR);
        }
        let mut frame = *input.frame();
        frame.color = DMABUF_COLOR_BT2020_PQ;
        if self.mode == 3 {
            frame.sequence += 1;
        }
        Ok(output(&self.state, frame))
    }
    fn cancel(&mut self, _: u64) {
        assert_eq!(self.state.outstanding.get(), 0);
        self.state.cancelled.set(true);
    }
    fn shutdown(&mut self) {
        assert_eq!(self.state.outstanding.get(), 0);
        self.state.shutdown.set(true);
    }
    fn hdr_metadata(&self, _: u64) -> Option<HdrMetadata> {
        Some(HdrMetadata {
            enabled: 1,
            ..Default::default()
        })
    }
}
struct AlternateEffect {
    state: Rc<State>,
}
impl GpuFrameProcessor for AlternateEffect {
    fn process(&mut self, _: u64, input: GpuFrame<'_>) -> Result<GpuOutputFrame, Status> {
        Ok(output(&self.state, *input.frame()))
    }
    fn cancel(&mut self, _: u64) {
        assert_eq!(self.state.outstanding.get(), 0);
    }
    fn shutdown(&mut self) {
        self.state.shutdown.set(true);
    }
}

/// This consumer does not know the concrete effect or choose a fallback.
fn unchanged_consumer(api: &GpuProcessorApi) -> u32 {
    let mut color = 0;
    assert_eq!(
        unsafe {
            process_gpu(api, 44, &input(), CAPTURE_FRAME_CACHED_REUSE, |output| {
                assert_eq!(output.frame.sequence, 7);
                assert_eq!(output.frame.timestamp_us, 99);
                color = output.frame.color;
                STATUS_OK
            })
        },
        ProcessingOutcome::Accepted
    );
    color
}

#[test]
fn unchanged_consumer_accepts_independent_provider_implementations() {
    let first = Rc::new(State::default());
    let second = Rc::new(State::default());
    let a = into_gpu_binding(FirstEffect {
        state: first.clone(),
        mode: 0,
    });
    let b = into_gpu_binding(AlternateEffect {
        state: second.clone(),
    });
    // SAFETY: each SDK-created binding is kept alive until after synchronous use.
    unsafe {
        assert_eq!(unchanged_consumer(&*a), DMABUF_COLOR_BT2020_PQ);
        assert_eq!(unchanged_consumer(&*b), DMABUF_COLOR_SRGB);
        ((*a).cancel_dmabuf_leases)((*a).base.context, 44);
        (*a).base.release.unwrap()((*a).base.context);
        (*b).base.release.unwrap()((*b).base.context);
    }
    assert!(first.cancelled.get());
    assert!(first.shutdown.get());
    assert!(second.shutdown.get());
}

#[test]
fn errors_sink_rejection_and_retention_preserve_reference_ownership() {
    for mode in 0..=3 {
        let state = Rc::new(State::default());
        let raw = into_gpu_binding(FirstEffect {
            state: state.clone(),
            mode,
        });
        let api = unsafe { &*raw };
        let mut held = None;
        // SAFETY: fixture binding stays live through delivery and explicit release.
        let result = unsafe {
            process_gpu(api, 44, &input(), CAPTURE_FRAME_FRESH, |lease| {
                assert_eq!(lease.retain.unwrap()(lease.context), STATUS_OK);
                held = Some(*lease);
                MEDIA_BUSY
            })
        };
        assert_eq!(
            result,
            match mode {
                0 | 1 => ProcessingOutcome::Busy,
                _ => ProcessingOutcome::Failed(MEDIA_BACKEND_ERROR),
            }
        );
        if let Some(held) = held {
            assert_eq!(state.outstanding.get(), 1);
            unsafe {
                held.release.unwrap()(held.context, -1);
            }
        }
        assert_eq!(state.outstanding.get(), 0);
        unsafe {
            api.base.release.unwrap()(api.base.context);
        }
    }
}

#[test]
fn factory_checks_version_and_uses_authenticated_principal() {
    use std::sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    };
    let calls = Arc::new(AtomicU32::new(0));
    let factory = GpuProcessorFactory::new({
        let calls = calls.clone();
        move |principal: &str| {
            assert_eq!(principal, "unchanged.video");
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(AlternateEffect {
                state: Rc::new(State::default()),
            })
        }
    });
    let principal = BorrowedBytes::new(b"unchanged.video");
    let mut query = NativeInterfaceQuery {
        header: AbiHeader::of::<NativeInterfaceQuery>(),
        interface_major: 99,
        min_minor: 0,
        min_size: GPU_PROCESSOR_API_V1_0_SIZE,
    };
    let mut result = std::ptr::null();
    fn invoke<F: Fn(&str) -> Result<AlternateEffect, Status> + Send + Sync>(
        factory: &GpuProcessorFactory<F>,
        principal: BorrowedBytes,
        query: &NativeInterfaceQuery,
        result: &mut *const NativeInterface,
    ) -> Status {
        unsafe {
            bind::<F, AlternateEffect>(
                std::ptr::from_ref(factory).cast_mut().cast(),
                principal,
                query,
                result,
            )
        }
    }
    unsafe {
        assert_eq!(
            invoke(&factory, principal, &query, &mut result),
            MEDIA_UNSUPPORTED
        );
        assert!(result.is_null());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        query.interface_major = 1;
        query.min_minor = 1;
        query.min_size = GPU_PROCESSOR_API_V1_1_SIZE;
        assert_eq!(invoke(&factory, principal, &query, &mut result), STATUS_OK);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        (*result).release.unwrap()((*result).context);
    }
}

#[test]
fn metadata_tail_preserves_prefix_and_initializes_output() {
    assert_eq!(
        std::mem::offset_of!(GpuProcessorApiV1_1, hdr_metadata),
        size_of::<GpuProcessorApi>()
    );
    let state = Rc::new(State::default());
    let raw = into_gpu_binding(FirstEffect { state, mode: 0 });
    let api = unsafe { &*raw.cast::<GpuProcessorApiV1_1>() };
    let mut metadata = HdrMetadata::default();
    unsafe {
        assert_eq!(
            (api.hdr_metadata)(api.base.base.context, 44, &mut metadata),
            STATUS_OK
        );
        assert_eq!(metadata.enabled, 1);
        assert_eq!(
            (api.hdr_metadata)(api.base.base.context, 44, std::ptr::null_mut()),
            STATUS_INVALID_ARGUMENT
        );
        api.base.base.release.unwrap()(api.base.base.context);
    }
}

#[test]
fn consumer_panic_is_contained_and_output_reference_is_retired() {
    let state = Rc::new(State::default());
    let raw = into_gpu_binding(AlternateEffect {
        state: state.clone(),
    });
    let api = unsafe { &*raw };
    assert_eq!(
        unsafe {
            process_gpu(api, 44, &input(), CAPTURE_FRAME_FRESH, |_| {
                panic!("consumer failed")
            })
        },
        ProcessingOutcome::Failed(STATUS_INTERNAL_ERROR)
    );
    assert_eq!(state.outstanding.get(), 0);
    assert_eq!(
        unsafe { process_gpu(api, 44, &input(), 99, |_| panic!("invalid freshness")) },
        ProcessingOutcome::Failed(STATUS_INVALID_ARGUMENT)
    );
    unsafe {
        api.base.release.unwrap()(api.base.context);
    }
}
