//! The update's phases and the rules for moving between them.
//!
//! Requests are answered on the host thread and never wait: a check or a
//! download runs on one worker thread, and the host shell performs activation.
use super::check::check;
use super::download::download;
use super::environment::{Environment, UpdateSource};
use super::plan::{Checked, Offer};
use super::transport::Connector;
use super::{Action, Failure};
use crate::candidate_cache::PreparedCandidate;
use crate::host_activation::{self, RequestRefusal};
use amaterasu_contracts::HostPackageActivation;
use serde_json::{Value, json};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread::{self, JoinHandle},
    time::{SystemTime, UNIX_EPOCH},
};

/// Why a request was refused without changing anything.
type Refusal = &'static str;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Phase {
    /// Nothing has been checked since this module started.
    Idle,
    Checking,
    /// Checked, and the repository offers nothing newer.
    Current,
    Available,
    Downloading,
    /// The offered set is verified in the repository cache.
    Ready,
    /// The host holds the activation request until streams have ended.
    WaitingStream,
    Activating,
    Failed,
    Cancelled,
}

impl Phase {
    fn name(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Checking => "checking",
            Self::Current => "current",
            Self::Available => "available",
            Self::Downloading => "downloading",
            Self::Ready => "ready",
            Self::WaitingStream => "waiting_stream",
            Self::Activating => "activating",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

/// How the host answered an activation request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Admission {
    /// The host holds the request and will act on it.
    Accepted,
    /// The host already holds or is performing an activation.
    InProgress,
    /// This host does not perform activations for management.
    Unavailable,
}

/// The host's view of the one activation request it can hold.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct HostActivationState {
    /// A request is waiting for streams to end.
    pub pending: bool,
    /// The host has taken the request and is replacing the running set.
    pub started: bool,
}

/// The host shell's activation endpoint. It alone decides whether a prepared
/// candidate may run; this module only asks.
pub(crate) trait ActivationHost {
    fn activate(&mut self, request: HostPackageActivation) -> Admission;
    /// Withdraws a request the host has not started. False once it has.
    fn cancel(&mut self) -> bool;
    fn state(&mut self) -> Option<HostActivationState>;
}

enum Job {
    Check(JoinHandle<Result<Checked, Failure>>),
    Download(JoinHandle<Result<PreparedCandidate, Failure>>),
}

impl Job {
    fn is_finished(&self) -> bool {
        match self {
            Self::Check(thread) => thread.is_finished(),
            Self::Download(thread) => thread.is_finished(),
        }
    }
}

pub(crate) struct Service<C: Connector> {
    connector: C,
    environment: Environment,
    phase: Phase,
    error: Option<&'static str>,
    checked_unix_ms: Option<u64>,
    offer: Option<Arc<Offer>>,
    /// The `update` object of a status reply, built once per offer.
    described_offer: Option<Value>,
    prepared: Option<PreparedCandidate>,
    cancelled: Arc<AtomicBool>,
    transferred: Arc<AtomicU64>,
    job: Option<Job>,
    shutdown_announced: bool,
}

impl<C: Connector> Service<C> {
    pub(crate) fn new(connector: C, environment: Environment) -> Self {
        let failed = environment.last_activation_failed;
        Self {
            connector,
            environment,
            phase: if failed { Phase::Failed } else { Phase::Idle },
            error: failed.then_some("activation_failed"),
            checked_unix_ms: None,
            offer: None,
            described_offer: None,
            prepared: None,
            cancelled: Arc::new(AtomicBool::new(false)),
            transferred: Arc::new(AtomicU64::new(0)),
            job: None,
            shutdown_announced: false,
        }
    }

    /// Answers one console request. `other_install_active` is true while
    /// another flow uses the repository cache or the host's activation request.
    pub(crate) fn request(
        &mut self,
        action: Action,
        host: &mut dyn ActivationHost,
        other_install_active: bool,
    ) -> Value {
        self.advance();
        let outcome = match action {
            Action::Status => {
                self.observe_activation(host);
                Ok(())
            }
            Action::Check => self
                .admit(other_install_active)
                .and_then(|()| self.begin_check()),
            Action::Download => self
                .admit(other_install_active)
                .and_then(|()| self.begin_download()),
            Action::Activate => self
                .admit(other_install_active)
                .and_then(|()| self.request_activation(host)),
            Action::Cancel => self.cancel(host),
        };
        match outcome {
            Ok(()) => self.status(),
            Err(code) => json!({"ok": false, "code": code}),
        }
    }

    /// Retires a finished worker without waiting for a running one.
    pub(crate) fn advance(&mut self) {
        if !self.job.as_ref().is_some_and(Job::is_finished) {
            return;
        }
        match self.job.take().expect("finished job exists") {
            Job::Check(thread) => {
                let outcome = thread
                    .join()
                    .unwrap_or(Err(Failure::Failed("repository_unavailable")));
                self.finish_check(outcome);
            }
            Job::Download(thread) => {
                let outcome = thread
                    .join()
                    .unwrap_or(Err(Failure::Failed("download_failed")));
                self.finish_download(outcome);
            }
        }
    }

    /// Close admission without interrupting a worker that is already running.
    pub(crate) fn prepare_shutdown(&mut self) {
        self.shutdown_announced = true;
    }

    /// Request cancellation and retire the worker once it has returned. The
    /// host keeps this module and its network provider until this is true.
    pub(crate) fn quiesce(&mut self) -> bool {
        self.shutdown_announced = true;
        self.cancelled.store(true, Ordering::Release);
        self.advance();
        self.job.is_none()
    }

    /// True while this flow uses the repository cache or the host's one
    /// activation request, so no other install may start.
    pub(crate) fn in_progress(&self) -> bool {
        self.job.is_some() || self.activation_requested()
    }

    fn activation_requested(&self) -> bool {
        matches!(self.phase, Phase::WaitingStream | Phase::Activating)
    }

    /// The refusals every request that starts work shares.
    fn admit(&self, other_install_active: bool) -> Result<(), Refusal> {
        if self.shutdown_announced {
            return Err("shutting_down");
        }
        if self.activation_requested() {
            return Err("activation_in_progress");
        }
        if self.job.is_some() || other_install_active {
            return Err("busy");
        }
        Ok(())
    }

    fn begin_check(&mut self) -> Result<(), Refusal> {
        let Some(source) = self.environment.source.clone() else {
            // Without a recorded source there is nothing to check; the status
            // says so.
            return Ok(());
        };
        let cancelled = Arc::new(AtomicBool::new(false));
        let connector = self.connector.clone();
        let worker_cancelled = cancelled.clone();
        let thread = spawn_worker(move || check(&source, &connector, &worker_cancelled))?;
        self.forget_offer();
        self.cancelled = cancelled;
        self.job = Some(Job::Check(thread));
        self.phase = Phase::Checking;
        self.error = None;
        Ok(())
    }

    fn begin_download(&mut self) -> Result<(), Refusal> {
        let (Some(source), Some(offer)) = (self.environment.source.clone(), self.offer.clone())
        else {
            return Err("nothing_to_download");
        };
        if self.prepared.is_some() {
            // The offered set is already verified in the cache.
            return Ok(());
        }
        let cancelled = Arc::new(AtomicBool::new(false));
        let transferred = Arc::new(AtomicU64::new(0));
        let connector = self.connector.clone();
        let (worker_cancelled, worker_transferred) = (cancelled.clone(), transferred.clone());
        let thread = spawn_worker(move || {
            download(
                &source,
                &offer,
                &connector,
                &worker_cancelled,
                &worker_transferred,
            )
        })?;
        self.cancelled = cancelled;
        self.transferred = transferred;
        self.job = Some(Job::Download(thread));
        self.phase = Phase::Downloading;
        self.error = None;
        Ok(())
    }

    /// Asks the host to activate the prepared candidate. The request is built
    /// from host-injected roots and the verified cache; nothing in it comes
    /// from the browser.
    fn request_activation(&mut self, host: &mut dyn ActivationHost) -> Result<(), Refusal> {
        let (Some(source), Some(prepared)) = (&self.environment.source, &self.prepared) else {
            return Err("not_ready");
        };
        let request = match activation_request(source, prepared) {
            Ok(request) => request,
            Err(RequestRefusal::ActivationInProgress) => return Err("activation_in_progress"),
            Err(RequestRefusal::SelectionUnavailable) => {
                self.fail("activation_unavailable");
                return Ok(());
            }
        };
        match host.activate(request) {
            Admission::Accepted => {
                self.phase = Phase::WaitingStream;
                self.error = None;
                Ok(())
            }
            Admission::InProgress => Err("activation_in_progress"),
            Admission::Unavailable => {
                self.fail("activation_unavailable");
                Ok(())
            }
        }
    }

    fn cancel(&mut self, host: &mut dyn ActivationHost) -> Result<(), Refusal> {
        self.observe_activation(host);
        match self.phase {
            Phase::Activating => Err("activation_in_progress"),
            Phase::WaitingStream => {
                if !host.cancel() {
                    return Err("activation_in_progress");
                }
                // The candidate is still verified in the cache.
                self.phase = Phase::Ready;
                self.error = None;
                Ok(())
            }
            _ => {
                if self.job.is_some() {
                    self.cancelled.store(true, Ordering::Release);
                }
                Ok(())
            }
        }
    }

    /// Follows an activation the host holds. The module is unloaded before
    /// the host replaces the set, so a later outcome is read at the next start.
    fn observe_activation(&mut self, host: &mut dyn ActivationHost) {
        if !self.activation_requested() {
            return;
        }
        let Some(state) = host.state() else {
            return;
        };
        if state.started {
            self.phase = Phase::Activating;
        } else if state.pending {
            self.phase = Phase::WaitingStream;
        } else {
            self.fail("activation_interrupted");
        }
    }

    fn finish_check(&mut self, outcome: Result<Checked, Failure>) {
        let checked = match outcome {
            Ok(checked) => checked,
            Err(failure) => return self.stop(failure),
        };
        self.checked_unix_ms = Some(unix_milliseconds());
        self.error = None;
        match checked {
            Checked::Current => self.phase = Phase::Current,
            Checked::Available(offer) => {
                self.described_offer = self
                    .environment
                    .source
                    .as_ref()
                    .map(|source| offer.describe(&source.repository));
                self.offer = Some(Arc::from(offer));
                self.phase = Phase::Available;
            }
        }
    }

    fn finish_download(&mut self, outcome: Result<PreparedCandidate, Failure>) {
        match outcome {
            Ok(prepared) => {
                self.prepared = Some(prepared);
                self.phase = Phase::Ready;
                self.error = None;
            }
            Err(failure) => self.stop(failure),
        }
    }

    fn stop(&mut self, failure: Failure) {
        match failure {
            Failure::Cancelled => {
                self.phase = Phase::Cancelled;
                self.error = self.shutdown_announced.then_some("shutting_down");
            }
            Failure::Failed(code) => self.fail(code),
        }
    }

    fn fail(&mut self, code: &'static str) {
        self.phase = Phase::Failed;
        self.error = Some(code);
    }

    fn forget_offer(&mut self) {
        self.offer = None;
        self.described_offer = None;
        self.prepared = None;
        self.transferred = Arc::new(AtomicU64::new(0));
    }

    fn status(&self) -> Value {
        let environment = &self.environment;
        let source = environment.source.as_ref().map(
            |source| json!({"repository": source.repository, "fingerprint": source.fingerprint}),
        );
        let error = self
            .error
            .or_else(|| environment.source.is_none().then_some("no_source"));
        json!({
            "ok": true,
            "supported": true,
            "phase": self.phase.name(),
            "checked_unix_ms": self.checked_unix_ms,
            "source": source,
            "running": {
                "digest": environment.running_digest,
                "sequence": environment.running_sequence.map(|sequence| sequence.to_string()),
            },
            "update": self.described_offer,
            "transferred_bytes": self.transferred_bytes(),
            "error": error,
        })
    }

    /// Bytes received for the offered set's files. The repository also serves
    /// the signed manifest during a download, so the count is held at the
    /// size the reply announces.
    fn transferred_bytes(&self) -> u64 {
        let received = self.transferred.load(Ordering::Relaxed);
        self.offer
            .as_ref()
            .map_or(received, |offer| received.min(offer.download_bytes))
    }
}

#[cfg(test)]
impl<C: Connector> Service<C> {
    /// Puts the service where an activation request the host accepted leaves
    /// it, for tests of how the module routes other installs meanwhile.
    pub(crate) fn assume_activation_requested(&mut self) {
        self.phase = Phase::WaitingStream;
    }
}

impl<C: Connector> Drop for Service<C> {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
        match self.job.take() {
            Some(Job::Check(thread)) => {
                let _ = thread.join();
            }
            Some(Job::Download(thread)) => {
                let _ = thread.join();
            }
            None => {}
        }
    }
}

/// The request for a prepared candidate, only while the selection still names
/// the set this module was loaded from as the active one.
fn activation_request(
    source: &UpdateSource,
    prepared: &PreparedCandidate,
) -> Result<HostPackageActivation, RequestRefusal> {
    let request = host_activation::request(&source.package_root, prepared)?;
    if request.expected_active != source.running.digest {
        return Err(RequestRefusal::SelectionUnavailable);
    }
    Ok(request)
}

fn spawn_worker<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<JoinHandle<T>, Refusal> {
    thread::Builder::new()
        .name("amaterasu-package-update".into())
        .spawn(work)
        .map_err(|_| "busy")
}

fn unix_milliseconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
        })
}
