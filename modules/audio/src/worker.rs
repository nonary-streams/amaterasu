//! Worker-owned COM capture feeds a bounded queue; no COM interface is Send.
use crate::{
    model::{AudioResult, Endpoint, Format, Health, Packet},
    packet::Continuity,
    platform,
};
use amaterasu_media::AUDIO_DISCONTINUITY;
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

pub const QUEUE_PACKETS: usize = 16;
const POLL_INTERVAL: Duration = Duration::from_millis(2);
const RECOVERY_RETRY_DELAY: Duration = Duration::from_secs(5);

struct RecoveryPolicy {
    expected_endpoint: String,
    expected_format: Format,
    retry_delay: Duration,
}

pub trait Source: Send {
    fn read(&mut self) -> AudioResult<Option<Packet>>;
    fn health(&self) -> Health;
}

trait CaptureDevice {
    fn endpoint(&self) -> &str;
    fn format(&self) -> Format;
    fn read(&mut self) -> AudioResult<Option<Packet>>;
}

impl CaptureDevice for platform::Capture {
    fn endpoint(&self) -> &str {
        platform::Capture::endpoint(self)
    }

    fn format(&self) -> Format {
        platform::Capture::format(self)
    }

    fn read(&mut self) -> AudioResult<Option<Packet>> {
        platform::Capture::read(self)
    }
}

pub struct Opened {
    pub source: Box<dyn Source>,
    pub endpoint: String,
    pub format: Format,
}

pub trait Backend: Send {
    fn enumerate(&self) -> AudioResult<Vec<Endpoint>>;
    fn open(
        &self,
        endpoint: &str,
        virtual_sink: Option<&str>,
        channels: Option<u32>,
        host_audio_enabled: bool,
        capture_sink_without_routing: bool,
    ) -> AudioResult<Opened>;
}

pub struct NativeBackend;
impl Backend for NativeBackend {
    fn enumerate(&self) -> AudioResult<Vec<Endpoint>> {
        enumerate()
    }

    fn open(
        &self,
        endpoint: &str,
        virtual_sink: Option<&str>,
        channels: Option<u32>,
        host_audio_enabled: bool,
        capture_sink_without_routing: bool,
    ) -> AudioResult<Opened> {
        let (source, endpoint, format) = Worker::open(
            endpoint.to_owned(),
            virtual_sink.map(str::to_owned),
            channels,
            host_audio_enabled,
            capture_sink_without_routing,
        )?;
        Ok(Opened {
            source: Box::new(source),
            endpoint,
            format,
        })
    }
}

pub struct Worker {
    packets: Receiver<Packet>,
    health: Arc<Mutex<Health>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Worker {
    pub fn open(
        endpoint: String,
        virtual_sink: Option<String>,
        channels: Option<u32>,
        host_audio_enabled: bool,
        capture_sink_without_routing: bool,
    ) -> AudioResult<(Self, String, Format)> {
        let (sender, packets) = mpsc::sync_channel(QUEUE_PACKETS);
        let (ready, initialized) = mpsc::sync_channel(1);
        let health = Arc::new(Mutex::new(Health::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let thread_health = health.clone();
        let thread_stop = stop.clone();
        let thread = thread::Builder::new()
            .name("amaterasu-loopback".into())
            .spawn(move || {
                let capture = match platform::Capture::open(
                    &endpoint,
                    virtual_sink.as_deref(),
                    channels,
                    host_audio_enabled,
                    capture_sink_without_routing,
                ) {
                    Ok(capture) => capture,
                    Err(error) => {
                        let _ = ready.send(Err(error));
                        return;
                    }
                };
                if ready
                    .send(Ok((capture.endpoint().to_owned(), capture.format())))
                    .is_err()
                {
                    return;
                }
                let endpoint = capture.endpoint().to_owned();
                let format = capture.format();
                let recovery_config = capture.recovery_config();
                run_capture_loop(
                    Box::new(capture),
                    move || {
                        platform::Capture::reopen(&recovery_config)
                            .map(|capture| Box::new(capture) as Box<dyn CaptureDevice>)
                    },
                    &sender,
                    &thread_health,
                    &thread_stop,
                    RecoveryPolicy {
                        expected_endpoint: endpoint,
                        expected_format: format,
                        retry_delay: RECOVERY_RETRY_DELAY,
                    },
                );
            })
            .map_err(|_| "audio_worker_failed")?;
        // Never detach DLL code still running in a COM activation. Initialization
        // and teardown may wait on a vendor driver; external host supervision is
        // the boundary for a driver that never returns.
        match initialized
            .recv()
            .map_err(|_| "audio_worker_failed")
            .and_then(|value| value)
        {
            Ok((endpoint, format)) => Ok((
                Self {
                    packets,
                    health,
                    stop,
                    thread: Some(thread),
                },
                endpoint,
                format,
            )),
            Err(error) => {
                stop.store(true, Ordering::Release);
                let _ = thread.join();
                Err(error)
            }
        }
    }
}

impl Source for Worker {
    fn read(&mut self) -> AudioResult<Option<Packet>> {
        if let Some(error) = self.health().failure {
            return Err(error);
        }
        match self.packets.try_recv() {
            Ok(packet) => Ok(Some(packet)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err("audio_worker_stopped"),
        }
    }

    fn health(&self) -> Health {
        self.health
            .lock()
            .map(|health| health.clone())
            .unwrap_or(Health {
                failure: Some("audio_worker_failed"),
                ..Health::default()
            })
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            thread.thread().unpark();
            let _ = thread.join();
        }
    }
}

fn fail(health: &Mutex<Health>, error: &'static str) {
    if let Ok(mut health) = health.lock() {
        health.failure = Some(error);
        health.recovering = false;
    }
}

fn set_recovering(health: &Mutex<Health>, recovering: bool) {
    if let Ok(mut health) = health.lock()
        && health.failure.is_none()
    {
        health.recovering = recovering;
    }
}

fn run_capture_loop<F>(
    mut capture: Box<dyn CaptureDevice>,
    mut reopen: F,
    sender: &SyncSender<Packet>,
    health: &Mutex<Health>,
    stop: &AtomicBool,
    recovery_policy: RecoveryPolicy,
) where
    F: FnMut() -> AudioResult<Box<dyn CaptureDevice>>,
{
    let mut continuity = Continuity::new();
    while !stop.load(Ordering::Acquire) {
        match capture.read() {
            Ok(Some(mut packet)) => {
                let recovered_packet = match continuity.normalize(&mut packet) {
                    Ok(recovered_packet) => recovered_packet,
                    Err(error) => {
                        fail(health, error);
                        break;
                    }
                };
                if recovered_packet {
                    set_recovering(health, false);
                }
                if !enqueue(sender, health, packet) {
                    break;
                }
            }
            Ok(None) => thread::park_timeout(POLL_INTERVAL),
            Err("device_lost") => {
                set_recovering(health, true);
                continuity.begin_recovery();
                drop(capture);
                loop {
                    if !wait_for_retry(stop, recovery_policy.retry_delay) {
                        return;
                    }
                    match reopen() {
                        Ok(reopened) => {
                            if reopened.endpoint() != recovery_policy.expected_endpoint {
                                fail(health, "audio_endpoint_changed");
                                return;
                            }
                            if reopened.format() != recovery_policy.expected_format {
                                fail(health, "audio_format_changed");
                                return;
                            }
                            capture = reopened;
                            break;
                        }
                        Err(error) if error == "audio_endpoint_changed" => {
                            fail(health, error);
                            return;
                        }
                        Err(_) if stop.load(Ordering::Acquire) => return,
                        Err(_) => {}
                    }
                }
            }
            Err(error) => {
                fail(health, error);
                break;
            }
        }
    }
}

fn wait_for_retry(stop: &AtomicBool, delay: Duration) -> bool {
    let started = Instant::now();
    loop {
        if stop.load(Ordering::Acquire) {
            return false;
        }
        let remaining = delay.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return true;
        }
        thread::park_timeout(remaining);
    }
}

fn enqueue(sender: &SyncSender<Packet>, health: &Mutex<Health>, packet: Packet) -> bool {
    if packet.flags & AUDIO_DISCONTINUITY != 0
        && let Ok(mut health) = health.lock()
    {
        health.discontinuities = health.discontinuities.saturating_add(1);
    }
    match sender.try_send(packet) {
        Ok(()) => true,
        Err(TrySendError::Full(_)) => {
            if let Ok(mut health) = health.lock() {
                health.failure = Some("audio_queue_overflow");
                health.dropped_packets = health.dropped_packets.saturating_add(1);
            }
            false
        }
        Err(TrySendError::Disconnected(_)) => false,
    }
}

pub fn enumerate() -> AudioResult<Vec<Endpoint>> {
    thread::Builder::new()
        .name("amaterasu-audio-endpoints".into())
        .spawn(platform::enumerate)
        .map_err(|_| "audio_worker_failed")?
        .join()
        .map_err(|_| "audio_worker_failed")?
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::VecDeque, sync::atomic::AtomicUsize};

    struct FakeCapture {
        endpoint: String,
        format: Format,
        events: VecDeque<AudioResult<Option<Packet>>>,
    }

    impl FakeCapture {
        fn new(
            format: Format,
            events: impl IntoIterator<Item = AudioResult<Option<Packet>>>,
        ) -> Self {
            Self {
                endpoint: "render.fixed".into(),
                format,
                events: events.into_iter().collect(),
            }
        }
    }

    impl CaptureDevice for FakeCapture {
        fn endpoint(&self) -> &str {
            &self.endpoint
        }

        fn format(&self) -> Format {
            self.format
        }

        fn read(&mut self) -> AudioResult<Option<Packet>> {
            self.events.pop_front().unwrap_or(Ok(None))
        }
    }

    fn packet(timestamp_us: u64, sequence: u64) -> Packet {
        Packet {
            frames: 1,
            flags: 0,
            bytes: vec![0; 4],
            timestamp_us,
            sequence,
        }
    }

    #[test]
    fn full_queue_is_terminal_visible_and_preserves_queued_packets() {
        let (sender, receiver) = mpsc::sync_channel(1);
        let health = Mutex::new(Health::default());
        let packet = |sequence| Packet {
            frames: 1,
            flags: AUDIO_DISCONTINUITY,
            bytes: vec![0; 4],
            timestamp_us: sequence,
            sequence,
        };
        assert!(enqueue(&sender, &health, packet(0)));
        assert!(!enqueue(&sender, &health, packet(1)));
        let health = health.lock().unwrap();
        assert_eq!(health.failure, Some("audio_queue_overflow"));
        assert_eq!(health.dropped_packets, 1);
        assert_eq!(health.discontinuities, 2);
        assert_eq!(receiver.try_recv().unwrap().sequence, 0);
    }

    #[test]
    fn device_loss_retries_open_and_preserves_packet_clock() {
        let format = Format::new(48_000, 1, amaterasu_media::PCM_F32).unwrap();
        let initial = FakeCapture::new(format, [Ok(Some(packet(1_000, 0))), Err("device_lost")]);
        let (sender, packets) = mpsc::sync_channel(4);
        let health = Arc::new(Mutex::new(Health::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let attempts = Arc::new(AtomicUsize::new(0));

        let runner = thread::spawn({
            let health = health.clone();
            let stop = stop.clone();
            let attempts = attempts.clone();
            move || {
                let mut recovery_attempt = 0;
                run_capture_loop(
                    Box::new(initial),
                    move || {
                        attempts.fetch_add(1, Ordering::SeqCst);
                        recovery_attempt += 1;
                        if recovery_attempt == 1 {
                            return Err("endpoint_not_found");
                        }
                        Ok(Box::new(FakeCapture::new(format, [Ok(Some(packet(0, 0)))]))
                            as Box<dyn CaptureDevice>)
                    },
                    &sender,
                    &health,
                    &stop,
                    RecoveryPolicy {
                        expected_endpoint: "render.fixed".into(),
                        expected_format: format,
                        retry_delay: Duration::ZERO,
                    },
                );
            }
        });

        let first = packets.recv_timeout(Duration::from_secs(1)).unwrap();
        let recovered = packets.recv_timeout(Duration::from_secs(1)).unwrap();
        stop.store(true, Ordering::Release);
        runner.thread().unpark();
        runner.join().unwrap();

        assert_eq!((first.sequence, first.timestamp_us), (0, 1_000));
        assert_eq!(recovered.sequence, 1);
        assert!(recovered.timestamp_us > first.timestamp_us);
        assert_ne!(recovered.flags & AUDIO_DISCONTINUITY, 0);
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        let health = health.lock().unwrap();
        assert!(!health.recovering);
        assert_eq!(health.failure, None);
        assert_eq!(health.discontinuities, 1);
    }

    #[test]
    fn recovery_wait_is_promptly_cancelled_by_shutdown() {
        let format = Format::new(48_000, 1, amaterasu_media::PCM_F32).unwrap();
        let initial = FakeCapture::new(format, [Err("device_lost")]);
        let (sender, _packets) = mpsc::sync_channel(1);
        let health = Arc::new(Mutex::new(Health::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let attempts = Arc::new(AtomicUsize::new(0));
        let runner = thread::spawn({
            let health = health.clone();
            let stop = stop.clone();
            let attempts = attempts.clone();
            move || {
                run_capture_loop(
                    Box::new(initial),
                    move || {
                        attempts.fetch_add(1, Ordering::SeqCst);
                        Err("endpoint_not_found")
                    },
                    &sender,
                    &health,
                    &stop,
                    RecoveryPolicy {
                        expected_endpoint: "render.fixed".into(),
                        expected_format: format,
                        retry_delay: Duration::from_secs(30),
                    },
                );
            }
        });

        let deadline = Instant::now() + Duration::from_secs(1);
        while !health.lock().unwrap().recovering && Instant::now() < deadline {
            thread::yield_now();
        }
        assert!(health.lock().unwrap().recovering);
        stop.store(true, Ordering::Release);
        runner.thread().unpark();
        runner.join().unwrap();
        assert_eq!(attempts.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn incompatible_reopened_format_is_terminal() {
        let format = Format::new(48_000, 1, amaterasu_media::PCM_F32).unwrap();
        let incompatible = Format::new(48_000, 1, amaterasu_media::PCM_I16).unwrap();
        let initial = FakeCapture::new(format, [Err("device_lost")]);
        let (sender, _packets) = mpsc::sync_channel(1);
        let health = Mutex::new(Health::default());
        let stop = AtomicBool::new(false);
        run_capture_loop(
            Box::new(initial),
            || Ok(Box::new(FakeCapture::new(incompatible, [])) as Box<dyn CaptureDevice>),
            &sender,
            &health,
            &stop,
            RecoveryPolicy {
                expected_endpoint: "render.fixed".into(),
                expected_format: format,
                retry_delay: Duration::ZERO,
            },
        );
        let health = health.lock().unwrap();
        assert_eq!(health.failure, Some("audio_format_changed"));
        assert!(!health.recovering);
    }
}
