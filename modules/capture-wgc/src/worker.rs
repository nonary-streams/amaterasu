//! One resource-owning thread per stream; only owned CPU frames cross channels.
#[cfg(any(windows, test))]
mod implementation {
    use crate::backend::CaptureSession;
    use crate::model::{CaptureError, CaptureResult, Frame};
    use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TryRecvError};
    use std::thread::{self, JoinHandle};
    use std::time::Duration;

    pub trait FrameSource {
        fn poll(&mut self) -> CaptureResult<Option<Frame>>;
    }

    pub struct Worker {
        requests: Option<SyncSender<()>>,
        replies: Receiver<CaptureResult<Option<Frame>>>,
        thread: Option<JoinHandle<()>>,
        waiting: bool,
    }

    impl Worker {
        #[cfg(test)]
        pub fn start<S, F>(initialize: F) -> CaptureResult<Self>
        where
            S: FrameSource + 'static,
            F: FnOnce() -> CaptureResult<S> + Send + 'static,
        {
            Self::start_with_metadata(move || initialize().map(|source| (source, ())))
                .map(|(worker, ())| worker)
        }

        pub fn start_with_metadata<S, M, F>(initialize: F) -> CaptureResult<(Self, M)>
        where
            S: FrameSource + 'static,
            M: Send + 'static,
            F: FnOnce() -> CaptureResult<(S, M)> + Send + 'static,
        {
            let (requests, request_receiver) = mpsc::sync_channel(1);
            let (reply_sender, replies) = mpsc::sync_channel(1);
            let (ready_sender, ready) = mpsc::sync_channel(1);
            let thread = thread::Builder::new()
                .name("amaterasu-wgc".into())
                .spawn(move || {
                    let (mut source, metadata) = match initialize() {
                        Ok(value) => value,
                        Err(error) => {
                            let _ = ready_sender.send(Err(error));
                            return;
                        }
                    };
                    if ready_sender.send(Ok(metadata)).is_err() {
                        return;
                    }
                    while request_receiver.recv().is_ok() {
                        if reply_sender.try_send(source.poll()).is_err() {
                            break;
                        }
                    }
                    // Source drops here, on its creation thread, before join.
                })
                .map_err(|error| CaptureError::new("backend_error", error.to_string()))?;
            let worker = Self {
                requests: Some(requests),
                replies,
                thread: Some(thread),
                waiting: false,
            };
            let metadata = ready.recv().map_err(|_| worker_ended())??;
            Ok((worker, metadata))
        }
    }

    impl CaptureSession for Worker {
        fn read(&mut self, wait_ms: u32) -> CaptureResult<Option<Frame>> {
            if !self.waiting {
                self.requests
                    .as_ref()
                    .ok_or_else(worker_ended)?
                    .try_send(())
                    .map_err(|_| worker_ended())?;
                self.waiting = true;
            }
            let reply = if wait_ms == 0 {
                self.replies.try_recv().map_err(|error| match error {
                    TryRecvError::Empty => RecvTimeoutError::Timeout,
                    TryRecvError::Disconnected => RecvTimeoutError::Disconnected,
                })
            } else {
                self.replies
                    .recv_timeout(Duration::from_millis(u64::from(wait_ms)))
            };
            match reply {
                Ok(result) => {
                    self.waiting = false;
                    result
                }
                Err(RecvTimeoutError::Timeout) => Ok(None),
                Err(RecvTimeoutError::Disconnected) => Err(worker_ended()),
            }
        }
    }

    impl Drop for Worker {
        fn drop(&mut self) {
            // Disconnect before joining, so an idle worker wakes. Joining is
            // mandatory: no worker code may outlive module library unloading.
            self.requests.take();
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    fn worker_ended() -> CaptureError {
        CaptureError::new(
            "access_lost",
            "WGC worker ended; enumerate and reopen the output",
        )
    }
}

#[cfg(any(windows, test))]
pub use implementation::{FrameSource, Worker};

#[cfg(test)]
mod tests {
    use super::{FrameSource, Worker};
    use crate::backend::CaptureSession;
    use crate::model::{CaptureError, CaptureResult, Frame};
    use std::rc::Rc;
    use std::sync::{Arc, Mutex, mpsc};
    use std::thread::{self, ThreadId};
    use std::time::Duration;

    struct ThreadBoundSource {
        trace: Arc<Mutex<Vec<(String, ThreadId)>>>,
        gate: mpsc::Receiver<()>,
        // Compile-time proof that the source need not be Send. Only the
        // initialization closure crosses threads, never the resource itself.
        _thread_bound: Rc<()>,
    }

    impl FrameSource for ThreadBoundSource {
        fn poll(&mut self) -> CaptureResult<Option<Frame>> {
            self.trace
                .lock()
                .unwrap()
                .push(("poll".into(), thread::current().id()));
            self.gate.recv().unwrap();
            Ok(Some(Frame {
                width: 1,
                height: 1,
                stride: 4,
                timestamp_us: 0,
                capture_pacing_timestamp_us: 42,
                sequence: 1,
                bytes: Arc::new(vec![1, 2, 3, 4]),
            }))
        }
    }

    impl Drop for ThreadBoundSource {
        fn drop(&mut self) {
            self.trace
                .lock()
                .unwrap()
                .push(("drop".into(), thread::current().id()));
        }
    }

    #[test]
    fn worker_owns_non_send_resources_and_joins_after_destruction() {
        let trace = Arc::new(Mutex::new(Vec::new()));
        let worker_trace = trace.clone();
        let (gate, gate_receiver) = mpsc::channel();
        let mut worker = Worker::start(move || {
            worker_trace
                .lock()
                .unwrap()
                .push(("open".into(), thread::current().id()));
            Ok(ThreadBoundSource {
                trace: worker_trace,
                gate: gate_receiver,
                _thread_bound: Rc::new(()),
            })
        })
        .unwrap();
        // The worker's poll is held by the gate; repeated reads never wait or
        // enqueue more than this single outstanding poll.
        for _ in 0..100 {
            assert!(worker.read(0).unwrap().is_none());
        }
        gate.send(()).unwrap();
        let mut frame = None;
        for _ in 0..100_000 {
            frame = worker.read(0).unwrap();
            if frame.is_some() {
                break;
            }
            thread::yield_now();
        }
        assert_eq!(frame.unwrap().bytes.as_slice(), [1, 2, 3, 4]);
        drop(worker);
        let trace = trace.lock().unwrap();
        assert_eq!(
            trace
                .iter()
                .map(|(operation, _)| operation.as_str())
                .collect::<Vec<_>>(),
            ["open", "poll", "drop"]
        );
        assert_ne!(trace[0].1, thread::current().id());
        assert!(trace.iter().all(|(_, id)| *id == trace[0].1));
    }

    #[test]
    fn bounded_read_waits_for_existing_poll_without_requesting_another_frame() {
        let trace = Arc::new(Mutex::new(Vec::new()));
        let worker_trace = trace.clone();
        let (gate, gate_receiver) = mpsc::channel();
        let mut worker = Worker::start(move || {
            Ok(ThreadBoundSource {
                trace: worker_trace,
                gate: gate_receiver,
                _thread_bound: Rc::new(()),
            })
        })
        .unwrap();

        assert!(worker.read(1).unwrap().is_none());
        let release = thread::spawn(move || {
            thread::sleep(Duration::from_millis(10));
            gate.send(()).unwrap();
        });
        let frame = worker.read(1_000).unwrap().unwrap();
        release.join().unwrap();
        assert_eq!(frame.bytes.as_slice(), [1, 2, 3, 4]);
        assert_eq!(
            trace
                .lock()
                .unwrap()
                .iter()
                .filter(|(operation, _)| operation == "poll")
                .count(),
            1
        );
    }

    #[test]
    fn failed_worker_initialization_returns_error_after_cleanup() {
        let trace = Arc::new(Mutex::new(Vec::new()));
        let worker_trace = trace.clone();
        let result = Worker::start(move || -> CaptureResult<ThreadBoundSource> {
            let (_, gate) = mpsc::channel();
            let _resource = ThreadBoundSource {
                trace: worker_trace,
                gate,
                _thread_bound: Rc::new(()),
            };
            Err(CaptureError::new(
                "unsupported_platform",
                "test startup failure",
            ))
        });
        assert!(matches!(result, Err(error) if error.code == "unsupported_platform"));
        assert_eq!(trace.lock().unwrap().len(), 1);
    }
}
