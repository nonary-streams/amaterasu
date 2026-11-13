//! The update's phases and the rules for moving between them.
//!
//! Requests are answered on the host thread and never wait: a check or a
//! download runs on one worker thread, and the host shell performs activation.
//! With automatic updates on, the same steps also start from the host tick.
use super::automatic::{Automatic, AutomaticMode, Progress, Step};
use super::check::check;
use super::download::download;
use super::environment::{Environment, UnattendedCaution, UpdateSource};
use super::plan::{Checked, Offer};
use super::transport::Connector;
use super::versions::{VersionRequest, Versions};
use super::{Action, Failure};
use crate::candidate_cache::PreparedCandidate;
use crate::host_activation::{self, RequestRefusal};
use amaterasu_contracts::{HostPackageActivation, HostPackageRestore};
use serde_json::{Value, json};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

/// How often, and how many times, a freshly started service asks whether the
/// host takes restore requests. The host shows that it does within its first
/// few ticks; one that has not after ten seconds is not going to.
const HOST_PROBE_INTERVAL: Duration = Duration::from_millis(250);
const HOST_PROBES: u32 = 40;

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
    /// Asks the host to return to a retained set. It holds the request like
    /// an activation, and `cancel` and `state` speak for either.
    fn restore(&mut self, request: HostPackageRestore) -> Admission;
    /// True when the host shell takes restore requests.
    fn restore_supported(&mut self) -> bool;
    /// Withdraws a request the host has not started. False once it has.
    fn cancel(&mut self) -> bool;
    fn state(&mut self) -> Option<HostActivationState>;
}

/// A return to a retained set that the host holds or is performing.
struct Restoring {
    digest: String,
    sequence: u64,
    /// Where the update stood when the restore was asked for, which is where
    /// it stands again if the restore is withdrawn.
    resume: (Phase, Option<&'static str>),
    /// The set has not run here with a module that reads a settings document
    /// carrying a choice of automatic updates.
    predates_automatic_updates: bool,
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
    versions: Versions,
    restoring: Option<Restoring>,
    /// When to ask the host next whether it takes restores, and how many
    /// more times. None once it has said yes or the asking has run out.
    host_probe: Option<(Instant, u32)>,
    /// The last check found the repository serving a catalog older than the
    /// one this host already accepted.
    repository_behind: bool,
    automatic: Automatic,
}

impl<C: Connector> Service<C> {
    pub(crate) fn new(connector: C, environment: Environment) -> Self {
        let failed = environment.failed_activation.is_some();
        Self {
            versions: Versions::start(&environment),
            restoring: None,
            host_probe: Some((Instant::now(), HOST_PROBES)),
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
            repository_behind: false,
            automatic: Automatic::starting(Instant::now()),
        }
    }

    /// Follows the administrator's saved choice of automatic updates.
    pub(crate) fn select_automatic(&mut self, mode: AutomaticMode) {
        self.automatic.select(mode);
    }

    /// One host tick: retires a finished worker, then starts the automatic
    /// step that is due. Never waits. Deciding whether a step is due reads
    /// nothing but memory; starting one does on this thread what the
    /// console's request for that step does, and no more often than once for
    /// each check: a check or a download spawns the worker, and an
    /// activation reads the selection to build the host's request.
    pub(crate) fn tick(
        &mut self,
        now: Instant,
        host: &mut dyn ActivationHost,
        other_install_active: bool,
    ) {
        self.retire_finished_worker(now);
        self.start_automatic_step(now, host, other_install_active);
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

    /// Answers a request about the retained sets: the list of them, or a
    /// return to one. A restore is refused or answered with the status.
    ///
    /// `settings_carry_automatic` is what the module's settings document
    /// says, or may be about to say, about automatic updates: true when it
    /// carries a choice other than off, and None when it has not been read.
    pub(crate) fn versions(
        &mut self,
        request: VersionRequest,
        host: &mut dyn ActivationHost,
        other_install_active: bool,
        settings_carry_automatic: Option<bool>,
    ) -> Value {
        self.advance();
        if !host.restore_supported() {
            return match request {
                VersionRequest::List => super::versions::unsupported(),
                VersionRequest::Restore { .. } => {
                    json!({"ok": false, "code": "unsupported_platform"})
                }
            };
        }
        self.host_switches();
        match request {
            VersionRequest::List => self.versions.reply(amaterasu_package::unix_timestamp()),
            VersionRequest::Restore { digest } => {
                let outcome = self
                    .admit(other_install_active)
                    .and_then(|()| self.request_restore(&digest, host, settings_carry_automatic));
                match outcome {
                    Ok(()) => self.status(),
                    Err(code) => json!({"ok": false, "code": code}),
                }
            }
        }
    }

    /// True while the host holds or performs a return to a set that has not
    /// run here with automatic updates. Turning them on meanwhile would leave
    /// that set with settings its module cannot read.
    pub(crate) fn restoring_set_from_before_automatic_updates(&self) -> bool {
        self.restoring
            .as_ref()
            .is_some_and(|restoring| restoring.predates_automatic_updates)
    }

    /// Learns, shortly after start, whether the host takes restore requests.
    /// Called on every tick: it compares two instants, and asks the host at
    /// most `HOST_PROBES` times in all.
    pub(crate) fn observe_host(&mut self, host: &mut dyn ActivationHost) {
        let Some((due, remaining)) = self.host_probe else {
            return;
        };
        let now = Instant::now();
        if now < due {
            return;
        }
        if host.restore_supported() {
            self.host_switches();
        } else {
            self.host_probe = remaining
                .checked_sub(1)
                .map(|remaining| (now + HOST_PROBE_INTERVAL, remaining));
        }
    }

    /// The host takes restore requests, so this set's console can switch
    /// sets. The set records that about itself, once.
    fn host_switches(&mut self) {
        self.host_probe = None;
        self.versions.host_switches();
    }

    /// Asks the host to return to a listed set. The request names the set and
    /// the selection it was made against; the host decides everything else.
    fn request_restore(
        &mut self,
        digest: &str,
        host: &mut dyn ActivationHost,
        settings_carry_automatic: Option<bool>,
    ) -> Result<(), Refusal> {
        if self.versions.loading() {
            return Err("busy");
        }
        let (Some(target), Some(root)) = (
            self.versions
                .restorable(digest, amaterasu_package::unix_timestamp()),
            self.environment.package_root.clone(),
        ) else {
            return Err("not_restorable");
        };
        // A management module from before automatic updates cannot read a
        // settings document that carries the choice, and a module that
        // cannot read its settings admits nobody to the console. Only a set
        // that recorded itself here is known to have a module that reads it:
        // the record is written by modules that do. Any other set is refused
        // before the host is asked, until the choice is off and saved.
        if !target.switches_back {
            match settings_carry_automatic {
                Some(false) => {}
                Some(true) => return Err("automatic_updates_on"),
                None => return Err("settings_unavailable"),
            }
        }
        let request = match host_activation::restore_request(&root, &target.digest) {
            Ok(request) => request,
            Err(RequestRefusal::ActivationInProgress) => return Err("activation_in_progress"),
            Err(RequestRefusal::SelectionUnavailable) => {
                self.fail("activation_unavailable");
                return Ok(());
            }
        };
        if Some(&request.expected_active) != self.environment.running_digest.as_ref() {
            // The selection no longer names the set this module was loaded from.
            self.fail("activation_unavailable");
            return Ok(());
        }
        let resume = (self.phase, self.error);
        match host.restore(request) {
            Admission::Accepted => {
                self.phase = Phase::WaitingStream;
                self.error = None;
                self.restoring = Some(Restoring {
                    digest: target.digest,
                    sequence: target.sequence,
                    resume,
                    predates_automatic_updates: !target.switches_back,
                });
                Ok(())
            }
            Admission::InProgress => Err("activation_in_progress"),
            Admission::Unavailable => {
                self.fail("activation_unavailable");
                Ok(())
            }
        }
    }

    /// Retires a finished worker without waiting for a running one.
    pub(crate) fn advance(&mut self) {
        self.retire_finished_worker(Instant::now());
    }

    fn retire_finished_worker(&mut self, now: Instant) {
        self.versions.advance();
        if !self.job.as_ref().is_some_and(Job::is_finished) {
            return;
        }
        match self.job.take().expect("finished job exists") {
            Job::Check(thread) => {
                let outcome = thread
                    .join()
                    .unwrap_or(Err(Failure::Failed("repository_unavailable")));
                self.finish_check(outcome, now);
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
        // A read of the retained sets is short and is not interrupted.
        self.job.is_none() && !self.versions.reading()
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

    /// Starts the automatic step that is due, when the administrator chose
    /// automatic updates and nothing else is under way. A step that cannot
    /// start waits; one that starts and fails is left until the next check.
    fn start_automatic_step(
        &mut self,
        now: Instant,
        host: &mut dyn ActivationHost,
        other_install_active: bool,
    ) {
        if self.environment.source.is_none() {
            return;
        }
        let Some(step) = self.automatic.next_step(now, self.automatic_progress()) else {
            return;
        };
        if self.admit(other_install_active).is_err() {
            return;
        }
        let started = match step {
            Step::Check => self.begin_scheduled_check(now),
            Step::Download => self.begin_download(),
            // The saved choice is the administrator's authorization. The
            // host decides about the request exactly as it does for a
            // console request.
            Step::Activate => self.request_activation(host),
        };
        if started.is_err() {
            self.automatic.hold();
        }
    }

    /// What the automatic flow may continue from. A set the host already
    /// failed to activate is left to the administrator: asking again on a
    /// schedule would restart the service into the same failure.
    fn automatic_progress(&self) -> Progress {
        match self.phase {
            Phase::Available if !self.offers_failed_activation() => Progress::Available,
            Phase::Ready if self.may_activate_unattended() => Progress::Ready,
            _ => Progress::Settled,
        }
    }

    /// False when the host's receipt speaks against asking for the offered
    /// set with nobody watching.
    fn may_activate_unattended(&self) -> bool {
        match &self.environment.unattended_caution {
            UnattendedCaution::None => true,
            UnattendedCaution::AnySet => false,
            UnattendedCaution::Set(digest) => self
                .offer
                .as_ref()
                .is_none_or(|offer| offer.candidate.manifest_sha256 != *digest),
        }
    }

    /// True when the offered set is the one the host's last activation
    /// failed to reach.
    fn offers_failed_activation(&self) -> bool {
        match (&self.offer, &self.environment.failed_activation) {
            (Some(offer), Some(failed)) => offer.candidate.manifest_sha256 == *failed,
            _ => false,
        }
    }

    fn begin_scheduled_check(&mut self, now: Instant) -> Result<(), Refusal> {
        self.automatic.scheduled_check_starting(now);
        self.begin_check()?;
        self.automatic.scheduled_check_started();
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
        self.repository_behind = false;
        self.automatic.work_started();
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
        self.automatic.work_started();
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
                match self.restoring.take() {
                    // A withdrawn restore leaves the update where it stood.
                    // Nothing of the update's was stopped, so the automatic
                    // flow goes on from there as it would have.
                    Some(restoring) => (self.phase, self.error) = restoring.resume,
                    // The candidate is still verified in the cache, and an
                    // activation the administrator withdrew is not asked for
                    // again before the next check.
                    None => {
                        self.phase = Phase::Ready;
                        self.error = None;
                        self.automatic.hold();
                    }
                }
                Ok(())
            }
            _ => {
                if self.job.is_some() {
                    self.cancelled.store(true, Ordering::Release);
                }
                // What the administrator stopped does not start again on
                // its own before the next check.
                self.automatic.hold();
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
            self.restoring = None;
            self.fail("activation_interrupted");
        }
    }

    fn finish_check(&mut self, outcome: Result<Checked, Failure>, now: Instant) {
        let scheduled = self.automatic.check_finished(now);
        let checked = match outcome {
            Ok(checked) => checked,
            Err(failure) => return self.stop(failure),
        };
        self.checked_unix_ms = Some(unix_milliseconds());
        self.error = None;
        match checked {
            Checked::Current => self.phase = Phase::Current,
            Checked::RepositoryBehind => {
                self.phase = Phase::Current;
                self.repository_behind = true;
            }
            Checked::Available(offer) => {
                self.described_offer = self
                    .environment
                    .source
                    .as_ref()
                    .map(|source| offer.describe(&source.repository));
                self.offer = Some(Arc::from(offer));
                self.phase = Phase::Available;
                if scheduled && self.offers_failed_activation() {
                    // Nobody asked for this check, so it must not replace
                    // the report of a failure nobody has seen.
                    self.fail("activation_failed");
                }
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
        self.automatic.hold();
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
            "restoring": self.restoring.as_ref().map(|restoring| json!({
                "digest": restoring.digest,
                "sequence": restoring.sequence.to_string(),
            })),
            "repository_behind": self.repository_behind,
            "automatic": self.automatic.describe(environment.source.is_some()),
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

    /// Reads the retained sets again now, as an aged listing is on request.
    pub(crate) fn reread_versions(&mut self) {
        self.versions.reread();
    }

    /// Waits for the retained sets to be read and the running set recorded.
    pub(crate) fn settle_versions(&mut self) {
        self.versions.settle();
    }

    /// True while the service still means to ask the host about restores.
    pub(crate) fn probing_host(&self) -> bool {
        self.host_probe.is_some()
    }

    /// True while a worker thread has not been retired.
    pub(crate) fn has_worker(&self) -> bool {
        self.job.is_some()
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
