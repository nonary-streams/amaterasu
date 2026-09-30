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
