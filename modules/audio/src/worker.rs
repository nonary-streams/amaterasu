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
    follow_console_default: bool,
    retry_delay: Duration,
}

pub trait Source: Send {
    /// Return queued packets before a terminal error; once drained, report
    /// the error here even if health has already marked the source failed.
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
                let follow_console_default = recovery_config.follow_console_default;
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
                        follow_console_default,
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
        match self.packets.try_recv() {
            Ok(packet) => Ok(Some(packet)),
            Err(state) => {
                // Capture may fail after it has already queued valid PCM. Drain
                // those packets in order before exposing the terminal failure.
                if let Some(error) = self.health().failure {
                    return Err(error);
                }
                match state {
                    TryRecvError::Empty => Ok(None),
                    TryRecvError::Disconnected => Err("audio_worker_stopped"),
                }
            }
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

fn set_active_endpoint(health: &Mutex<Health>, endpoint: &str) {
    if let Ok(mut health) = health.lock() {
        health.active_endpoint = Some(endpoint.to_owned());
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
    // A capture that opens but fails before delivering PCM has not recovered.
    // Keep the retry delay across those immediate losses to avoid spinning on
    // an endpoint that accepts activation but cannot capture yet.
    let mut recovery_without_packet = false;
    set_active_endpoint(health, capture.endpoint());
    while !stop.load(Ordering::Acquire) {
        match capture.read() {
            Ok(Some(mut packet)) => {
                if let Err(error) = continuity.normalize(&mut packet) {
                    fail(health, error);
                    break;
                }
                if !enqueue(sender, health, packet) {
                    break;
                }
                // Activation alone does not prove a reopened device can
                // capture. Keep reporting recovery until PCM enters the queue.
                if recovery_without_packet {
                    set_recovering(health, false);
                }
                recovery_without_packet = false;
            }
            Ok(None) => thread::park_timeout(POLL_INTERVAL),
            Err(error)
                if error == "device_lost"
                    || (error == "default_endpoint_changed"
                        && recovery_policy.follow_console_default) =>
            {
                set_recovering(health, true);
                continuity.begin_recovery();
                drop(capture);
                let mut retry_immediately = !recovery_without_packet;
                recovery_without_packet = true;
                loop {
                    if stop.load(Ordering::Acquire) {
                        return;
                    }
                    if !retry_immediately && !wait_for_retry(stop, recovery_policy.retry_delay) {
                        return;
                    }
                    retry_immediately = false;
                    match reopen() {
                        Ok(reopened) => {
                            if !recovery_policy.follow_console_default
                                && reopened.endpoint() != recovery_policy.expected_endpoint
                            {
                                fail(health, "audio_endpoint_changed");
                                return;
                            }
                            if reopened.format() != recovery_policy.expected_format {
                                fail(health, "audio_format_changed");
                                return;
                            }
                            if recovery_policy.follow_console_default {
                                set_active_endpoint(health, reopened.endpoint());
                            }
                            capture = reopened;
                            break;
                        }
                        Err(error) if error == "audio_endpoint_changed" => {
                            fail(health, error);
                            return;
                        }
                        Err(error) if permanent_reopen_error(error) => {
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

fn permanent_reopen_error(error: &str) -> bool {
    matches!(
        error,
        "invalid_endpoint"
            | "not_render_endpoint"
            | "invalid_channel_layout"
            | "unsupported_channel_layout"
            | "unsupported_format"
            | "virtual_sink_format_unavailable"
    )
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
    let discontinuity = packet.flags & AUDIO_DISCONTINUITY != 0;
    match sender.try_send(packet) {
        Ok(()) => {
            if discontinuity && let Ok(mut health) = health.lock() {
                health.discontinuities = health.discontinuities.saturating_add(1);
            }
            true
        }
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
        drops: Option<Arc<AtomicUsize>>,
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
                drops: None,
            }
        }

        fn with_endpoint(mut self, endpoint: &str) -> Self {
            self.endpoint = endpoint.into();
            self
        }

        fn with_drop_counter(mut self, drops: Arc<AtomicUsize>) -> Self {
            self.drops = Some(drops);
            self
        }
    }

    impl Drop for FakeCapture {
        fn drop(&mut self) {
            if let Some(drops) = &self.drops {
                drops.fetch_add(1, Ordering::SeqCst);
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

    struct SignaledCapture {
        format: Format,
        events: Receiver<AudioResult<Option<Packet>>>,
        drops: Arc<AtomicUsize>,
    }

    impl Drop for SignaledCapture {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::SeqCst);
        }
    }

    impl CaptureDevice for SignaledCapture {
        fn endpoint(&self) -> &str {
            "render.fixed"
        }

        fn format(&self) -> Format {
            self.format
        }

        fn read(&mut self) -> AudioResult<Option<Packet>> {
            self.events.try_recv().unwrap_or(Ok(None))
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
        assert_eq!(health.discontinuities, 1);
        assert_eq!(receiver.try_recv().unwrap().sequence, 0);
    }

    #[test]
    fn overflow_drains_accepted_pcm_without_counting_dropped_discontinuity() {
        let format = Format::new(48_000, 1, amaterasu_media::PCM_F32).unwrap();
        let drops = Arc::new(AtomicUsize::new(0));
        let mut accepted = packet(1_000, 0);
        accepted.flags = AUDIO_DISCONTINUITY;
        let mut dropped = packet(2_000, 1);
        dropped.flags = AUDIO_DISCONTINUITY;
        let capture = FakeCapture::new(format, [Ok(Some(accepted)), Ok(Some(dropped))])
            .with_drop_counter(drops.clone());
        let (sender, packets) = mpsc::sync_channel(1);
        let health = Arc::new(Mutex::new(Health::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let thread_health = health.clone();
        let thread_stop = stop.clone();
        let thread = thread::spawn(move || {
            run_capture_loop(
                Box::new(capture),
                || panic!("queue overflow must not reopen capture"),
                &sender,
                &thread_health,
                &thread_stop,
                RecoveryPolicy {
                    expected_endpoint: "render.fixed".into(),
                    expected_format: format,
                    follow_console_default: false,
                    retry_delay: Duration::ZERO,
                },
            );
        });
        let mut worker = Worker {
            packets,
            health,
            stop,
            thread: Some(thread),
        };
        let deadline = Instant::now() + Duration::from_secs(1);
        while worker.health().failure.is_none() && Instant::now() < deadline {
            thread::yield_now();
        }
        let health = worker.health();
        assert_eq!(health.failure, Some("audio_queue_overflow"));
        assert_eq!(health.dropped_packets, 1);
        assert_eq!(health.discontinuities, 1);
        assert_eq!(worker.read().unwrap().unwrap().sequence, 0);
        assert_eq!(worker.read().err(), Some("audio_queue_overflow"));
        drop(worker);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn queued_pcm_drains_before_terminal_capture_failure_and_releases_capture() {
        let format = Format::new(48_000, 1, amaterasu_media::PCM_F32).unwrap();
        let drops = Arc::new(AtomicUsize::new(0));
        let capture = FakeCapture::new(
            format,
            [
                Ok(Some(packet(1_000, 0))),
                Ok(Some(packet(2_000, 1))),
                Err("audio_capture_failed"),
            ],
        )
        .with_drop_counter(drops.clone());
        let (sender, packets) = mpsc::sync_channel(QUEUE_PACKETS);
        let health = Arc::new(Mutex::new(Health::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let thread_health = health.clone();
        let thread_stop = stop.clone();
        let thread = thread::spawn(move || {
            run_capture_loop(
                Box::new(capture),
                || panic!("terminal capture failure must not reopen"),
                &sender,
                &thread_health,
                &thread_stop,
                RecoveryPolicy {
                    expected_endpoint: "render.fixed".into(),
                    expected_format: format,
                    follow_console_default: false,
                    retry_delay: Duration::ZERO,
                },
            );
        });
        let mut worker = Worker {
            packets,
            health,
            stop,
            thread: Some(thread),
        };
        let deadline = Instant::now() + Duration::from_secs(1);
        while worker.health().failure.is_none() && Instant::now() < deadline {
            thread::yield_now();
        }
        assert_eq!(worker.health().failure, Some("audio_capture_failed"));
        assert_eq!(worker.read().unwrap().unwrap().sequence, 0);
        assert_eq!(worker.read().unwrap().unwrap().sequence, 1);
        assert_eq!(worker.read().err(), Some("audio_capture_failed"));
        drop(worker);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn temporary_empty_capture_poll_preserves_pcm_and_later_device_recovery() {
        let format = Format::new(48_000, 1, amaterasu_media::PCM_F32).unwrap();
        let drops = Arc::new(AtomicUsize::new(0));
        let capture = FakeCapture::new(
            format,
            [Ok(None), Ok(Some(packet(1_000, 0))), Err("device_lost")],
        )
        .with_drop_counter(drops.clone());
        let (sender, packets) = mpsc::sync_channel(3);
        let health = Arc::new(Mutex::new(Health::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let thread_health = health.clone();
        let thread_stop = stop.clone();
        let runner_drops = drops.clone();
        let runner = thread::spawn(move || {
            run_capture_loop(
                Box::new(capture),
                || {
                    Ok(Box::new(
                        FakeCapture::new(
                            format,
                            [Ok(Some(packet(0, 0))), Err("audio_capture_failed")],
                        )
                        .with_drop_counter(runner_drops.clone()),
                    ) as Box<dyn CaptureDevice>)
                },
                &sender,
                &thread_health,
                &thread_stop,
                RecoveryPolicy {
                    expected_endpoint: "render.fixed".into(),
                    expected_format: format,
                    follow_console_default: false,
                    retry_delay: Duration::ZERO,
                },
            );
        });

        let captured = packets.recv_timeout(Duration::from_secs(1)).unwrap();
        let recovered = packets.recv_timeout(Duration::from_secs(1)).unwrap();
        runner.join().unwrap();
        assert_eq!((captured.sequence, captured.timestamp_us), (0, 1_000));
        assert_eq!(recovered.sequence, 1);
        assert!(recovered.timestamp_us > captured.timestamp_us);
        assert_ne!(recovered.flags & AUDIO_DISCONTINUITY, 0);
        let health = health.lock().unwrap();
        assert_eq!(health.failure, Some("audio_capture_failed"));
        assert!(!health.recovering);
        assert_eq!(drops.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn device_loss_retries_open_and_preserves_packet_clock() {
        let format = Format::new(48_000, 1, amaterasu_media::PCM_F32).unwrap();
        let drops = Arc::new(AtomicUsize::new(0));
        let initial = FakeCapture::new(format, [Ok(Some(packet(1_000, 0))), Err("device_lost")])
            .with_drop_counter(drops.clone());
        let (sender, packets) = mpsc::sync_channel(4);
        let health = Arc::new(Mutex::new(Health::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let attempts = Arc::new(AtomicUsize::new(0));
        let recovering_during_retry = Arc::new(AtomicBool::new(false));

        let runner = thread::spawn({
            let health = health.clone();
            let recovery_health = health.clone();
            let stop = stop.clone();
            let attempts = attempts.clone();
            let recovering_during_retry = recovering_during_retry.clone();
            let drops = drops.clone();
            move || {
                let mut recovery_attempt = 0;
                run_capture_loop(
                    Box::new(initial),
                    move || {
                        attempts.fetch_add(1, Ordering::SeqCst);
                        recovery_attempt += 1;
                        if recovery_attempt == 1 {
                            recovering_during_retry.store(
                                recovery_health.lock().unwrap().recovering,
                                Ordering::Release,
                            );
                            return Err("endpoint_not_found");
                        }
                        Ok(Box::new(
                            FakeCapture::new(format, [Ok(Some(packet(0, 0)))])
                                .with_drop_counter(drops.clone()),
                        ) as Box<dyn CaptureDevice>)
                    },
                    &sender,
                    &health,
                    &stop,
                    RecoveryPolicy {
                        expected_endpoint: "render.fixed".into(),
                        expected_format: format,
                        follow_console_default: false,
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
        assert_eq!(drops.load(Ordering::SeqCst), 2);
        assert!(recovering_during_retry.load(Ordering::Acquire));
        let health = health.lock().unwrap();
        assert!(!health.recovering);
        assert_eq!(health.failure, None);
        assert_eq!(health.active_endpoint.as_deref(), Some("render.fixed"));
        assert_eq!(health.discontinuities, 1);
    }

    #[test]
    fn shared_virtual_format_change_retries_recovery_without_failing_stream() {
        let format = Format::new(48_000, 1, amaterasu_media::PCM_F32).unwrap();
        let initial = FakeCapture::new(format, [Err("device_lost")]);
        let (sender, packets) = mpsc::sync_channel(1);
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
                    || {
                        if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                            return Err("virtual_sink_format_changed");
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
                        follow_console_default: false,
                        retry_delay: Duration::from_millis(1),
                    },
                );
            }
        });

        let recovered = packets.recv_timeout(Duration::from_secs(1)).unwrap();
        stop.store(true, Ordering::Release);
        runner.thread().unpark();
        runner.join().unwrap();
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        assert_eq!(recovered.sequence, 0);
        assert_ne!(recovered.flags & AUDIO_DISCONTINUITY, 0);
        let health = health.lock().unwrap();
        assert_eq!(health.failure, None);
        assert!(!health.recovering);
    }

    #[test]
    fn device_loss_reopens_immediately_before_retry_backoff() {
        let format = Format::new(48_000, 1, amaterasu_media::PCM_F32).unwrap();
        let initial = FakeCapture::new(format, [Err("device_lost")]);
        let (sender, packets) = mpsc::sync_channel(1);
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
                        Ok(Box::new(FakeCapture::new(format, [Ok(Some(packet(0, 0)))]))
                            as Box<dyn CaptureDevice>)
                    },
                    &sender,
                    &health,
                    &stop,
                    RecoveryPolicy {
                        expected_endpoint: "render.fixed".into(),
                        expected_format: format,
                        follow_console_default: false,
                        retry_delay: Duration::from_secs(30),
                    },
                );
            }
        });

        let recovered = packets.recv_timeout(Duration::from_secs(1)).unwrap();
        stop.store(true, Ordering::Release);
        runner.thread().unpark();
        runner.join().unwrap();

        assert_eq!(recovered.sequence, 0);
        assert_ne!(recovered.flags & AUDIO_DISCONTINUITY, 0);
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        assert!(!health.lock().unwrap().recovering);
    }

    #[test]
    fn reopened_capture_that_loses_device_without_pcm_waits_before_reopening_again() {
        let format = Format::new(48_000, 1, amaterasu_media::PCM_F32).unwrap();
        let initial = FakeCapture::new(format, [Err("device_lost")]);
        let (sender, _packets) = mpsc::sync_channel(1);
        let health = Arc::new(Mutex::new(Health::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let attempts = Arc::new(AtomicUsize::new(0));
        let drops = Arc::new(AtomicUsize::new(0));
        let runner = thread::spawn({
            let health = health.clone();
            let stop = stop.clone();
            let attempts = attempts.clone();
            let drops = drops.clone();
            move || {
                run_capture_loop(
                    Box::new(initial),
                    move || {
                        attempts.fetch_add(1, Ordering::SeqCst);
                        Ok(Box::new(
                            FakeCapture::new(format, [Err("device_lost")])
                                .with_drop_counter(drops.clone()),
                        ) as Box<dyn CaptureDevice>)
                    },
                    &sender,
                    &health,
                    &stop,
                    RecoveryPolicy {
                        expected_endpoint: "render.fixed".into(),
                        expected_format: format,
                        follow_console_default: false,
                        retry_delay: Duration::from_secs(30),
                    },
                );
            }
        });

        let deadline = Instant::now() + Duration::from_secs(1);
        while drops.load(Ordering::SeqCst) == 0 && Instant::now() < deadline {
            thread::yield_now();
        }
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        // The next open would happen almost immediately without the backoff.
        thread::sleep(Duration::from_millis(20));
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        assert!(health.lock().unwrap().recovering);
        stop.store(true, Ordering::Release);
        runner.thread().unpark();
        runner.join().unwrap();
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn recovered_pcm_allows_next_device_loss_to_reopen_immediately() {
        let format = Format::new(48_000, 1, amaterasu_media::PCM_F32).unwrap();
        let initial = FakeCapture::new(format, [Err("device_lost")]);
        let (sender, packets) = mpsc::sync_channel(2);
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
                        let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                        let events = if attempt == 0 {
                            vec![Ok(Some(packet(1_000, 0))), Err("device_lost")]
                        } else {
                            vec![Ok(Some(packet(0, 0)))]
                        };
                        Ok(Box::new(FakeCapture::new(format, events)) as Box<dyn CaptureDevice>)
                    },
                    &sender,
                    &health,
                    &stop,
                    RecoveryPolicy {
                        expected_endpoint: "render.fixed".into(),
                        expected_format: format,
                        follow_console_default: false,
                        retry_delay: Duration::from_secs(30),
                    },
                );
            }
        });

        let first = packets.recv_timeout(Duration::from_secs(1)).unwrap();
        let second = packets.recv_timeout(Duration::from_secs(1)).unwrap();
        stop.store(true, Ordering::Release);
        runner.thread().unpark();
        runner.join().unwrap();
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        assert_eq!((first.sequence, second.sequence), (0, 1));
        assert!(second.timestamp_us > first.timestamp_us);
        assert_ne!(second.flags & AUDIO_DISCONTINUITY, 0);
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
                        Err("virtual_sink_format_changed")
                    },
                    &sender,
                    &health,
                    &stop,
                    RecoveryPolicy {
                        expected_endpoint: "render.fixed".into(),
                        expected_format: format,
                        follow_console_default: false,
                        retry_delay: Duration::from_secs(30),
                    },
                );
            }
        });

        let deadline = Instant::now() + Duration::from_secs(1);
        while (!health.lock().unwrap().recovering || attempts.load(Ordering::SeqCst) == 0)
            && Instant::now() < deadline
        {
            thread::yield_now();
        }
        assert!(health.lock().unwrap().recovering);
        stop.store(true, Ordering::Release);
        runner.thread().unpark();
        runner.join().unwrap();
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn quiet_reopened_capture_stays_recovering_and_is_released_on_shutdown() {
        let format = Format::new(48_000, 1, amaterasu_media::PCM_F32).unwrap();
        let drops = Arc::new(AtomicUsize::new(0));
        let initial =
            FakeCapture::new(format, [Err("device_lost")]).with_drop_counter(drops.clone());
        let (sender, _packets) = mpsc::sync_channel(1);
        let health = Arc::new(Mutex::new(Health::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let reopened = Arc::new(AtomicBool::new(false));
        let runner = thread::spawn({
            let health = health.clone();
            let stop = stop.clone();
            let reopened = reopened.clone();
            let drops = drops.clone();
            move || {
                run_capture_loop(
                    Box::new(initial),
                    || {
                        reopened.store(true, Ordering::Release);
                        Ok(Box::new(
                            FakeCapture::new(format, [Ok(None)]).with_drop_counter(drops.clone()),
                        ) as Box<dyn CaptureDevice>)
                    },
                    &sender,
                    &health,
                    &stop,
                    RecoveryPolicy {
                        expected_endpoint: "render.fixed".into(),
                        expected_format: format,
                        follow_console_default: false,
                        retry_delay: Duration::ZERO,
                    },
                );
            }
        });

        let deadline = Instant::now() + Duration::from_secs(1);
        while !reopened.load(Ordering::Acquire) && Instant::now() < deadline {
            thread::yield_now();
        }
        assert!(reopened.load(Ordering::Acquire));
        let deadline = Instant::now() + Duration::from_secs(1);
        while drops.load(Ordering::SeqCst) == 0 && Instant::now() < deadline {
            thread::yield_now();
        }
        let snapshot = health.lock().unwrap().clone();
        assert!(snapshot.recovering);
        assert_eq!(snapshot.failure, None);
        assert_eq!(snapshot.active_endpoint.as_deref(), Some("render.fixed"));
        stop.store(true, Ordering::Release);
        runner.thread().unpark();
        runner.join().unwrap();
        assert_eq!(drops.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn failed_reopen_then_quiet_capture_recovers_only_after_pcm_is_queued() {
        let format = Format::new(48_000, 1, amaterasu_media::PCM_F32).unwrap();
        let drops = Arc::new(AtomicUsize::new(0));
        let initial =
            FakeCapture::new(format, [Err("device_lost")]).with_drop_counter(drops.clone());
        let (capture_sender, capture_events) = mpsc::channel();
        let (sender, packets) = mpsc::sync_channel(1);
        let health = Arc::new(Mutex::new(Health::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let attempts = Arc::new(AtomicUsize::new(0));
        let runner = thread::spawn({
            let health = health.clone();
            let stop = stop.clone();
            let attempts = attempts.clone();
            let drops = drops.clone();
            move || {
                let mut capture_events = Some(capture_events);
                run_capture_loop(
                    Box::new(initial),
                    move || {
                        if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                            return Err("endpoint_not_found");
                        }
                        Ok(Box::new(SignaledCapture {
                            format,
                            events: capture_events.take().expect("capture reopens only once"),
                            drops: drops.clone(),
                        }) as Box<dyn CaptureDevice>)
                    },
                    &sender,
                    &health,
                    &stop,
                    RecoveryPolicy {
                        expected_endpoint: "render.fixed".into(),
                        expected_format: format,
                        follow_console_default: false,
                        retry_delay: Duration::ZERO,
                    },
                );
            }
        });

        let deadline = Instant::now() + Duration::from_secs(1);
        while attempts.load(Ordering::SeqCst) < 2 && Instant::now() < deadline {
            thread::yield_now();
        }
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        assert!(health.lock().unwrap().recovering);
        capture_sender.send(Ok(Some(packet(5_000, 0)))).unwrap();
        assert_eq!(
            packets
                .recv_timeout(Duration::from_secs(1))
                .unwrap()
                .sequence,
            0
        );
        let deadline = Instant::now() + Duration::from_secs(1);
        while health.lock().unwrap().recovering && Instant::now() < deadline {
            thread::yield_now();
        }
        assert!(!health.lock().unwrap().recovering);
        stop.store(true, Ordering::Release);
        runner.thread().unpark();
        runner.join().unwrap();
        assert_eq!(drops.load(Ordering::SeqCst), 2);
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
                follow_console_default: false,
                retry_delay: Duration::ZERO,
            },
        );
        let health = health.lock().unwrap();
        assert_eq!(health.failure, Some("audio_format_changed"));
        assert!(!health.recovering);
    }

    #[test]
    fn unsupported_reopened_format_fails_instead_of_retrying_forever() {
        for error in ["unsupported_format", "unsupported_channel_layout"] {
            let format = Format::new(48_000, 1, amaterasu_media::PCM_F32).unwrap();
            let initial = FakeCapture::new(format, [Err("device_lost")]);
            let (sender, _packets) = mpsc::sync_channel(1);
            let health = Mutex::new(Health::default());
            let attempts = AtomicUsize::new(0);
            run_capture_loop(
                Box::new(initial),
                || {
                    attempts.fetch_add(1, Ordering::SeqCst);
                    Err(error)
                },
                &sender,
                &health,
                &AtomicBool::new(false),
                RecoveryPolicy {
                    expected_endpoint: "render.fixed".into(),
                    expected_format: format,
                    follow_console_default: false,
                    retry_delay: Duration::from_secs(30),
                },
            );
            assert_eq!(attempts.load(Ordering::SeqCst), 1);
            let health = health.lock().unwrap();
            assert_eq!(health.failure, Some(error));
            assert!(!health.recovering);
        }
    }

    #[test]
    fn console_default_change_reopens_new_endpoint_without_breaking_packet_clock() {
        let format = Format::new(48_000, 1, amaterasu_media::PCM_F32).unwrap();
        let initial = FakeCapture::new(
            format,
            [Ok(Some(packet(1_000, 0))), Err("default_endpoint_changed")],
        );
        let (sender, packets) = mpsc::sync_channel(4);
        let health = Arc::new(Mutex::new(Health::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let runner = thread::spawn({
            let health = health.clone();
            let stop = stop.clone();
            move || {
                run_capture_loop(
                    Box::new(initial),
                    || {
                        Ok(Box::new(
                            FakeCapture::new(format, [Ok(Some(packet(0, 0)))])
                                .with_endpoint("render.console-new"),
                        ) as Box<dyn CaptureDevice>)
                    },
                    &sender,
                    &health,
                    &stop,
                    RecoveryPolicy {
                        expected_endpoint: "render.fixed".into(),
                        expected_format: format,
                        follow_console_default: true,
                        retry_delay: Duration::from_secs(30),
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
        let health = health.lock().unwrap();
        assert!(!health.recovering);
        assert_eq!(health.failure, None);
        assert_eq!(
            health.active_endpoint.as_deref(),
            Some("render.console-new")
        );
        assert_eq!(health.discontinuities, 1);
    }

    #[test]
    fn pinned_endpoint_recovery_rejects_a_different_endpoint() {
        let format = Format::new(48_000, 1, amaterasu_media::PCM_F32).unwrap();
        let initial = FakeCapture::new(format, [Err("device_lost")]);
        let (sender, _packets) = mpsc::sync_channel(1);
        let health = Mutex::new(Health::default());
        run_capture_loop(
            Box::new(initial),
            || {
                Ok(
                    Box::new(FakeCapture::new(format, []).with_endpoint("render.other"))
                        as Box<dyn CaptureDevice>,
                )
            },
            &sender,
            &health,
            &AtomicBool::new(false),
            RecoveryPolicy {
                expected_endpoint: "render.fixed".into(),
                expected_format: format,
                follow_console_default: false,
                retry_delay: Duration::ZERO,
            },
        );
        assert_eq!(
            health.lock().unwrap().failure,
            Some("audio_endpoint_changed")
        );
    }
}
