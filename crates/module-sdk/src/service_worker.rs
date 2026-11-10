//! Opt-in serialized workers for broker services with blocking CPU/storage work.
//!
//! The existing lifecycle and HostApi contracts are unchanged. The service and
//! all its request, cancellation and persistence state live on one worker. Host
//! ticks only coalesce a wake; they never borrow service state or wait on its I/O.
use crate::{ManagedModule, ModuleResult, RuntimeClient, WorkNotifier};
use std::{
    marker::PhantomData,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender, TrySendError},
    },
    thread::{self, JoinHandle},
    time::Instant,
};

const RUNNING: u8 = 0;
const PREPARING: u8 = 1;
const QUIESCING: u8 = 2;
const STOPPING: u8 = 3;

struct Control {
    phase: AtomicU8,
    deadline_us: AtomicU64,
    origin: Instant,
    failure: OnceLock<String>,
    shutdown_failure: OnceLock<String>,
    retired: AtomicBool,
}

impl Control {
    fn new() -> Self {
        Self {
            phase: AtomicU8::new(RUNNING),
            deadline_us: AtomicU64::new(u64::MAX),
            origin: Instant::now(),
            failure: OnceLock::new(),
            shutdown_failure: OnceLock::new(),
            retired: AtomicBool::new(false),
        }
    }

    fn elapsed_us(&self) -> u64 {
        crate::diagnostics::elapsed_us(self.origin)
    }

    fn announce(&self, remaining_ms: u64) {
        let deadline = self
            .elapsed_us()
            .saturating_add(remaining_ms.saturating_mul(1_000));
        self.deadline_us.fetch_min(deadline, Ordering::AcqRel);
        self.phase.fetch_max(PREPARING, Ordering::AcqRel);
    }

    fn remaining_ms(&self) -> u64 {
        self.deadline_us
            .load(Ordering::Acquire)
            .saturating_sub(self.elapsed_us())
            / 1_000
    }

    fn record_failure(&self, error: String, notifier: &WorkNotifier) {
        if self.failure.set(error).is_ok() {
            notifier.notify();
        }
    }

    fn record_shutdown_failure(&self, error: String, notifier: &WorkNotifier) {
        let _ = self.shutdown_failure.set(error.clone());
        self.record_failure(error, notifier);
    }
}

// HostApi explicitly permits concurrent invokes from consumer workers. This
// endpoint remains borrowed until ServiceWorker has joined its owning thread.
struct WorkerEndpoint(crate::abi::HostApi);
// SAFETY: the endpoint's thread-safe invoke contract is required by RuntimeClient.
// The endpoint is transferred as a whole; no native table or service state moves.
unsafe impl Send for WorkerEndpoint {}

impl WorkerEndpoint {
    fn client(&self) -> RuntimeClient {
        // SAFETY: the owning module joins this worker before returning from destroy.
        unsafe { RuntimeClient::from_endpoint(self.0) }
    }
}

/// A broker-only service may explicitly export `ServiceWorker<MyService>`.
/// Do not use this adapter for services whose native callbacks independently
/// access their module instance, or whose lifecycle requires host-thread affinity.
pub struct ServiceWorker<M> {
    control: Arc<Control>,
    wake: SyncSender<()>,
    thread: Option<JoinHandle<()>>,
    marker: PhantomData<fn() -> M>,
}

impl<M: ManagedModule + 'static> ManagedModule for ServiceWorker<M> {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        let endpoint = WorkerEndpoint(runtime.endpoint);
        let notifier = runtime.work_notifier();
        let control = Arc::new(Control::new());
        let worker_control = control.clone();
        let (wake, wakes) = mpsc::sync_channel(1);
        let (started, startup) = mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name(format!(
                "ama-{}",
                std::any::type_name::<M>()
                    .rsplit("::")
                    .next()
                    .unwrap_or("service")
            ))
            .spawn(move || {
                let result = catch_unwind(AssertUnwindSafe(|| {
                    let module = match M::start(endpoint.client()) {
                        Ok(module) => module,
                        Err(error) => {
                            let _ = started.send(Err(error));
                            return;
                        }
                    };
                    // Registration and initialization finish before the host may
                    // start dependent services. No live tick waits on this channel.
                    if started.send(Ok(())).is_err() {
                        return;
                    }
                    run(module, wakes, &worker_control, &endpoint, &notifier);
                }));
                if result.is_err() {
                    worker_control.record_failure("service worker panicked".into(), &notifier);
                }
                // Notification follows destruction of the service and its workers.
                notifier.notify();
            })
            .map_err(|error| format!("cannot start service worker: {error}"))?;
        match startup.recv() {
            Ok(Ok(())) => Ok(Self {
                control,
                wake,
                thread: Some(thread),
                marker: PhantomData,
            }),
            outcome => {
                let _ = thread.join();
                match outcome {
                    Ok(Err(error)) => Err(error),
                    _ => Err("service worker initialization failed".into()),
                }
            }
        }
    }

    fn tick(&mut self) -> ModuleResult<()> {
        self.health()?;
        self.wake();
        Ok(())
    }

    fn health(&self) -> ModuleResult<()> {
        if let Some(error) = self.control.failure.get() {
            return Err(error.clone());
        }
        if self.control.phase.load(Ordering::Acquire) < QUIESCING
            && self.thread.as_ref().is_some_and(JoinHandle::is_finished)
        {
            return Err("service worker stopped unexpectedly".into());
        }
        Ok(())
    }

    fn prepare_shutdown(&mut self, remaining_ms: u64) -> ModuleResult<()> {
        self.control.announce(remaining_ms);
        self.wake();
        Ok(())
    }

    fn quiesce(&mut self) -> ModuleResult<bool> {
        if self.control.phase.load(Ordering::Acquire) == RUNNING {
            self.control.announce(0);
        }
        self.control.phase.fetch_max(QUIESCING, Ordering::AcqRel);
        self.wake();
        let Some(thread) = self.thread.as_ref() else {
            return Ok(true);
        };
        if !thread.is_finished() {
            if let Some(error) = self.control.shutdown_failure.get() {
                return Err(error.clone());
            }
            return Ok(false);
        }
        self.thread
            .take()
            .expect("worker exists")
            .join()
            .map_err(|_| "service worker panicked".to_owned())?;
        if self.control.retired.load(Ordering::Acquire) {
            Ok(true)
        } else {
            Err(self
                .control
                .failure
                .get()
                .cloned()
                .unwrap_or_else(|| "service worker stopped without retiring".into()))
        }
    }
}

impl<M> ServiceWorker<M> {
    fn wake(&self) {
        // Full means an existing wake already covers this pass. No request,
        // credential, result or service state is copied into this bounded slot.
        match self.wake.try_send(()) {
            Ok(()) | Err(TrySendError::Full(())) | Err(TrySendError::Disconnected(())) => {}
        }
    }
}

fn run<M: ManagedModule>(
    mut module: M,
    wakes: Receiver<()>,
    control: &Control,
    endpoint: &WorkerEndpoint,
    notifier: &WorkNotifier,
) {
    let mut announced = false;
    let mut retired = false;
    let mut timing = crate::diagnostics::ModuleTiming::new(Instant::now());
    while wakes.recv().is_ok() {
        let phase = control.phase.load(Ordering::Acquire);
        if phase == STOPPING {
            break;
        }
        if phase >= PREPARING && !announced {
            announced = true;
            if let Err(error) = contain(|| module.prepare_shutdown(control.remaining_ms())) {
                control.record_shutdown_failure(error, notifier);
            }
        }
        if phase == QUIESCING {
            match contain(|| module.quiesce()) {
                Ok(true) => {
                    retired = true;
                    break;
                }
                Ok(false) => {}
                Err(error) => control.record_shutdown_failure(error, notifier),
            }
        }
        // A failed service still receives drain ticks during quiescence, just
        // as a synchronous provider does while dependencies remain available.
        if phase < QUIESCING && control.failure.get().is_some() {
            continue;
        }
        let started = Instant::now();
        timing.begin(started);
        let result = contain(|| module.tick());
        let completed = Instant::now();
        timing.end_tick_cpu();
        let health = contain(|| module.health());
        timing.observe_health(completed, Instant::now(), health.is_err());
        if let Some(mut summary) = timing.finish(started, completed, result.is_err()) {
            summary["module_type"] = std::any::type_name::<M>().into();
            summary["event"] = "service_worker_window".into();
            summary["scope"] = "dedicated_service_worker_calls_not_host_loop_holds".into();
            crate::diagnostics::log_window(&endpoint.client(), "service.worker", &summary);
        }
        if let Err(error) = result.and(health) {
            control.record_failure(error, notifier);
        }
        // Ordinary idle passes do not notify the host. Waking it for every idle
        // completion would create a feedback loop that prevents host sleeping.
    }
    // Native effects, journals, SQLite connections and any service-owned threads
    // retire here before the host observes a finished JoinHandle.
    drop(module);
    control.retired.store(retired, Ordering::Release);
}

fn contain<T>(operation: impl FnOnce() -> ModuleResult<T>) -> ModuleResult<T> {
    catch_unwind(AssertUnwindSafe(operation))
        .unwrap_or_else(|_| Err("service worker operation panicked".into()))
}

impl<M> Drop for ServiceWorker<M> {
    fn drop(&mut self) {
        self.control.phase.store(STOPPING, Ordering::Release);
        self.wake();
        if let Some(thread) = self.thread.take() {
            // Destruction is the final code-lifetime barrier, not a normal tick.
            // The host ordinarily reaches it only after nonblocking quiescence.
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests;
