//! Owned identity work that never calls runtime services or publishes native
//! snapshots. Initialization workers return generated material to the pipeline
//! for serial validation and state CAS. Configured provisioning workers create
//! the certificate/key pair with the existing file permission and rollback guards.
use crate::protocol::Identity;
use std::thread::{self, JoinHandle};

pub(super) type GenerationResult<T = Identity> = Result<T, &'static str>;
pub(super) type Generator<T = Identity> = Box<dyn FnOnce() -> GenerationResult<T> + Send>;

pub(super) struct IdentityWorker<T: Send + 'static> {
    thread: Option<JoinHandle<GenerationResult<T>>>,
}

impl<T: Send + 'static> IdentityWorker<T> {
    pub(super) fn start(generator: Generator<T>) -> Result<Self, &'static str> {
        let thread = thread::Builder::new()
            .name("pairing-identity".into())
            .spawn(generator)
            .map_err(|_| "identity_worker_unavailable")?;
        Ok(Self {
            thread: Some(thread),
        })
    }

    pub(super) fn take_finished(&mut self) -> Option<GenerationResult<T>> {
        if !self.thread.as_ref()?.is_finished() {
            return None;
        }
        self.thread
            .take()
            .map(|thread| thread.join().unwrap_or(Err("identity_failure")))
    }
}

impl<T: Send + 'static> Drop for IdentityWorker<T> {
    fn drop(&mut self) {
        // RSA generation cannot be interrupted safely. Retain ownership even
        // after request cancellation and join before the native module unloads.
        // Joining is restricted to teardown or an already-finished worker.
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
