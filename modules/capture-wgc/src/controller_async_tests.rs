use super::*;
use crate::model::Output;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::Duration;

struct HeldBackend {
    entered: mpsc::Sender<()>,
    gate: Mutex<mpsc::Receiver<()>>,
    dropped: Arc<AtomicUsize>,
    retired: Arc<AtomicBool>,
}

struct OwnedSession(Arc<AtomicUsize>, Arc<AtomicBool>);

impl CaptureSession for OwnedSession {
    fn read(&mut self, _: u32) -> CaptureResult<Option<Frame>> {
        Ok(None)
    }
    fn quiesce(&mut self) -> CaptureResult<bool> {
        Ok(self.1.load(Ordering::SeqCst))
    }
}

impl Drop for OwnedSession {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

impl Backend for HeldBackend {
    fn supported(&self) -> bool {
        true
    }
    fn enumerate(&self) -> CaptureResult<Vec<Output>> {
        Ok(Vec::new())
    }
    fn open(&self, _: &str) -> CaptureResult<Box<dyn CaptureSession>> {
        self.entered.send(()).unwrap();
        self.gate
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        Ok(Box::new(OwnedSession(
            self.dropped.clone(),
            self.retired.clone(),
        )))
    }
}

#[test]
fn cancelled_startup_keeps_ownership_until_worker_and_session_retire() {
    let (entered, observed) = mpsc::channel();
    let (release, gate) = mpsc::channel();
    let dropped = Arc::new(AtomicUsize::new(0));
    let mut controller = Controller::new(
        HeldBackend {
            entered,
            gate: Mutex::new(gate),
            dropped: dropped.clone(),
            retired: Arc::new(AtomicBool::new(true)),
        },
        true,
    );
    let OpenStart::Pending(token) = controller
        .begin_open("video", "display".into(), Some("op".into()), false, None)
        .unwrap()
    else {
        panic!("expected pending open")
    };
    observed.recv_timeout(Duration::from_secs(1)).unwrap();
    controller.cancel_open(token);
    assert!(!controller.quiesce().unwrap());
    assert_eq!(dropped.load(Ordering::SeqCst), 0);
    release.send(()).unwrap();
    let until = Instant::now() + Duration::from_secs(2);
    while !controller.quiesce().unwrap() {
        assert!(Instant::now() < until);
        std::thread::yield_now();
    }
    assert_eq!(dropped.load(Ordering::SeqCst), 1);
}

#[test]
fn operation_reservation_survives_cancel_until_actual_capture_retirement() {
    let (entered, observed) = mpsc::channel();
    let (release, gate) = mpsc::channel();
    let dropped = Arc::new(AtomicUsize::new(0));
    let retired = Arc::new(AtomicBool::new(false));
    let mut controller = Controller::new(
        HeldBackend {
            entered,
            gate: Mutex::new(gate),
            dropped: dropped.clone(),
            retired: retired.clone(),
        },
        true,
    );
    let OpenStart::Pending(token) = controller
        .begin_open("video", "display".into(), Some("op".into()), false, None)
        .unwrap()
    else {
        panic!("expected pending")
    };
    observed.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(controller.operation_pending("video", "op"));
    assert_eq!(
        controller.lookup("video", "op").unwrap_err().code,
        "cleanup_pending"
    );
    assert_eq!(
        controller
            .begin_open("video", "other".into(), Some("op".into()), false, None)
            .err()
            .unwrap()
            .code,
        "operation_conflict"
    );
    assert_eq!(
        controller
            .begin_open("video", "display".into(), Some("op".into()), false, None)
            .err()
            .unwrap()
            .code,
        "cleanup_pending"
    );
    controller.cancel_open(token);
    release.send(()).unwrap();
    let until = Instant::now() + Duration::from_secs(2);
    let (reply, committed) = loop {
        if let Some(completed) = controller.poll_open(token, true) {
            break completed;
        }
        assert!(Instant::now() < until);
        std::thread::yield_now();
    };
    assert_eq!(reply["error"]["code"], "cancelled");
    assert!(committed.is_none());
    assert!(controller.operation_pending("video", "op"));
    assert!(controller.poll_closing().is_empty());
    assert_eq!(dropped.load(Ordering::SeqCst), 0);
    assert!(controller.operation_pending("video", "op"));
    retired.store(true, Ordering::SeqCst);
    assert!(controller.poll_closing().is_empty());
    assert_eq!(dropped.load(Ordering::SeqCst), 1);
    assert!(!controller.operation_pending("video", "op"));
    assert_eq!(
        controller.lookup("video", "op").unwrap_err().code,
        "not_found"
    );
}

#[test]
fn successful_startup_is_owned_and_replay_returns_the_same_stream() {
    let (entered, observed) = mpsc::channel();
    let (release, gate) = mpsc::channel();
    let dropped = Arc::new(AtomicUsize::new(0));
    let mut controller = Controller::new(
        HeldBackend {
            entered,
            gate: Mutex::new(gate),
            dropped: dropped.clone(),
            retired: Arc::new(AtomicBool::new(true)),
        },
        true,
    );
    let OpenStart::Pending(token) = controller
        .begin_open("video", "display".into(), Some("op".into()), false, None)
        .unwrap()
    else {
        panic!("expected pending")
    };
    observed.recv_timeout(Duration::from_secs(1)).unwrap();
    release.send(()).unwrap();
    let until = Instant::now() + Duration::from_secs(2);
    let (reply, stream) = loop {
        if let Some(completed) = controller.poll_open(token, true) {
            break completed;
        }
        assert!(Instant::now() < until);
        std::thread::yield_now();
    };
    assert_eq!(stream, Some(token));
    assert_eq!(reply["stream"], token);
    let OpenStart::Ready(replay) = controller
        .begin_open("video", "display".into(), Some("op".into()), false, None)
        .unwrap()
    else {
        panic!("expected replay")
    };
    assert_eq!(replay["stream"], token);
    assert_eq!(dropped.load(Ordering::SeqCst), 0);
    controller.close("video", token).unwrap();
    assert_eq!(dropped.load(Ordering::SeqCst), 1);
}

#[test]
fn in_flight_startups_reserve_all_capture_slots_before_native_completion() {
    let (entered, observed) = mpsc::channel();
    let (release, gate) = mpsc::channel();
    let dropped = Arc::new(AtomicUsize::new(0));
    let mut controller = Controller::new(
        HeldBackend {
            entered,
            gate: Mutex::new(gate),
            dropped: dropped.clone(),
            retired: Arc::new(AtomicBool::new(true)),
        },
        true,
    );
    let mut tokens = Vec::new();
    for index in 0..MAX_STREAMS {
        let OpenStart::Pending(token) = controller
            .begin_open(
                "video",
                "display".into(),
                Some(format!("op-{index}")),
                false,
                None,
            )
            .unwrap()
        else {
            panic!("expected pending")
        };
        tokens.push(token);
    }
    for _ in 0..MAX_STREAMS {
        observed.recv_timeout(Duration::from_secs(1)).unwrap();
    }
    assert_eq!(
        controller
            .begin_open(
                "video",
                "display".into(),
                Some("overflow".into()),
                false,
                None
            )
            .err()
            .unwrap()
            .code,
        "resource_exhausted"
    );
    for token in &tokens {
        controller.cancel_open(*token);
    }
    for _ in 0..MAX_STREAMS {
        release.send(()).unwrap();
    }
    let until = Instant::now() + Duration::from_secs(2);
    while !controller.quiesce().unwrap() {
        assert!(Instant::now() < until);
        std::thread::yield_now();
    }
    assert_eq!(dropped.load(Ordering::SeqCst), MAX_STREAMS);
}

struct PanickingBackend;

impl Backend for PanickingBackend {
    fn supported(&self) -> bool {
        true
    }
    fn enumerate(&self) -> CaptureResult<Vec<Output>> {
        Ok(Vec::new())
    }
    fn open(&self, _: &str) -> CaptureResult<Box<dyn CaptureSession>> {
        panic!("unproven native cleanup")
    }
}

struct UncertainBackend;

impl Backend for UncertainBackend {
    fn supported(&self) -> bool {
        true
    }
    fn enumerate(&self) -> CaptureResult<Vec<Output>> {
        Ok(Vec::new())
    }
    fn open(&self, _: &str) -> CaptureResult<Box<dyn CaptureSession>> {
        Err(CaptureError::new(
            "retirement_uncertain",
            "nested worker panicked",
        ))
    }
}

#[test]
fn nested_worker_retirement_uncertainty_is_not_mistaken_for_absence() {
    let mut controller = Controller::new(UncertainBackend, true);
    let OpenStart::Pending(token) = controller
        .begin_open("video", "display".into(), Some("op".into()), false, None)
        .unwrap()
    else {
        panic!("expected pending")
    };
    let until = Instant::now() + Duration::from_secs(2);
    let (reply, _) = loop {
        if let Some(completed) = controller.poll_open(token, true) {
            break completed;
        }
        assert!(Instant::now() < until);
        std::thread::yield_now();
    };
    assert_eq!(reply["error"]["code"], "retirement_uncertain");
    assert_eq!(
        controller.lookup("video", "op").unwrap_err().code,
        "cleanup_failed"
    );
    assert_eq!(controller.quiesce().unwrap_err().code, "cleanup_failed");
}

#[test]
fn startup_panic_permanently_prevents_false_quiescence() {
    let mut controller = Controller::new(PanickingBackend, true);
    let OpenStart::Pending(token) = controller
        .begin_open("video", "display".into(), Some("op".into()), false, None)
        .unwrap()
    else {
        panic!("expected pending")
    };
    let until = Instant::now() + Duration::from_secs(2);
    loop {
        if let Some((reply, _)) = controller.poll_open(token, true) {
            assert_eq!(reply["error"]["code"], "capture_unavailable");
            break;
        }
        assert!(Instant::now() < until);
        std::thread::yield_now();
    }
    assert_eq!(controller.quiesce().unwrap_err().code, "cleanup_failed");
    assert_eq!(controller.quiesce().unwrap_err().code, "cleanup_failed");
    assert_eq!(
        controller.lookup("video", "op").unwrap_err().code,
        "cleanup_failed"
    );
}

struct InvalidGeometryBackend {
    dropped: Arc<AtomicUsize>,
    retired: Arc<AtomicBool>,
}

struct InvalidGeometrySession(Arc<AtomicUsize>, Arc<AtomicBool>);

impl CaptureSession for InvalidGeometrySession {
    fn read(&mut self, _: u32) -> CaptureResult<Option<Frame>> {
        Ok(None)
    }
    fn geometry(&mut self) -> CaptureResult<Option<CaptureGeometry>> {
        Err(CaptureError::new("backend_error", "geometry failed"))
    }
    fn quiesce(&mut self) -> CaptureResult<bool> {
        Ok(self.1.load(Ordering::SeqCst))
    }
}

impl Drop for InvalidGeometrySession {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

impl Backend for InvalidGeometryBackend {
    fn supported(&self) -> bool {
        true
    }
    fn enumerate(&self) -> CaptureResult<Vec<Output>> {
        Ok(Vec::new())
    }
    fn open(&self, _: &str) -> CaptureResult<Box<dyn CaptureSession>> {
        Ok(Box::new(InvalidGeometrySession(
            self.dropped.clone(),
            self.retired.clone(),
        )))
    }
}

#[test]
fn failed_initialization_retains_session_and_operation_until_real_retirement() {
    let dropped = Arc::new(AtomicUsize::new(0));
    let retired = Arc::new(AtomicBool::new(false));
    let mut controller = Controller::new(
        InvalidGeometryBackend {
            dropped: dropped.clone(),
            retired: retired.clone(),
        },
        true,
    );
    let OpenStart::Pending(token) = controller
        .begin_open("video", "display".into(), Some("op".into()), false, None)
        .unwrap()
    else {
        panic!("expected pending")
    };
    let until = Instant::now() + Duration::from_secs(2);
    let (reply, stream) = loop {
        if let Some(completed) = controller.poll_open(token, true) {
            break completed;
        }
        assert!(Instant::now() < until);
        std::thread::yield_now();
    };
    assert_eq!(reply["error"]["code"], "backend_error");
    assert!(stream.is_none());
    assert!(controller.operation_pending("video", "op"));
    assert!(controller.poll_closing().is_empty());
    assert_eq!(dropped.load(Ordering::SeqCst), 0);
    retired.store(true, Ordering::SeqCst);
    assert!(controller.poll_closing().is_empty());
    assert_eq!(dropped.load(Ordering::SeqCst), 1);
    assert!(!controller.operation_pending("video", "op"));
}
