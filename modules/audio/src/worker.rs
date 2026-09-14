//! Worker-owned COM capture feeds a bounded queue; no COM interface is Send.
use crate::{
    model::{AudioResult, Endpoint, Format, Health, Packet},
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
    time::Duration,
};

pub const QUEUE_PACKETS: usize = 16;
pub trait Source: Send {
    fn read(&mut self) -> AudioResult<Option<Packet>>;
    fn health(&self) -> Health;
}

pub struct Opened {
    pub source: Box<dyn Source>,
    pub endpoint: String,
    pub format: Format,
}

pub trait Backend: Send {
    fn enumerate(&self) -> AudioResult<Vec<Endpoint>>;
    fn open(&self, endpoint: &str) -> AudioResult<Opened>;
}

pub struct NativeBackend;
impl Backend for NativeBackend {
    fn enumerate(&self) -> AudioResult<Vec<Endpoint>> {
        enumerate()
    }

    fn open(&self, endpoint: &str) -> AudioResult<Opened> {
        let (source, endpoint, format) = Worker::open(endpoint.to_owned())?;
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
    pub fn open(endpoint: String) -> AudioResult<(Self, String, Format)> {
        let (sender, packets) = mpsc::sync_channel(QUEUE_PACKETS);
        let (ready, initialized) = mpsc::sync_channel(1);
        let health = Arc::new(Mutex::new(Health::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let thread_health = health.clone();
        let thread_stop = stop.clone();
        let thread = thread::Builder::new()
            .name("amaterasu-loopback".into())
            .spawn(move || {
                let mut capture = match platform::Capture::open(&endpoint) {
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
                while !thread_stop.load(Ordering::Acquire) {
                    match capture.read() {
                        Ok(Some(packet)) => {
                            if !enqueue(&sender, &thread_health, packet) {
                                break;
                            }
                        }
                        Ok(None) => thread::sleep(Duration::from_millis(2)),
                        Err(error) => {
                            fail(&thread_health, error);
                            break;
                        }
                    }
                }
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
            let _ = thread.join();
        }
    }
}

fn fail(health: &Mutex<Health>, error: &'static str) {
    if let Ok(mut health) = health.lock() {
        health.failure = Some(error);
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
}
