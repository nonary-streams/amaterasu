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
        unsafe {
            &mut (*(*self.instance.cast::<Instance<ShutdownFixture>>())
                .state
                .get())
            .module
        }
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
    let state = unsafe {
        &mut *(*fixture.instance.cast::<Instance<ShutdownFixture>>())
            .state
            .get()
    };
    let old = std::time::Instant::now() - std::time::Duration::from_secs(11);
    state.timing = crate::diagnostics::ModuleTiming::new(old);
    state.timing.begin(old);
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

/// A provider that serves `echo` directly and can re-enter itself from `tick`.
#[derive(Default)]
struct DirectFixture {
    served: Vec<(String, String)>,
    reenter: Option<(amaterasu_abi::DirectCallHandler, *mut c_void)>,
    reentry_status: Option<Status>,
}

impl ManagedModule for DirectFixture {
    fn start(_runtime: RuntimeClient) -> ModuleResult<Self> {
        Ok(Self::default())
    }

    fn tick(&mut self) -> ModuleResult<()> {
        if let Some((call, instance)) = self.reenter {
            let mut reply = Vec::new();
            // SAFETY: the test supplied this module's own live export and instance.
            self.reentry_status =
                Some(unsafe { call_direct_export(call, instance, "echo", &mut reply) });
        }
        Ok(())
    }

    fn call_direct(&mut self, call: DirectCall<'_>) -> Option<serde_json::Value> {
        if call.payload["action"] == "panic" {
            panic!("direct handler panicked");
        }
        if call.payload["action"] != "echo" {
            return None;
        }
        self.served
            .push((call.caller.to_owned(), call.service.to_owned()));
        Some(serde_json::json!({"ok": true, "echo": call.payload["value"]}))
    }
}

unsafe extern "C" fn collect_reply(context: *mut c_void, bytes: BorrowedBytes) {
    // SAFETY: the context is the caller's reply vector for this call.
    unsafe { &mut *context.cast::<Vec<u8>>() }.extend_from_slice(unsafe { bytes.read() }.unwrap());
}

/// # Safety
/// `instance` must be a live `Instance<DirectFixture>` matching `call`.
unsafe fn call_direct_export(
    call: amaterasu_abi::DirectCallHandler,
    instance: *mut c_void,
    action: &str,
    reply: &mut Vec<u8>,
) -> Status {
    let request = serde_json::to_vec(&serde_json::json!({"action": action, "value": 7})).unwrap();
    // SAFETY: every borrowed range and the sink outlive this synchronous call.
    unsafe {
        call(
            instance,
            BorrowedBytes::new(b"fixture/1"),
            BorrowedBytes::new(b"consumer"),
            BorrowedBytes::new(&request),
            ReplySink {
                context: std::ptr::from_mut(reply).cast(),
                write: collect_reply,
            },
        )
    }
}

fn direct_fixture() -> (*mut c_void, amaterasu_abi::DirectCallHandler) {
    let endpoint = HostApi {
        header: AbiHeader::of::<HostApi>(),
        context: std::ptr::null_mut(),
        invoke: unavailable_runtime,
    };
    let mut instance = std::ptr::null_mut();
    // SAFETY: endpoint and output are valid for this serialized create call.
    assert_eq!(
        unsafe { create::<DirectFixture>(&endpoint, &mut instance) },
        STATUS_OK
    );
    (instance, direct::<DirectFixture>)
}

/// # Safety
/// `instance` must be a live `Instance<DirectFixture>` with no call active.
unsafe fn direct_module<'a>(instance: *mut c_void) -> &'a mut DirectFixture {
    // SAFETY: guaranteed by the caller.
    unsafe { &mut (*(*instance.cast::<Instance<DirectFixture>>()).state.get()).module }
}

#[test]
fn a_direct_call_reaches_the_module_and_returns_its_reply() {
    let (instance, call) = direct_fixture();
    let mut reply = Vec::new();
    // SAFETY: the instance is live and no other call is active.
    assert_eq!(
        unsafe { call_direct_export(call, instance, "echo", &mut reply) },
        STATUS_OK
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&reply).unwrap(),
        serde_json::json!({"ok": true, "echo": 7})
    );
    // SAFETY: the call returned, so no borrow of the module remains.
    assert_eq!(
        unsafe { direct_module(instance) }.served,
        vec![("consumer".to_owned(), "fixture/1".to_owned())]
    );
    // SAFETY: the test owns the instance and destroys it once.
    unsafe { destroy::<DirectFixture>(instance) };
}

#[test]
fn a_declined_operation_and_a_module_without_direct_support_are_unavailable() {
    let (instance, call) = direct_fixture();
    let mut reply = Vec::new();
    // SAFETY: the instance is live and no other call is active.
    assert_eq!(
        unsafe { call_direct_export(call, instance, "other", &mut reply) },
        STATUS_UNAVAILABLE
    );
    assert!(reply.is_empty());
    // SAFETY: as above; the module was not changed by the declined call.
    assert!(unsafe { direct_module(instance) }.served.is_empty());
    unsafe { destroy::<DirectFixture>(instance) };

    // `ShutdownFixture` keeps the trait's default and never serves directly.
    let fixture = ExportedFixture::create();
    // SAFETY: the macro exported this library's table for the same module type.
    let table = unsafe { *amaterasu_module_direct_v1() };
    let request = br#"{"action":"echo"}"#;
    let mut reply = Vec::<u8>::new();
    // SAFETY: the fixture owns a live instance of the exported module type.
    let status = unsafe {
        (table.call)(
            fixture.instance,
            BorrowedBytes::new(b"fixture/1"),
            BorrowedBytes::new(b"consumer"),
            BorrowedBytes::new(request),
            ReplySink {
                context: std::ptr::from_mut(&mut reply).cast(),
                write: collect_reply,
            },
        )
    };
    assert_eq!(status, STATUS_UNAVAILABLE);
}

#[test]
fn a_reentrant_direct_call_is_refused_instead_of_aliasing_the_module() {
    let (instance, call) = direct_fixture();
    // SAFETY: no call is active while the test configures the module.
    unsafe { direct_module(instance) }.reenter = Some((call, instance));
    // SAFETY: the instance is live; `tick` calls back into the same instance.
    assert_eq!(unsafe { tick::<DirectFixture>(instance) }, STATUS_OK);
    // SAFETY: tick returned.
    let module = unsafe { direct_module(instance) };
    assert_eq!(module.reentry_status, Some(STATUS_UNAVAILABLE));
    assert!(module.served.is_empty());

    // Once the tick has returned the module can be entered again.
    module.reenter = None;
    let mut reply = Vec::new();
    assert_eq!(
        unsafe { call_direct_export(call, instance, "echo", &mut reply) },
        STATUS_OK
    );
    unsafe { destroy::<DirectFixture>(instance) };
}

#[test]
fn another_thread_cannot_enter_the_module_through_a_direct_call() {
    let (instance, call) = direct_fixture();
    let address = instance as usize;
    let status = std::thread::spawn(move || {
        let mut reply = Vec::new();
        // SAFETY: the instance outlives this joined thread; the adapter
        // refuses the call before touching the module's state.
        unsafe { call_direct_export(call, address as *mut c_void, "echo", &mut reply) }
    })
    .join()
    .unwrap();
    assert_eq!(status, STATUS_UNAVAILABLE);
    // SAFETY: the thread has been joined.
    assert!(unsafe { direct_module(instance) }.served.is_empty());
    unsafe { destroy::<DirectFixture>(instance) };
}

#[test]
fn a_panicking_direct_handler_is_contained_and_the_module_stays_usable() {
    let (instance, call) = direct_fixture();
    let mut reply = Vec::new();
    // SAFETY: the instance is live and no other call is active.
    assert_eq!(
        unsafe { call_direct_export(call, instance, "panic", &mut reply) },
        STATUS_INTERNAL_ERROR
    );
    assert!(reply.is_empty());
    assert_eq!(
        unsafe { call_direct_export(call, instance, "echo", &mut reply) },
        STATUS_OK
    );
    unsafe { destroy::<DirectFixture>(instance) };
}
