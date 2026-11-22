//! Worker-backed SDK adapter for independently selected CPU frame fitters.
use super::{BorrowedCpuFrame, OwnedCpuFrame, native_contracts};
use crate::{ModuleResult, NativeBinding, RuntimeClient};
use amaterasu_abi::{
    AbiHeader, BorrowedBytes, NativeInterface, NativeInterfaceQuery, NativeServiceFactory,
    STATUS_INVALID_ARGUMENT, STATUS_OK, Status,
};
use amaterasu_media::{
    CPU_FRAME_FIT_MAJOR, CPU_FRAME_FIT_MINOR, CpuFrame, CpuFrameFitApi, CpuFrameFitTarget,
    FrameSink, MEDIA_BACKEND_ERROR, MEDIA_BUSY, MEDIA_PENDING, MEDIA_UNSUPPORTED,
};
use std::{
    cell::UnsafeCell,
    collections::BTreeMap,
    ffi::c_void,
    panic::{AssertUnwindSafe, catch_unwind},
    ptr,
    sync::mpsc::{self, Receiver, SyncSender, TrySendError},
    thread::{self, JoinHandle},
};

const MAX_IN_FLIGHT: usize = 2;

pub fn bind(runtime: &RuntimeClient, service: &str) -> ModuleResult<NativeBinding> {
    // SAFETY: callers only supply a trusted startup-selected alias. The runtime
    // authenticates the family before it calls the factory.
    unsafe {
        runtime.bind_native_typed(
            service,
            native_contracts::CPU_FRAME_FIT,
            CPU_FRAME_FIT_MAJOR,
            CPU_FRAME_FIT_MINOR,
            size_of::<CpuFrameFitApi>() as u32,
        )
    }
}

pub trait Fitter: 'static {
    /// A matching frame may go straight to the encoder if no work is needed.
    fn identity(_: &CpuFrame, _: CpuFrameFitTarget) -> bool {
        false
    }

    /// Process a single job off the host thread. An error completes the job.
    fn fit(
        &mut self,
        frame: OwnedCpuFrame,
        target: CpuFrameFitTarget,
    ) -> Result<OwnedCpuFrame, Status>;
}

type JobId = u64;
type FitResult = Result<OwnedCpuFrame, Status>;

struct Job {
    id: JobId,
    frame: OwnedCpuFrame,
    target: CpuFrameFitTarget,
}

pub struct Factory<F: Fitter> {
    pub api: NativeServiceFactory,
    create: fn() -> F,
}

impl<F: Fitter> Factory<F> {
    pub fn new(create: fn() -> F) -> Box<Self> {
        let mut factory = Box::new(Self {
            api: NativeServiceFactory {
                header: AbiHeader::of::<NativeServiceFactory>(),
                context: ptr::null_mut(),
                bind: Some(bind_factory::<F>),
            },
            create,
        });
        factory.api.context = (&mut *factory as *mut Self).cast();
        factory
    }
}

struct Binding {
    api: CpuFrameFitApi,
    worker_state: UnsafeCell<Worker>,
}

struct Worker {
    jobs: Option<SyncSender<Job>>,
    results: Option<Receiver<(JobId, FitResult)>>,
    worker: Option<JoinHandle<()>>,
    next: JobId,
    streams: BTreeMap<u64, JobId>,
    ready: BTreeMap<JobId, FitResult>,
    identity: fn(&CpuFrame, CpuFrameFitTarget) -> bool,
}

impl Binding {
    fn new<F: Fitter>(create: fn() -> F) -> Result<Box<Self>, Status> {
        let (jobs, work) = mpsc::sync_channel::<Job>(MAX_IN_FLIGHT);
        let (delivery, results) = mpsc::sync_channel(MAX_IN_FLIGHT);
        let worker = thread::Builder::new()
            .name("amaterasu-frame-fit".into())
            .spawn(move || {
                let mut fitter = create();
                while let Ok(job) = work.recv() {
                    let result = fitter.fit(job.frame, job.target);
                    if delivery.send((job.id, result)).is_err() {
                        break;
                    }
                }
            })
            .map_err(|_| MEDIA_BACKEND_ERROR)?;
        let mut binding = Box::new(Self {
            api: CpuFrameFitApi {
                base: NativeInterface {
                    header: AbiHeader {
                        size: size_of::<CpuFrameFitApi>() as u32,
                        major: CPU_FRAME_FIT_MAJOR,
                        minor: CPU_FRAME_FIT_MINOR,
                    },
                    context: ptr::null_mut(),
                    release: Some(release),
                },
                submit,
                receive,
                cancel,
            },
            worker_state: UnsafeCell::new(Worker {
                jobs: Some(jobs),
                results: Some(results),
                worker: Some(worker),
                next: 0,
                streams: BTreeMap::new(),
                ready: BTreeMap::new(),
                identity: F::identity,
            }),
        });
        binding.api.base.context = (&mut *binding as *mut Self).cast();
        Ok(binding)
    }
}

impl Worker {
    fn drain(&mut self) {
        for _ in 0..MAX_IN_FLIGHT {
            let Ok((id, result)) = self.results.as_ref().unwrap().try_recv() else {
                break;
            };
            if self.streams.values().any(|active| *active == id) {
                self.ready.insert(id, result);
            }
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.jobs.take();
        self.results.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

unsafe extern "C" fn bind_factory<F: Fitter>(
    context: *mut c_void,
    _: BorrowedBytes,
    query: *const NativeInterfaceQuery,
    output: *mut *const NativeInterface,
) -> Status {
    boundary(|| {
        if context.is_null() || query.is_null() || output.is_null() {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: the runtime supplies live query and output pointers.
        let query = unsafe { &*query };
        unsafe { *output = ptr::null() };
        if !query.header.supports::<NativeInterfaceQuery>()
            || query.interface_major != CPU_FRAME_FIT_MAJOR
            || query.min_minor > CPU_FRAME_FIT_MINOR
            || query.min_size as usize > size_of::<CpuFrameFitApi>()
        {
            return MEDIA_UNSUPPORTED;
        }
        // SAFETY: the registered factory remains live until consumer destruction.
        let factory = unsafe { &*context.cast::<Factory<F>>() };
        let binding = match Binding::new(factory.create) {
            Ok(binding) => Box::into_raw(binding),
            Err(status) => return status,
        };
        unsafe { *output = &(*binding).api.base };
        STATUS_OK
    })
}

unsafe extern "C" fn submit(
    context: *mut c_void,
    stream: u64,
    frame: *const CpuFrame,
    target: CpuFrameFitTarget,
) -> Status {
    boundary(|| {
        if context.is_null() || frame.is_null() || !target.valid() {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: the typed ABI serializes calls, and the input is borrowed
        // until this call returns. Only the copied frame goes to the worker.
        let binding = unsafe { &mut *(*context.cast::<Binding>()).worker_state.get() };
        binding.drain();
        if binding.streams.len() == MAX_IN_FLIGHT || binding.streams.contains_key(&stream) {
            return MEDIA_BUSY;
        }
        let frame = match unsafe { BorrowedCpuFrame::from_descriptor(&*frame) } {
            Ok(frame) => frame,
            Err(status) => return status,
        };
        if (binding.identity)(frame.descriptor(), target) {
            return MEDIA_UNSUPPORTED;
        }
        let Some(id) = binding.next.checked_add(1) else {
            return MEDIA_BACKEND_ERROR;
        };
        let descriptor = frame.descriptor();
        let mut pixels = Vec::new();
        if pixels.try_reserve_exact(frame.pixels().len()).is_err() {
            return MEDIA_BACKEND_ERROR;
        }
        pixels.extend_from_slice(frame.pixels());
        let frame = match OwnedCpuFrame::new(
            descriptor.width,
            descriptor.height,
            descriptor.format,
            descriptor.stride,
            pixels,
            descriptor.timestamp_us,
            descriptor.sequence,
        ) {
            Ok(frame) => frame,
            Err(status) => return status,
        };
        let job = Job { id, frame, target };
        match binding.jobs.as_ref().unwrap().try_send(job) {
            Ok(()) => {
                binding.next = id;
                binding.streams.insert(stream, id);
                STATUS_OK
            }
            Err(TrySendError::Full(_)) => MEDIA_BUSY,
            Err(TrySendError::Disconnected(_)) => MEDIA_BACKEND_ERROR,
        }
    })
}

unsafe extern "C" fn receive(context: *mut c_void, stream: u64, sink: FrameSink) -> Status {
    boundary(|| {
        if context.is_null() {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: the consumer retains this binding and serializes calls.
        let binding = unsafe { &mut *(*context.cast::<Binding>()).worker_state.get() };
        binding.drain();
        let Some(id) = binding.streams.get(&stream) else {
            return MEDIA_PENDING;
        };
        let Some(result) = binding.ready.get(id) else {
            return if binding.worker.as_ref().unwrap().is_finished() {
                MEDIA_BACKEND_ERROR
            } else {
                MEDIA_PENDING
            };
        };
        let status = match result {
            Ok(frame) => {
                let frame = frame.borrow();
                // SAFETY: the allocation remains live for the synchronous sink.
                unsafe { (sink.write)(sink.context, frame.descriptor()) }
            }
            Err(STATUS_OK | MEDIA_BUSY | MEDIA_PENDING) => MEDIA_BACKEND_ERROR,
            Err(status) => *status,
        };
        if status != MEDIA_BUSY {
            binding.ready.remove(id);
            binding.streams.remove(&stream);
        }
        status
    })
}

unsafe extern "C" fn cancel(context: *mut c_void, stream: u64) {
    // SAFETY: the consumer retains the binding and serializes calls.
    if let Some(binding) = unsafe { context.cast::<Binding>().as_ref() } {
        let worker = unsafe { &mut *binding.worker_state.get() };
        if let Some(id) = worker.streams.remove(&stream) {
            worker.ready.remove(&id);
        }
    }
}

unsafe extern "C" fn release(context: *mut c_void) {
    if !context.is_null() {
        // SAFETY: the runtime releases the exact factory allocation once.
        drop(unsafe { Box::from_raw(context.cast::<Binding>()) });
    }
}

fn boundary(call: impl FnOnce() -> Status) -> Status {
    catch_unwind(AssertUnwindSafe(call)).unwrap_or(MEDIA_BACKEND_ERROR)
}

#[cfg(test)]
mod tests {
    use super::*;
    use amaterasu_media::PIXEL_BGRA8;
    use std::time::{Duration, Instant};

    struct CopyFrame;
    impl Fitter for CopyFrame {
        fn fit(
            &mut self,
            frame: OwnedCpuFrame,
            _: CpuFrameFitTarget,
        ) -> Result<OwnedCpuFrame, Status> {
            Ok(frame)
        }
    }

    struct Delivery {
        expected_sequence: u64,
        calls: usize,
        accept: bool,
    }
    unsafe extern "C" fn deliver(context: *mut c_void, frame: *const CpuFrame) -> Status {
        // SAFETY: the test owns the delivery and the provider lends the frame synchronously.
        let delivery = unsafe { &mut *context.cast::<Delivery>() };
        let frame = unsafe { &*frame };
        if frame.sequence != delivery.expected_sequence {
            return STATUS_INVALID_ARGUMENT;
        }
        delivery.calls += 1;
        if delivery.accept {
            STATUS_OK
        } else {
            MEDIA_BUSY
        }
    }

    #[test]
    fn cancellation_fences_late_results_and_backpressure_retains_the_exact_replacement() {
        let binding = Binding::new(|| CopyFrame).unwrap();
        let api = &binding.api;
        let pixels = [1; 4];
        let mut frame = CpuFrame {
            width: 1,
            height: 1,
            format: PIXEL_BGRA8,
            stride: 4,
            data: pixels.as_ptr(),
            len: pixels.len(),
            timestamp_us: 1,
            sequence: 1,
        };
        let target = CpuFrameFitTarget {
            width: 1,
            height: 1,
            bt709: 1,
            full_range: 0,
        };
        // SAFETY: the ABI calls are serialized and the input is borrowed during each call.
        assert_eq!(
            unsafe { (api.submit)(api.base.context, 7, &frame, target) },
            STATUS_OK
        );
        assert_eq!(
            unsafe { (api.submit)(api.base.context, 7, &frame, target) },
            MEDIA_BUSY
        );
        unsafe { (api.cancel)(api.base.context, 7) };
        frame.sequence = 2;
        assert_eq!(
            unsafe { (api.submit)(api.base.context, 7, &frame, target) },
            STATUS_OK
        );

        let mut delivery = Delivery {
            expected_sequence: 2,
            calls: 0,
            accept: false,
        };
        let sink = FrameSink {
            context: (&mut delivery as *mut Delivery).cast(),
            write: deliver,
        };
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let status = unsafe { (api.receive)(api.base.context, 7, sink) };
            if status != MEDIA_PENDING {
                assert_eq!(status, MEDIA_BUSY);
                break;
            }
            assert!(Instant::now() < deadline, "fit worker did not finish");
            thread::yield_now();
        }
        assert_eq!(delivery.calls, 1);
        delivery.accept = true;
        assert_eq!(
            unsafe { (api.receive)(api.base.context, 7, sink) },
            STATUS_OK
        );
        assert_eq!(delivery.calls, 2);
        assert_eq!(
            unsafe { (api.receive)(api.base.context, 7, sink) },
            MEDIA_PENDING
        );
    }
}
