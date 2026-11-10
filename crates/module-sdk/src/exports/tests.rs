use super::*;
use amaterasu_abi::{AbiHeader, BorrowedBytes, ModuleApi, ModuleQuiesceApi, ReplySink};

#[derive(Default)]
enum PreparationOutcome {
    #[default]
    Success,
    Error,
    Panic,
}

#[derive(Default)]
struct ShutdownFixture {
    remaining_budget: Option<u64>,
    preparation_outcome: PreparationOutcome,
    quiesce_calls: usize,
}

impl ManagedModule for ShutdownFixture {
    fn start(_runtime: RuntimeClient) -> ModuleResult<Self> {
        Ok(Self::default())
    }

    fn tick(&mut self) -> ModuleResult<()> {
        Ok(())
    }

    fn prepare_shutdown(&mut self, remaining_ms: u64) -> ModuleResult<()> {
        self.remaining_budget = Some(remaining_ms);
        match self.preparation_outcome {
            PreparationOutcome::Success => Ok(()),
            PreparationOutcome::Error => Err("shutdown preparation failed".into()),
            PreparationOutcome::Panic => panic!("shutdown preparation panicked"),
        }
    }

    fn quiesce(&mut self) -> ModuleResult<bool> {
        self.quiesce_calls += 1;
        Ok(true)
    }
}

crate::export_module!(ShutdownFixture);

unsafe extern "C" fn unavailable_runtime(
    _context: *mut c_void,
    _input: BorrowedBytes,
    _reply: ReplySink,
) -> Status {
    STATUS_INTERNAL_ERROR
}

struct ExportedFixture {
    lifecycle: ModuleApi,
    instance: *mut c_void,
}

impl ExportedFixture {
    fn create() -> Self {
        // SAFETY: the macro returns this test library's static lifecycle table.
        let lifecycle = unsafe { *amaterasu_module_v1() };
        assert!(lifecycle.header.supports::<ModuleApi>());
        let endpoint = HostApi {
            header: AbiHeader::of::<HostApi>(),
            context: std::ptr::null_mut(),
            invoke: unavailable_runtime,
        };
        let mut instance = std::ptr::null_mut();
        // SAFETY: endpoint and output are valid for this serialized create call;
        // its runtime stub is static and the fixture never issues runtime calls.
        assert_eq!(
            unsafe { (lifecycle.create)(&endpoint, &mut instance) },
            STATUS_OK
        );
        assert!(!instance.is_null());
        Self {
            lifecycle,
            instance,
        }
    }

    fn module(&mut self) -> &mut ShutdownFixture {
        // SAFETY: this fixture owns the instance created through the macro table,
        // and no lifecycle call occurs while this mutable borrow is retained.
        unsafe { &mut (*self.instance.cast::<Instance<ShutdownFixture>>()).module }
    }
}

impl Drop for ExportedFixture {
    fn drop(&mut self) {
        // SAFETY: this fixture owns the live instance and destroys it once after
        // all synchronous lifecycle calls have returned.
        unsafe { (self.lifecycle.destroy)(self.instance) };
    }
}

#[test]
fn exported_shutdown_forwards_budget_without_starting_quiescence() {
    let mut fixture = ExportedFixture::create();
    // SAFETY: both macro entries return static tables owned by this library.
    let shutdown = unsafe { &*amaterasu_module_shutdown_v1() };
    let quiesce = unsafe { &*amaterasu_module_quiesce_v1() };
    assert!(
        shutdown
            .header
            .supports::<amaterasu_abi::ModuleShutdownApi>()
    );
    assert!(quiesce.header.supports::<ModuleQuiesceApi>());
    for remaining_ms in [0, 731, u64::MAX] {
        // SAFETY: the fixture owns the live, exclusively borrowed module.
        assert_eq!(
            unsafe { (shutdown.prepare)(fixture.instance, remaining_ms) },
            STATUS_OK
        );
        assert_eq!(fixture.module().remaining_budget, Some(remaining_ms));
        assert_eq!(fixture.module().quiesce_calls, 0);
    }
    let mut ready = 0;
    // SAFETY: the fixture owns this live instance and lends a writable result.
    assert_eq!(
        unsafe { (quiesce.poll)(fixture.instance, &mut ready) },
        STATUS_OK
    );
    assert_eq!(ready, 1);
    assert_eq!(fixture.module().quiesce_calls, 1);
}

#[test]
fn exported_shutdown_rejects_a_null_instance() {
    // SAFETY: the macro entry returns this library's static shutdown table.
    let shutdown = unsafe { &*amaterasu_module_shutdown_v1() };
    // SAFETY: the adapter must reject null before accessing an instance.
    assert_eq!(
        unsafe { (shutdown.prepare)(std::ptr::null_mut(), 500) },
        STATUS_INVALID_ARGUMENT
    );
}

#[test]
fn exported_shutdown_contains_module_errors_and_panics() {
    for outcome in [PreparationOutcome::Error, PreparationOutcome::Panic] {
        let mut fixture = ExportedFixture::create();
        fixture.module().preparation_outcome = outcome;
        // SAFETY: the macro returns this library's static shutdown table.
        let shutdown = unsafe { &*amaterasu_module_shutdown_v1() };
        // SAFETY: the fixture owns the live module for this synchronous call.
        assert_eq!(
            unsafe { (shutdown.prepare)(fixture.instance, 417) },
            STATUS_INTERNAL_ERROR
        );
        assert_eq!(fixture.module().remaining_budget, Some(417));
        assert_eq!(fixture.module().quiesce_calls, 0);
        // SAFETY: error and panic containment retain the same live instance.
        assert_eq!(
            unsafe { (fixture.lifecycle.health)(fixture.instance) },
            STATUS_OK
        );
    }
}

struct DefaultShutdown;

impl ManagedModule for DefaultShutdown {
    fn start(_runtime: RuntimeClient) -> ModuleResult<Self> {
        Ok(Self)
    }

    fn tick(&mut self) -> ModuleResult<()> {
        Ok(())
    }
}

#[test]
fn existing_modules_keep_a_noop_shutdown_preparation() {
    let endpoint = HostApi {
        header: AbiHeader::of::<HostApi>(),
        context: std::ptr::null_mut(),
        invoke: unavailable_runtime,
    };
    let mut instance = std::ptr::null_mut();
    // SAFETY: the static stub outlives the exclusively owned instance.
    assert_eq!(
        unsafe { super::create::<DefaultShutdown>(&endpoint, &mut instance) },
        STATUS_OK
    );
    for remaining_ms in [0, 731] {
        // SAFETY: the live instance belongs to the matching create adapter.
        assert_eq!(
            unsafe { super::prepare_shutdown::<DefaultShutdown>(instance, remaining_ms,) },
            STATUS_OK
        );
    }
    // SAFETY: all calls finished and ownership is relinquished exactly once.
    unsafe { super::destroy::<DefaultShutdown>(instance) };
}

#[test]
fn unavailable_timing_log_does_not_change_exported_tick_or_lifecycle() {
    let fixture = ExportedFixture::create();
    // SAFETY: the fixture owns the matching create adapter's opaque allocation.
    let instance = unsafe { &mut *fixture.instance.cast::<Instance<ShutdownFixture>>() };
    let old = std::time::Instant::now() - std::time::Duration::from_secs(11);
    instance.timing = crate::diagnostics::ModuleTiming::new(old);
    instance.timing.begin(old);
    // The delayed interval triggers a log against the unavailable endpoint.
    // SAFETY: this serialized lifecycle call borrows the live fixture.
    assert_eq!(
        unsafe { (fixture.lifecycle.tick)(fixture.instance) },
        STATUS_OK
    );
    assert_eq!(
        unsafe { (fixture.lifecycle.health)(fixture.instance) },
        STATUS_OK
    );
}
