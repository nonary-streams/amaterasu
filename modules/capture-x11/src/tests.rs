use super::{
    backend::{Backend, CaptureSession},
    controller::Controller,
    model::*,
};
use amaterasu_media::{CaptureGeometry, DesktopRect};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

struct FakeBackend {
    opened: Arc<AtomicUsize>,
    dropped: Arc<AtomicUsize>,
}

impl Backend for FakeBackend {
    fn availability(&self) -> NativeAvailability {
        NativeAvailability {
            native_supported: true,
            available: true,
            reason: None,
        }
    }

    fn enumerate(&self) -> CaptureResult<(Vec<Output>, DesktopRect)> {
        let output = output("screen-one");
        Ok((vec![output], DesktopRect::new(0, 0, 2, 2).unwrap()))
    }

    fn open(&self, output_id: &str) -> CaptureResult<Box<dyn CaptureSession>> {
        if output_id != "screen-one" {
            return Err(CaptureError::new(
                "output_unavailable",
                "missing test output",
            ));
        }
        self.opened.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(FakeSession {
            dropped: self.dropped.clone(),
        }))
    }
}

struct FakeSession {
    dropped: Arc<AtomicUsize>,
}

impl CaptureSession for FakeSession {
    fn read(&mut self) -> CaptureResult<Frame> {
        Ok(Frame {
            width: 2,
            height: 2,
            stride: 8,
            timestamp_us: 0,
            sequence: 0,
            bytes: vec![0; 16],
        })
    }

    fn geometry(&self) -> DesktopGeometry {
        DesktopGeometry {
            capture: CaptureGeometry::new(
                DesktopRect::new(0, 0, 2, 2).unwrap(),
                DesktopRect::new(0, 0, 2, 2).unwrap(),
            )
            .unwrap(),
        }
    }
}

impl Drop for FakeSession {
    fn drop(&mut self) {
        self.dropped.fetch_add(1, Ordering::SeqCst);
    }
}

fn output(id: &str) -> Output {
    Output {
        id: id.into(),
        adapter: "test".into(),
        adapter_vendor_id: None,
        adapter_device_id: None,
        adapter_description: None,
        device_name: "test-screen".into(),
        attached: true,
        left: 0,
        top: 0,
        width: 2,
        height: 2,
        rotation: 1,
    }
}

fn controller(enabled: bool) -> (Controller<FakeBackend>, Arc<AtomicUsize>, Arc<AtomicUsize>) {
    let opened = Arc::new(AtomicUsize::new(0));
    let dropped = Arc::new(AtomicUsize::new(0));
    (
        Controller::new(
            FakeBackend {
                opened: opened.clone(),
                dropped: dropped.clone(),
            },
            enabled,
        ),
        opened,
        dropped,
    )
}

fn open(controller: &mut Controller<FakeBackend>, caller: &str, operation_id: &str) -> u64 {
    let reply = controller.handle(
        caller,
        Operation::Open {
            output: "screen-one".into(),
            operation_id: Some(operation_id.into()),
        },
        &mut || Ok(()),
    );
    assert_eq!(reply["ok"], true, "{reply}");
    reply["stream"].as_u64().unwrap()
}

fn module(controller: Controller<FakeBackend>) -> super::CaptureModule {
    use amaterasu_module_sdk::{RuntimeClient, abi::*};
    use std::{ffi::c_void, sync::Mutex};

    unsafe extern "C" fn unused_host(
        _context: *mut c_void,
        _request: BorrowedBytes,
        _reply: ReplySink,
    ) -> Status {
        STATUS_INTERNAL_ERROR
    }

    let controller = Arc::new(Mutex::new(
        controller.with_backend(super::backend::NativeBackend::new()),
    ));
    let factory = super::native::Factory::new(controller.clone());
    // SAFETY: the stateless endpoint remains valid for this fixture. Shutdown
    // must not dispatch runtime operations or open any native display.
    let runtime = unsafe {
        RuntimeClient::from_endpoint(HostApi {
            header: AbiHeader::of::<HostApi>(),
            context: std::ptr::null_mut(),
            invoke: unused_host,
        })
    };
    super::CaptureModule {
        runtime,
        controller,
        _factory: factory,
        shutting_down: false,
    }
}

#[test]
fn module_quiescence_releases_capture_before_reporting_ready() {
    use amaterasu_module_sdk::ManagedModule;

    let (mut controller, opened, dropped) = controller(true);
    open(&mut controller, "console.one", "one");
    open(&mut controller, "console.two", "two");
    let mut module = module(controller);

    assert_eq!(opened.load(Ordering::SeqCst), 2);
    assert_eq!(dropped.load(Ordering::SeqCst), 0);
    assert_eq!(module.quiesce(), Ok(true));
    assert_eq!(dropped.load(Ordering::SeqCst), 2);
    assert_eq!(module.quiesce(), Ok(true));
    assert_eq!(dropped.load(Ordering::SeqCst), 2);
}

#[test]
fn shutdown_announcement_preserves_capture_until_consumers_drain() {
    use amaterasu_module_sdk::ManagedModule;

    let (mut controller, _, dropped) = controller(true);
    let stream = open(&mut controller, "console.one", "one");
    let mut module = module(controller);

    module.prepare_shutdown(20_000).unwrap();
    assert_eq!(dropped.load(Ordering::SeqCst), 0);
    assert!(
        module
            .controller
            .lock()
            .unwrap()
            .read("console.one", stream)
            .is_ok()
    );

    let shared = module.controller.clone();
    let held = shared.lock().unwrap();
    assert_eq!(module.quiesce(), Ok(false));
    assert_eq!(dropped.load(Ordering::SeqCst), 0);
    drop(held);
    assert_eq!(module.quiesce(), Ok(true));
    assert_eq!(dropped.load(Ordering::SeqCst), 1);
}

#[test]
fn tick_defers_polling_while_native_capture_holds_controller() {
    use amaterasu_module_sdk::ManagedModule;

    let (controller, _, _) = controller(true);
    let mut module = module(controller);
    let shared = module.controller.clone();
    let held = shared.lock().unwrap();
    // The fixture's runtime rejects every call, so success also proves that
    // Poll was not sent while a native reader held the controller.
    assert_eq!(module.tick(), Ok(()));
    assert_eq!(module.quiesce(), Ok(false));
    drop(held);
    assert!(module.tick().is_err());
    assert_eq!(module.quiesce(), Ok(true));
}

#[test]
fn stream_leases_are_caller_scoped_for_read_lookup_and_close() {
    let (mut controller, opened, _) = controller(true);
    let stream = open(&mut controller, "console.one", "launch.one");
    assert_eq!(opened.load(Ordering::SeqCst), 1);
    let lookup = controller.handle(
        "console.two",
        Operation::Lookup {
            operation_id: "launch.one".into(),
        },
        &mut || Ok(()),
    );
    assert_eq!(lookup["error"]["code"], "not_found");
    assert_eq!(
        controller.read("console.two", stream).unwrap_err().code,
        "not_found"
    );
    assert_eq!(
        controller.close("console.two", stream).unwrap_err().code,
        "not_found"
    );
    let frame = controller.read("console.one", stream).unwrap();
    assert_eq!((frame.width, frame.height, frame.sequence), (2, 2, 1));
    controller.close("console.one", stream).unwrap();
    assert_eq!(
        controller.read("console.one", stream).unwrap_err().code,
        "not_found"
    );
}

#[test]
fn same_operation_is_idempotent_but_cannot_change_output() {
    let (mut controller, opened, _) = controller(true);
    let first = open(&mut controller, "console.one", "launch.one");
    let repeated = open(&mut controller, "console.one", "launch.one");
    assert_eq!(first, repeated);
    assert_eq!(opened.load(Ordering::SeqCst), 1);
    let conflict = controller.handle(
        "console.one",
        Operation::Open {
            output: "screen-two".into(),
            operation_id: Some("launch.one".into()),
        },
        &mut || Ok(()),
    );
    assert_eq!(conflict["error"]["code"], "operation_conflict");
}

#[test]
fn cancellation_after_native_open_drops_uncommitted_session() {
    let (mut controller, opened, dropped) = controller(true);
    let mut checks = 0;
    let reply = controller.handle(
        "console.one",
        Operation::Open {
            output: "screen-one".into(),
            operation_id: Some("launch.one".into()),
        },
        &mut || {
            checks += 1;
            if checks == 1 {
                Ok(())
            } else {
                Err(CaptureError::new("cancelled", "request ended"))
            }
        },
    );
    assert_eq!(reply["error"]["code"], "cancelled");
    assert_eq!(opened.load(Ordering::SeqCst), 1);
    assert_eq!(dropped.load(Ordering::SeqCst), 1);
    assert!(controller.take_opened_stream().is_none());
}

#[test]
fn disabled_policy_rejects_open_without_allocating_resources() {
    let (mut controller, opened, _) = controller(false);
    let reply = controller.handle(
        "console.one",
        Operation::Open {
            output: "screen-one".into(),
            operation_id: None,
        },
        &mut || Ok(()),
    );
    assert_eq!(reply["error"]["code"], "capture_disabled");
    assert_eq!(opened.load(Ordering::SeqCst), 0);
}
