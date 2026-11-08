//! Metadata runs in the verified helper, never in the cooperative host thread.
//! The worker is joined before its result is returned. The shared registry keeps
//! the helper receipt until the exact child is reaped, even after cancellation.
use crate::{
    helper_client::Client,
    helper_process::{self, Registry},
    model::{CaptureError, CaptureResult},
    native::{NativeInfo, NativeOutput},
    open_task::{OpenTask, TaskPoll},
};
use std::io;

pub(crate) enum MetadataRequest {
    Probe,
    Enumerate,
}
pub(crate) enum Metadata {
    Probe(NativeInfo),
    Enumerate(Vec<NativeOutput>),
}

pub(crate) struct MetadataTask {
    task: OpenTask<CaptureResult<Metadata>>,
}

impl MetadataTask {
    pub fn spawn(registry: Registry, request: MetadataRequest) -> io::Result<Self> {
        let task = OpenTask::spawn(move |cancellation| {
            if cancellation.cancelled() {
                return Err(CaptureError::new(
                    "cancelled",
                    "KMS metadata request cancelled",
                ));
            }
            let image = include_bytes!(concat!(env!("OUT_DIR"), "/amaterasu-kms-helper"));
            let (socket, receipt) =
                helper_process::spawn_cancellable(&registry, image, &cancellation)
                    .map_err(|error| CaptureError::new("capture_unavailable", error.to_string()))?;
            if let Err(error) = cancellation.register(&socket) {
                receipt.stop();
                return Err(CaptureError::new("capture_unavailable", error.to_string()));
            }
            let mut client = Client::new(socket, receipt)
                .map_err(|error| CaptureError::new("capture_unavailable", error.to_string()))?;
            match request {
                MetadataRequest::Probe => client.probe().map(Metadata::Probe),
                MetadataRequest::Enumerate => client.enumerate().map(Metadata::Enumerate),
            }
        })?;
        Ok(Self { task })
    }

    pub fn cancel(&self) -> bool {
        self.task.cancel()
    }

    /// Failed joining is not retirement evidence. Callers must keep this task
    /// alive and retry or retain it behind their unload barrier.
    pub fn poll(&mut self) -> TaskPoll<CaptureResult<Metadata>> {
        self.task.poll()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        sync::mpsc,
        thread,
        time::{Duration, Instant},
    };

    #[test]
    fn cancellation_retains_worker_until_it_actually_exits() {
        let (entered, started) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let mut task = MetadataTask {
            task: OpenTask::spawn(move |cancellation| {
                entered.send(()).unwrap();
                released.recv().unwrap();
                assert!(cancellation.cancelled());
                Ok(Metadata::Probe(NativeInfo::default()))
            })
            .unwrap(),
        };
        started.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(task.cancel());
        assert!(matches!(task.poll(), TaskPoll::Pending));
        release.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match task.poll() {
                TaskPoll::Pending => {
                    assert!(Instant::now() < deadline);
                    thread::yield_now();
                }
                TaskPoll::Failed(error) => panic!("join failed: {error}"),
                TaskPoll::Retired(result) => {
                    // Cancellation wins ownership of the result. In
                    // particular, a completed result could contain a session
                    // that must be dropped instead of adopted after cancel.
                    assert!(matches!(
                        result,
                        Err(error) if error.kind() == io::ErrorKind::Interrupted
                    ));
                    break;
                }
            }
        }
        let Metadata::Enumerate(outputs) = Metadata::Enumerate(Vec::new()) else {
            unreachable!()
        };
        assert!(outputs.is_empty());
        let _ = (MetadataRequest::Probe, MetadataRequest::Enumerate);
        let _ = MetadataTask::spawn as fn(Registry, MetadataRequest) -> io::Result<MetadataTask>;
    }
}
