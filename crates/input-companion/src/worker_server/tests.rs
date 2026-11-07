use super::*;
use crate::{GrantRegistry, InputEvent, PROTOCOL_VERSION};
use std::collections::VecDeque;
use std::sync::{Arc, Condvar, atomic::AtomicUsize, mpsc};

#[derive(Default)]
struct NativeState {
    events: Vec<InputEvent>,
    alive: bool,
    releases: usize,
    heartbeats: usize,
    fail_release: bool,
    order: Vec<&'static str>,
}

struct SyntheticSink(Arc<Mutex<NativeState>>);

impl DeviceSink for SyntheticSink {
    fn submit(&mut self, event: &InputEvent) -> InputResult<()> {
        self.0.lock().unwrap().events.push(event.clone());
        Ok(())
    }

    fn release_all(&mut self) -> InputResult<()> {
        let mut native = self.0.lock().unwrap();
        native.releases += 1;
        if native.fail_release {
            return Err(InputError::Device);
        }
        native.alive = false;
        native.order.push("native_retired");
        Ok(())
    }

    fn keep_alive(&mut self) -> InputResult<()> {
        self.0.lock().unwrap().heartbeats += 1;
        Ok(())
    }
}

impl Drop for SyntheticSink {
    fn drop(&mut self) {
        self.0.lock().unwrap().order.push("sink_dropped");
    }
}

struct ChannelState {
    replies: Vec<Vec<u8>>,
    capacity: usize,
    blocked: bool,
    force_release: bool,
}

struct SyntheticConnection {
    process: ProcessIdentity,
    incoming: VecDeque<Vec<u8>>,
    channel: Arc<(Mutex<ChannelState>, Condvar)>,
    native: Arc<Mutex<NativeState>>,
    on_read: Option<Box<dyn FnOnce() + Send>>,
    read_count: usize,
    stop_after_reads: Option<usize>,
}

impl WorkerConnection for SyntheticConnection {
    fn authenticated_peer(&self) -> InputResult<ProcessIdentity> {
        Ok(self.process.clone())
    }

    fn read_frame(&mut self, deadline: Instant, stop: &AtomicBool) -> InputResult<Option<Vec<u8>>> {
        if let Some(callback) = self.on_read.take() {
            callback();
        }
        self.read_count += 1;
        if self.stop_after_reads == Some(self.read_count) {
            stop.store(true, Ordering::Release);
        }
        if let Some(frame) = self.incoming.pop_front() {
            return Ok(Some(frame));
        }
        let (lock, changed) = &*self.channel;
        let mut channel = lock.lock().unwrap();
        while !stop.load(Ordering::Acquire) {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            channel = changed
                .wait_timeout(channel, remaining.min(Duration::from_millis(10)))
                .unwrap()
                .0;
        }
        Ok(None)
    }

    fn write_frame(
        &mut self,
        frame: &[u8],
        deadline: Instant,
        stop: &AtomicBool,
    ) -> InputResult<()> {
        let (lock, changed) = &*self.channel;
        let mut channel = lock.lock().unwrap();
        while channel.replies.len() >= channel.capacity && !channel.force_release {
            channel.blocked = true;
            changed.notify_all();
            if stop.load(Ordering::Acquire) {
                return Err(InputError::Closed);
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(InputError::Device);
            }
            channel = changed
                .wait_timeout(channel, remaining.min(Duration::from_millis(10)))
                .unwrap()
                .0;
        }
        if stop.load(Ordering::Acquire) {
            return Err(InputError::Closed);
        }
        channel.replies.push(frame.to_vec());
        Ok(())
    }
}

impl Drop for SyntheticConnection {
    fn drop(&mut self) {
        self.native.lock().unwrap().order.push("transport_dropped");
    }
}

struct Fixture {
    process: ProcessIdentity,
    ledger: Arc<Mutex<PermitLedger>>,
    generation: u64,
    token: String,
    session: InputSession<SyntheticSink>,
    connection: SyntheticConnection,
    stop: Arc<AtomicBool>,
    cleanup_failed: Arc<AtomicBool>,
    wake_count: Arc<AtomicUsize>,
    sequence: u64,
}

#[derive(Clone)]
struct Observation {
    process: ProcessIdentity,
    ledger: Arc<Mutex<PermitLedger>>,
    generation: u64,
    token: String,
    native: Arc<Mutex<NativeState>>,
    channel: Arc<(Mutex<ChannelState>, Condvar)>,
    stop: Arc<AtomicBool>,
    cleanup_failed: Arc<AtomicBool>,
    wake_count: Arc<AtomicUsize>,
}

impl Fixture {
    fn new(reply_capacity: usize) -> Self {
        let process = ProcessIdentity::new("S-1-5-21-fixture".into(), 1, 42, 1234).unwrap();
        let mut ledger = PermitLedger::new(GrantRegistry::synthetic());
        ledger
            .refresh_from_host(process.clone(), "a".repeat(64), true, true, Instant::now())
            .unwrap();
        let lease = ledger.claim(&process, Instant::now()).unwrap();
        let native = Arc::new(Mutex::new(NativeState {
            alive: true,
            ..Default::default()
        }));
        let session = InputSession::connect(
            &lease.peer,
            lease.grant,
            SyntheticSink(Arc::clone(&native)),
            Instant::now(),
        )
        .unwrap();
        Self {
            connection: SyntheticConnection {
                process: process.clone(),
                incoming: VecDeque::new(),
                channel: Arc::new((
                    Mutex::new(ChannelState {
                        replies: Vec::new(),
                        capacity: reply_capacity,
                        blocked: false,
                        force_release: false,
                    }),
                    Condvar::new(),
                )),
                native,
                on_read: None,
                read_count: 0,
                stop_after_reads: None,
            },
            process,
            ledger: Arc::new(Mutex::new(ledger)),
            generation: lease.generation,
            token: lease.token,
            session,
            stop: Arc::new(AtomicBool::new(false)),
            cleanup_failed: Arc::new(AtomicBool::new(false)),
            wake_count: Arc::new(AtomicUsize::new(0)),
            sequence: 1,
        }
    }

    fn event(&mut self, event: InputEvent) {
        self.connection.incoming.push_back(
            serde_json::to_vec(&Request::WorkerEvent {
                packet: InputPacket {
                    version: PROTOCOL_VERSION,
                    generation: self.generation,
                    sequence: self.sequence,
                    lease_token: self.token.clone(),
                    event,
                },
            })
            .unwrap(),
        );
        self.sequence += 1;
    }

    fn close(&mut self) {
        self.connection
            .incoming
            .push_back(serde_json::to_vec(&Request::WorkerClose).unwrap());
    }

    fn observe(&self) -> Observation {
        Observation {
            process: self.process.clone(),
            ledger: Arc::clone(&self.ledger),
            generation: self.generation,
            token: self.token.clone(),
            native: Arc::clone(&self.connection.native),
            channel: Arc::clone(&self.connection.channel),
            stop: Arc::clone(&self.stop),
            cleanup_failed: Arc::clone(&self.cleanup_failed),
            wake_count: Arc::clone(&self.wake_count),
        }
    }

    fn run(mut self) -> InputResult<()> {
        let result = serve_worker(
            &mut self.connection,
            self.session,
            WorkerContext {
                process: &self.process,
                ledger: &self.ledger,
                stop: &self.stop,
                cleanup_failed: &self.cleanup_failed,
                generation: self.generation,
                token: &self.token,
            },
            || {
                self.wake_count.fetch_add(1, Ordering::Relaxed);
            },
        );
        drop(self.connection);
        result
    }

    fn spawn(self) -> (std::thread::JoinHandle<()>, mpsc::Receiver<InputResult<()>>) {
        let (finished, completion) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            finished.send(self.run()).unwrap();
        });
        (worker, completion)
    }
}

impl Observation {
    fn wait_blocked(&self) {
        let (lock, changed) = &*self.channel;
        let (state, timeout) = changed
            .wait_timeout_while(lock.lock().unwrap(), Duration::from_secs(2), |state| {
                !state.blocked
            })
            .unwrap();
        assert!(state.blocked && !timeout.timed_out());
    }

    fn request_stop(&self) {
        self.stop.store(true, Ordering::Release);
        self.channel.1.notify_all();
    }

    fn force_release(&self) {
        self.channel.0.lock().unwrap().force_release = true;
        self.channel.1.notify_all();
    }

    fn replies(&self) -> Vec<String> {
        self.channel
            .0
            .lock()
            .unwrap()
            .replies
            .iter()
            .map(|frame| {
                serde_json::from_slice::<serde_json::Value>(frame).unwrap()["kind"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect()
    }

    fn assert_retired(&self) {
        let native = self.native.lock().unwrap();
        assert!(!native.alive);
        assert!(native.releases != 0);
        let sink_drop = native
            .order
            .iter()
            .position(|event| *event == "sink_dropped")
            .unwrap();
        let pipe_drop = native
            .order
            .iter()
            .position(|event| *event == "transport_dropped")
            .unwrap();
        assert!(
            sink_drop < pipe_drop,
            "native sink must retire before transport containment"
        );
        drop(native);
        assert!(!self.ledger.lock().unwrap().is_current_lease(
            &self.process,
            self.generation,
            &self.token,
            Instant::now()
        ));
        assert!(!self.cleanup_failed.load(Ordering::Acquire));
        assert_eq!(self.wake_count.load(Ordering::Relaxed), 0);
    }
}

fn press_key() -> InputEvent {
    InputEvent::Key {
        scan_code: 30,
        extended: false,
        pressed: true,
    }
}

fn legacy_pipeline() -> Fixture {
    let mut fixture = Fixture::new(1);
    fixture.event(press_key());
    fixture.event(InputEvent::Relative { dx: 1, dy: -1 });
    fixture.close();
    fixture
}

#[test]
fn unread_legacy_pipeline_does_not_hold_native_lease_during_service_stop() {
    let fixture = legacy_pipeline();
    let observed = fixture.observe();
    let (worker, completion) = fixture.spawn();
    observed.wait_blocked();
    observed.request_stop();
    let completed_before_release = completion.recv_timeout(Duration::from_millis(500));
    let retired_before_release = !observed.native.lock().unwrap().alive;
    // A failing regression must still release its adapter and retire its thread.
    observed.force_release();
    worker.join().unwrap();
    assert!(
        completed_before_release.is_ok(),
        "blocked reply prevented native cleanup after service stop"
    );
    assert!(retired_before_release);
    observed.assert_retired();
    assert_eq!(observed.replies(), ["granted"]);
}

#[test]
fn unread_legacy_pipeline_times_out_and_retires_without_a_stop_request() {
    let fixture = legacy_pipeline();
    let observed = fixture.observe();
    let (worker, completion) = fixture.spawn();
    observed.wait_blocked();
    let completed = completion.recv_timeout(Duration::from_millis(500));
    observed.force_release();
    worker.join().unwrap();
    assert_eq!(completed.unwrap(), Err(InputError::Device));
    assert!(!observed.stop.load(Ordering::Acquire));
    observed.assert_retired();
    // The already-accepted key is released, but later pipelined input is never run.
    let events = &observed.native.lock().unwrap().events;
    assert_eq!(
        events,
        &[
            press_key(),
            InputEvent::Key {
                scan_code: 30,
                extended: false,
                pressed: false
            }
        ]
    );
    assert_eq!(observed.replies(), ["granted"]);
}

#[test]
fn blocked_close_acknowledgement_already_has_native_retirement_proof() {
    let mut fixture = Fixture::new(1);
    fixture.close();
    let observed = fixture.observe();
    let (worker, completion) = fixture.spawn();
    observed.wait_blocked();
    assert!(!observed.native.lock().unwrap().alive);
    assert!(!observed.ledger.lock().unwrap().is_current_lease(
        &observed.process,
        observed.generation,
        &observed.token,
        Instant::now()
    ));
    observed.request_stop();
    let completed = completion.recv_timeout(Duration::from_millis(500));
    observed.force_release();
    worker.join().unwrap();
    assert!(completed.is_ok());
    observed.assert_retired();
    assert_eq!(observed.replies(), ["granted"]);
}

#[test]
fn native_cleanup_failure_stops_service_and_keeps_exact_lease_fenced() {
    let fixture = legacy_pipeline();
    let observed = fixture.observe();
    observed.native.lock().unwrap().fail_release = true;
    let (worker, completion) = fixture.spawn();
    observed.wait_blocked();
    observed.request_stop();
    let completed = completion.recv_timeout(Duration::from_millis(500));
    observed.force_release();
    worker.join().unwrap();
    assert_eq!(completed.unwrap(), Err(InputError::Device));
    assert!(observed.native.lock().unwrap().alive);
    assert!(observed.cleanup_failed.load(Ordering::Acquire));
    assert_eq!(observed.wake_count.load(Ordering::Relaxed), 1);
    assert!(observed.ledger.lock().unwrap().is_current_lease(
        &observed.process,
        observed.generation,
        &observed.token,
        Instant::now()
    ));
    assert_eq!(observed.replies(), ["granted"]);
}

#[test]
fn passed_handle_identity_cannot_submit_input_or_preserve_its_old_lease() {
    let mut fixture = Fixture::new(8);
    fixture.event(press_key());
    fixture.connection.process.process_id += 1;
    let observed = fixture.observe();
    assert_eq!(fixture.run(), Ok(()));
    observed.assert_retired();
    assert!(observed.native.lock().unwrap().events.is_empty());
    assert_eq!(observed.replies(), ["granted", "denied"]);
}

#[test]
fn revoked_permit_between_read_and_submit_never_reaches_native_sink() {
    let mut fixture = Fixture::new(8);
    fixture.event(press_key());
    let observed = fixture.observe();
    let ledger = Arc::clone(&fixture.ledger);
    let process = fixture.process.clone();
    fixture.connection.on_read = Some(Box::new(move || ledger.lock().unwrap().revoke(&process)));
    assert_eq!(fixture.run(), Ok(()));
    observed.assert_retired();
    assert!(observed.native.lock().unwrap().events.is_empty());
    assert_eq!(observed.replies(), ["granted", "denied"]);
}

#[test]
fn idle_worker_keeps_native_heartbeat_until_service_stop() {
    let mut fixture = Fixture::new(8);
    fixture.connection.stop_after_reads = Some(3);
    let observed = fixture.observe();
    assert_eq!(fixture.run(), Ok(()));
    observed.assert_retired();
    assert_eq!(observed.native.lock().unwrap().heartbeats, 3);
    assert_eq!(observed.replies(), ["granted"]);
}

#[test]
fn already_stopped_service_sends_no_grant_and_retires_newly_owned_native_sink() {
    let fixture = Fixture::new(8);
    let observed = fixture.observe();
    observed.request_stop();
    assert_eq!(fixture.run(), Err(InputError::Closed));
    observed.assert_retired();
    assert!(observed.replies().is_empty());
}

#[test]
fn malformed_or_replayed_input_gets_no_success_acknowledgement() {
    for malformed in [true, false] {
        let mut fixture = Fixture::new(8);
        let frame = if malformed {
            b"not-json".to_vec()
        } else {
            serde_json::to_vec(&Request::WorkerEvent {
                packet: InputPacket {
                    version: PROTOCOL_VERSION,
                    generation: fixture.generation,
                    sequence: 0,
                    lease_token: fixture.token.clone(),
                    event: press_key(),
                },
            })
            .unwrap()
        };
        fixture.connection.incoming.push_back(frame);
        let observed = fixture.observe();
        assert_eq!(fixture.run(), Ok(()));
        observed.assert_retired();
        assert!(observed.native.lock().unwrap().events.is_empty());
        assert_eq!(observed.replies(), ["granted", "denied"]);
    }
}

#[test]
fn permit_revoked_during_native_creation_cannot_issue_a_grant_acknowledgement() {
    let fixture = Fixture::new(8);
    let observed = fixture.observe();
    observed.ledger.lock().unwrap().revoke(&observed.process);
    assert_eq!(fixture.run(), Err(InputError::PermissionDenied));
    observed.assert_retired();
    assert!(observed.native.lock().unwrap().events.is_empty());
    assert_eq!(observed.replies(), ["denied"]);
}

#[test]
fn server_frames_keep_the_installed_v1_wire_schema() {
    let requests = [
        serde_json::json!({"kind": "host_permit", "process_id": 42, "creation_time": 1234,
            "digest": "a".repeat(64), "keyboard": true, "mouse": true}),
        serde_json::json!({"kind": "worker_start"}),
        serde_json::json!({"kind": "worker_event", "packet": { "version": PROTOCOL_VERSION,
            "generation": 1, "sequence": 1, "lease_token": "0".repeat(32),
            "event": {"kind": "key", "scan_code": 30, "extended": false, "pressed": true} }}),
        serde_json::json!({"kind": "worker_close"}),
    ];
    for frame in requests {
        let request = decode_request(&serde_json::to_vec(&frame).unwrap()).unwrap();
        assert_eq!(serde_json::to_value(request).unwrap(), frame);
    }
    for frame in [
        serde_json::json!({"kind": "ok"}),
        serde_json::json!({"kind": "denied"}),
        serde_json::json!({"kind": "granted", "generation": 1, "token": "0".repeat(32)}),
    ] {
        let reply = serde_json::from_value::<Reply>(frame.clone()).unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&encode_reply(&reply).unwrap()).unwrap(),
            frame
        );
    }
}
