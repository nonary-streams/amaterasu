//! Correlates internal launch requests with authenticated extension sessions.
use crate::{connector, discovery::ProcessImage, focus::FocusPolicy, snapshot::valid_playnite_id};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const MAX_ACTIVE_LAUNCHES: usize = 8;
const MAX_RETAINED_LAUNCHES: usize = 32;
const LAUNCH_RETENTION: Duration = Duration::from_secs(300);
const MAX_TICKET_BYTES: usize = 48;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum State {
    Starting,
    Running,
    Stopping,
    Stopped,
    LaunchFailed,
    CleanupFailed,
}

impl State {
    fn wire(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Stopping => "stopping",
            Self::Stopped => "stopped",
            Self::LaunchFailed => "launch_failed",
            Self::CleanupFailed => "cleanup_failed",
        }
    }

    fn terminal(self) -> bool {
        matches!(
            self,
            Self::Stopped | Self::LaunchFailed | Self::CleanupFailed
        )
    }
}

struct Launch {
    operation_id: String,
    game_id: String,
    processes: Vec<ProcessImage>,
    trusted_install_roots: BTreeSet<String>,
    focus_policy: FocusPolicy,
    ticket: String,
    state: State,
    stop_requested: bool,
    changed_at: Instant,
    stop_sender: Option<SyncSender<StopRequest>>,
    events: Option<Receiver<State>>,
    _worker: Option<JoinHandle<()>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct StopRequest {
    pub(crate) timeout_seconds: u64,
    pub(crate) processes: Vec<ProcessImage>,
}

#[derive(Default)]
pub struct Launches {
    by_ticket: BTreeMap<String, Launch>,
}

impl Launches {
    /// Close worker command channels and poll retirement without blocking the host.
    pub(crate) fn quiesce(&mut self) -> bool {
        let mut retired = true;
        for launch in self.by_ticket.values_mut() {
            retired &= launch.retire_previous_worker();
        }
        retired
    }

    pub fn active_count(&self) -> usize {
        self.by_ticket
            .values()
            .filter(|launch| !launch.state.terminal())
            .count()
    }

    pub fn advance(&mut self) {
        for launch in self.by_ticket.values_mut() {
            while let Some(event) = launch.events.as_ref().map(|events| events.try_recv()) {
                match event {
                    Ok(state) => launch.apply_state(state),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        launch.events = None;
                        if !launch.state.terminal() {
                            launch.fail_cleanup();
                        }
                        break;
                    }
                }
            }
        }
        self.prune();
    }

    pub fn start(
        &mut self,
        operation_id: &str,
        game_id: &str,
        eligible_game_ids: &BTreeSet<String>,
        processes: Vec<ProcessImage>,
        focus_policy: FocusPolicy,
    ) -> Value {
        self.prune();
        if !valid_operation_id(operation_id) || !valid_playnite_id(game_id) {
            return not_started_error("invalid_request");
        }
        let game_id = game_id.to_ascii_lowercase();
        if !eligible_game_ids.contains(&game_id) {
            return not_started_error("game_not_in_complete_snapshot");
        }
        if let Some(existing) = self
            .by_ticket
            .values()
            .find(|launch| launch.operation_id == operation_id)
        {
            return if existing.game_id == game_id {
                existing.response()
            } else {
                not_started_error("operation_conflict")
            };
        }
        if self
            .by_ticket
            .values()
            .filter(|launch| !launch.state.terminal())
            .count()
            >= MAX_ACTIVE_LAUNCHES
            || self.by_ticket.len() >= MAX_RETAINED_LAUNCHES
        {
            return not_started_error("launch_limit");
        }
        let ticket = match new_ticket() {
            Ok(ticket) => ticket,
            Err(code) => return not_started_error(code),
        };
        let mut launch = Launch {
            operation_id: operation_id.into(),
            game_id,
            trusted_install_roots: installation_roots(&processes),
            focus_policy,
            processes,
            ticket: ticket.clone(),
            state: State::Starting,
            stop_requested: false,
            changed_at: Instant::now(),
            stop_sender: None,
            events: None,
            _worker: None,
        };
        launch.start_worker(false, None);
        let response = launch.response();
        self.by_ticket.insert(ticket, launch);
        response
    }

    pub fn recover_stop(
        &mut self,
        operation_id: &str,
        game_id: &str,
        timeout_seconds: u64,
        processes: Vec<ProcessImage>,
    ) -> Value {
        self.advance();
        if !valid_operation_id(operation_id)
            || !valid_playnite_id(game_id)
            || !(1..=300).contains(&timeout_seconds)
        {
            return error("invalid_request");
        }
        let game_id = game_id.to_ascii_lowercase();
        if let Some(existing) = self
            .by_ticket
            .values_mut()
            .find(|launch| launch.operation_id == operation_id)
        {
            if existing.game_id != game_id {
                return error("operation_conflict");
            }
            if existing.state == State::Stopped || existing.state == State::LaunchFailed {
                return existing.response();
            }
            if existing.state == State::CleanupFailed {
                let Ok(processes) =
                    refresh_processes_for_install(&existing.trusted_install_roots, processes)
                else {
                    return existing.response();
                };
                existing.processes = processes;
                existing.start_worker(true, Some(timeout_seconds));
            } else if !existing.stop_requested {
                let Ok(processes) =
                    refresh_processes_for_install(&existing.trusted_install_roots, processes)
                else {
                    existing.fail_cleanup();
                    return existing.response();
                };
                existing.processes = processes.clone();
                if existing.stop_sender.as_ref().is_none_or(|sender| {
                    sender
                        .try_send(StopRequest {
                            timeout_seconds,
                            processes,
                        })
                        .is_err()
                }) {
                    existing.fail_cleanup();
                    return existing.response();
                }
                existing.stop_requested = true;
                existing.apply_state(State::Stopping);
            }
            return existing.response();
        }
        if self.by_ticket.len() >= MAX_RETAINED_LAUNCHES
            || self
                .by_ticket
                .values()
                .filter(|launch| !launch.state.terminal())
                .count()
                >= MAX_ACTIVE_LAUNCHES
        {
            return error("launch_limit");
        }
        let ticket = match new_ticket() {
            Ok(ticket) => ticket,
            Err(code) => return error(code),
        };
        let mut launch = Launch {
            operation_id: operation_id.into(),
            game_id,
            trusted_install_roots: installation_roots(&processes),
            focus_policy: FocusPolicy::disabled(),
            processes,
            ticket: ticket.clone(),
            state: State::Stopping,
            stop_requested: true,
            changed_at: Instant::now(),
            stop_sender: None,
            events: None,
            _worker: None,
        };
        launch.start_worker(true, Some(timeout_seconds));
        let response = launch.response();
        self.by_ticket.insert(ticket, launch);
        response
    }

    pub fn lookup(&mut self, operation_id: &str) -> Value {
        self.advance();
        self.by_ticket
            .values()
            .find(|launch| launch.operation_id == operation_id)
            .map(Launch::response)
            .unwrap_or_else(|| error("launch_not_found"))
    }

    pub fn status(&mut self, ticket: &str) -> Value {
        self.advance();
        if !valid_ticket(ticket) {
            return error("invalid_ticket");
        }
        self.by_ticket
            .get(ticket)
            .map(Launch::response)
            .unwrap_or_else(|| error("launch_not_found"))
    }

    pub fn stop(
        &mut self,
        ticket: &str,
        timeout_seconds: u64,
        processes: Vec<ProcessImage>,
    ) -> Value {
        if !valid_ticket(ticket) || !(1..=300).contains(&timeout_seconds) {
            return error("invalid_request");
        }
        self.advance();
        let Some(launch) = self.by_ticket.get_mut(ticket) else {
            return error("launch_not_found");
        };
        if launch.state == State::Stopped {
            return launch.response();
        }
        if launch.state == State::LaunchFailed || launch.state == State::CleanupFailed {
            if launch.state == State::LaunchFailed {
                return launch.response();
            }
            let Ok(processes) =
                refresh_processes_for_install(&launch.trusted_install_roots, processes)
            else {
                return launch.response();
            };
            launch.processes = processes;
            launch.start_worker(true, Some(timeout_seconds));
            return launch.response();
        }
        if !launch.stop_requested {
            let Ok(processes) =
                refresh_processes_for_install(&launch.trusted_install_roots, processes)
            else {
                launch.fail_cleanup();
                return launch.response();
            };
            launch.processes = processes.clone();
            if launch.stop_sender.as_ref().is_none_or(|sender| {
                sender
                    .try_send(StopRequest {
                        timeout_seconds,
                        processes,
                    })
                    .is_err()
            }) {
                launch.fail_cleanup();
                return launch.response();
            }
            launch.stop_requested = true;
            launch.apply_state(State::Stopping);
        }
        launch.response()
    }

    fn prune(&mut self) {
        let now = Instant::now();
        self.by_ticket.retain(|_, launch| {
            !launch.state.terminal()
                || launch
                    ._worker
                    .as_ref()
                    .is_some_and(|worker| !worker.is_finished())
                || now.duration_since(launch.changed_at) < LAUNCH_RETENTION
        });
    }
}

impl Drop for Launch {
    fn drop(&mut self) {
        // A native module must not unload while its worker is executing code.
        self.stop_sender.take();
        if let Some(worker) = self._worker.take() {
            let _ = worker.join();
        }
    }
}

impl Launch {
    fn start_worker(&mut self, recovery: bool, timeout_seconds: Option<u64>) {
        if !self.retire_previous_worker() {
            return;
        }
        let (stop_sender, stop_receiver) = mpsc::sync_channel(1);
        let (event_sender, events) = mpsc::channel();
        if let Some(timeout_seconds) = timeout_seconds {
            let _ = stop_sender.try_send(StopRequest {
                timeout_seconds,
                processes: self.processes.clone(),
            });
        }
        let processes = self.processes.clone();
        let game_id = self.game_id.clone();
        let focus_policy = self.focus_policy;
        let worker = thread::spawn(move || {
            connector::watch_game(
                &processes,
                &game_id,
                stop_receiver,
                event_sender,
                recovery,
                focus_policy,
            );
        });
        self.stop_sender = Some(stop_sender);
        self.events = Some(events);
        self._worker = Some(worker);
        self.state = if recovery {
            State::Stopping
        } else {
            State::Starting
        };
        self.stop_requested = recovery;
        self.changed_at = Instant::now();
    }

    fn retire_previous_worker(&mut self) -> bool {
        self.stop_sender = None;
        if self
            ._worker
            .as_ref()
            .is_some_and(|worker| !worker.is_finished())
        {
            return false;
        }
        if let Some(worker) = self._worker.take() {
            let _ = worker.join();
        }
        self.events = None;
        true
    }

    fn fail_cleanup(&mut self) {
        self.stop_sender = None;
        self.apply_state(State::CleanupFailed);
    }

    fn apply_state(&mut self, state: State) {
        if self.state.terminal() {
            return;
        }
        // The launch event can arrive after stop was queued. It must not undo
        // the stop state while the correlated gameStopped event is outstanding.
        if self.state == State::Stopping && state == State::Running {
            return;
        }
        if self.state != state {
            self.state = state;
            self.changed_at = Instant::now();
        }
    }

    fn response(&self) -> Value {
        json!({
            "ok":true,
            "ticket":self.ticket,
            "game_id":self.game_id,
            "state":self.state.wire()
        })
    }
}

fn new_ticket() -> Result<String, &'static str> {
    let mut bytes = [0; 24];
    getrandom::fill(&mut bytes).map_err(|_| "entropy_unavailable")?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

pub(crate) fn installation_roots(processes: &[ProcessImage]) -> BTreeSet<String> {
    processes
        .iter()
        .filter_map(|process| process_installation_root(&process.path))
        .collect()
}

pub(crate) fn refresh_processes_for_install(
    trusted_roots: &BTreeSet<String>,
    processes: Vec<ProcessImage>,
) -> Result<Vec<ProcessImage>, &'static str> {
    if trusted_roots.is_empty() {
        return Err("process_identity_unavailable");
    }
    let mut seen_process_ids = BTreeSet::new();
    let refreshed: Vec<_> = processes
        .into_iter()
        .filter(|process| {
            process.process_id != 0
                && seen_process_ids.insert(process.process_id)
                && process_installation_root(&process.path)
                    .is_some_and(|root| trusted_roots.contains(&root))
        })
        .collect();
    if refreshed.is_empty() {
        return Err("process_identity_changed");
    }
    Ok(refreshed)
}

fn process_installation_root(path: &str) -> Option<String> {
    let normalized = path.replace('/', "\\");
    let (root, executable) = normalized.rsplit_once('\\')?;
    let filename = executable.to_ascii_lowercase();
    if !matches!(
        filename.as_str(),
        "playnite.desktopapp.exe" | "playnite.fullscreenapp.exe"
    ) || normalized.chars().any(char::is_control)
        || normalized
            .split('\\')
            .any(|segment| segment == "." || segment == "..")
    {
        return None;
    }
    let bytes = normalized.as_bytes();
    let drive_absolute =
        bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && bytes[2] == b'\\';
    let unc_absolute = normalized.starts_with("\\\\");
    if !drive_absolute && !unc_absolute {
        return None;
    }
    Some(root.to_ascii_lowercase())
}

fn valid_operation_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= 128 && value.bytes().all(|byte| byte.is_ascii_graphic())
}

fn valid_ticket(value: &str) -> bool {
    value.len() == MAX_TICKET_BYTES && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn error(code: &str) -> Value {
    json!({"ok":false,"code":code})
}

/// This response is emitted only before an extension worker or launch command exists.
pub fn not_started_error(code: &str) -> Value {
    json!({"ok":false,"code":code,"launch_state":"not_started"})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn launch(state: State) -> Launch {
        let (stop_sender, _stop_receiver) = mpsc::sync_channel(1);
        let (_event_sender, events) = mpsc::channel();
        Launch {
            operation_id: "launch-operation".into(),
            game_id: "a1b2c3d4-1234-5678-9abc-0123456789ab".into(),
            processes: Vec::new(),
            trusted_install_roots: BTreeSet::new(),
            focus_policy: FocusPolicy::default(),
            ticket: "ab".repeat(24),
            state,
            stop_requested: false,
            changed_at: Instant::now(),
            stop_sender: Some(stop_sender),
            events: Some(events),
            _worker: Some(thread::spawn(|| {})),
        }
    }

    fn process(process_id: u32, path: &str) -> ProcessImage {
        ProcessImage {
            process_id,
            path: path.into(),
            creation_time: u64::from(process_id),
        }
    }

    fn launch_observing_stop_request(
        state: State,
        processes: Vec<ProcessImage>,
    ) -> (Launch, Receiver<StopRequest>) {
        let (stop_sender, stop_receiver) = mpsc::sync_channel(1);
        let (observed_sender, observed_receiver) = mpsc::channel();
        let (event_sender, events) = mpsc::channel();
        let worker = thread::spawn(move || {
            if let Ok(request) = stop_receiver.recv() {
                let _ = observed_sender.send(request);
            }
            drop(event_sender);
        });
        let trusted_install_roots = installation_roots(&processes);
        (
            Launch {
                operation_id: "launch-operation".into(),
                game_id: "a1b2c3d4-1234-5678-9abc-0123456789ab".into(),
                processes,
                trusted_install_roots,
                focus_policy: FocusPolicy::default(),
                ticket: "ab".repeat(24),
                state,
                stop_requested: false,
                changed_at: Instant::now(),
                stop_sender: Some(stop_sender),
                events: Some(events),
                _worker: Some(worker),
            },
            observed_receiver,
        )
    }

    #[test]
    fn stop_state_is_not_reverted_by_a_late_start_status() {
        let mut launch = launch(State::Stopping);
        launch.apply_state(State::Running);
        assert_eq!(launch.state, State::Stopping);
        launch.apply_state(State::Stopped);
        launch.apply_state(State::Running);
        assert_eq!(launch.state, State::Stopped);
    }

    #[test]
    fn ticket_lookup_rejects_malformed_and_unowned_tickets() {
        let mut launches = Launches::default();
        launches
            .by_ticket
            .insert("ab".repeat(24), launch(State::Running));
        assert_eq!(
            launches.status(&"cd".repeat(24))["code"],
            "launch_not_found"
        );
        assert_eq!(launches.status("short")["code"], "invalid_ticket");
        assert_eq!(launches.status(&"gq".repeat(24))["code"], "invalid_ticket");
    }

    #[test]
    fn stop_timeout_is_bounded_before_any_pipe_worker_is_created() {
        let ticket = "ab".repeat(24);
        let mut launches = Launches::default();
        launches
            .by_ticket
            .insert(ticket.clone(), launch(State::Running));
        for timeout_seconds in [0, 301] {
            let response = launches.stop(&ticket, timeout_seconds, Vec::new());
            assert_eq!(response["ok"], false);
            assert_eq!(response["code"], "invalid_request");
        }
        assert_eq!(launches.by_ticket[&ticket].state, State::Running);
    }

    #[test]
    fn stop_refreshes_process_id_for_the_same_playnite_install() {
        let ticket = "ab".repeat(24);
        let original = process(41, r"C:\Program Files\Playnite\Playnite.DesktopApp.exe");
        let refreshed = process(73, r"C:\Program Files\Playnite\Playnite.FullscreenApp.exe");
        for state in [State::Starting, State::Running] {
            let (launch, observed) = launch_observing_stop_request(state, vec![original.clone()]);
            let mut launches = Launches::default();
            launches.by_ticket.insert(ticket.clone(), launch);

            let response = launches.stop(&ticket, 30, vec![refreshed.clone()]);
            assert_eq!(response["state"], "stopping");
            let request = observed.recv_timeout(Duration::from_secs(1)).unwrap();
            assert_eq!(request.timeout_seconds, 30);
            assert_eq!(request.processes, vec![refreshed.clone()]);
        }
    }

    #[test]
    fn stop_never_retargets_a_different_playnite_install() {
        let ticket = "ab".repeat(24);
        let original = process(41, r"C:\Program Files\Playnite\Playnite.DesktopApp.exe");
        let unrelated = process(73, r"D:\Untrusted\Playnite\Playnite.DesktopApp.exe");
        let (launch, observed) = launch_observing_stop_request(State::Running, vec![original]);
        let mut launches = Launches::default();
        launches.by_ticket.insert(ticket.clone(), launch);

        let response = launches.stop(&ticket, 30, vec![unrelated]);
        assert_eq!(response["state"], "cleanup_failed");
        assert!(observed.recv_timeout(Duration::from_millis(20)).is_err());
    }

    #[test]
    fn quiescence_waits_for_launch_watcher_retirement() {
        let (stop_sender, stop_receiver) = mpsc::sync_channel::<StopRequest>(1);
        let (cancelled, observed) = mpsc::channel();
        let (release, wait) = mpsc::channel();
        let worker = thread::spawn(move || {
            assert!(stop_receiver.recv().is_err());
            cancelled.send(()).unwrap();
            wait.recv().unwrap();
        });
        let mut launches = Launches::default();
        launches.by_ticket.insert(
            "ticket".into(),
            Launch {
                operation_id: "operation".into(),
                game_id: "game".into(),
                processes: Vec::new(),
                trusted_install_roots: BTreeSet::new(),
                focus_policy: FocusPolicy::default(),
                ticket: "ticket".into(),
                state: State::Running,
                stop_requested: false,
                changed_at: Instant::now(),
                stop_sender: Some(stop_sender),
                events: None,
                _worker: Some(worker),
            },
        );
        assert!(!launches.quiesce());
        observed.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(!launches.quiesce());
        release.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        while !launches.quiesce() {
            assert!(Instant::now() < deadline);
            thread::yield_now();
        }
    }

    #[test]
    fn reconnect_waits_for_the_old_watcher_after_a_failed_stop() {
        let game_id = "a1b2c3d4-1234-5678-9abc-0123456789ab";
        let original = process(41, r"C:\Program Files\Playnite\Playnite.DesktopApp.exe");
        let unrelated = process(73, r"D:\Untrusted\Playnite\Playnite.DesktopApp.exe");
        let restarted = process(91, r"C:\Program Files\Playnite\Playnite.FullscreenApp.exe");
        let (stop_sender, stop_receiver) = mpsc::sync_channel(1);
        let (cancelled_sender, cancelled_receiver) = mpsc::channel();
        let (release_sender, release_receiver) = mpsc::channel();
        let (_event_sender, events) = mpsc::channel();
        let worker = thread::spawn(move || {
            assert!(stop_receiver.recv().is_err());
            cancelled_sender.send(()).unwrap();
            release_receiver.recv().unwrap();
        });
        let ticket = "ab".repeat(24);
        let mut launches = Launches::default();
        launches.by_ticket.insert(
            ticket.clone(),
            Launch {
                operation_id: "launch-operation".into(),
                game_id: game_id.into(),
                processes: vec![original.clone()],
                trusted_install_roots: installation_roots(&[original]),
                focus_policy: FocusPolicy::default(),
                ticket: ticket.clone(),
                state: State::Running,
                stop_requested: false,
                changed_at: Instant::now(),
                stop_sender: Some(stop_sender),
                events: Some(events),
                _worker: Some(worker),
            },
        );

        assert_eq!(
            launches.stop(&ticket, 30, vec![unrelated])["state"],
            "cleanup_failed"
        );
        cancelled_receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
        assert_eq!(
            launches.recover_stop("launch-operation", game_id, 30, vec![restarted.clone()])["state"],
            "cleanup_failed"
        );
        assert!(launches.by_ticket[&ticket].stop_sender.is_none());
        launches.by_ticket.get_mut(&ticket).unwrap().changed_at =
            Instant::now() - LAUNCH_RETENTION - Duration::from_secs(1);
        launches.advance();
        assert!(launches.by_ticket.contains_key(&ticket));
        launches.by_ticket.get_mut(&ticket).unwrap().changed_at = Instant::now();
        release_sender.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        while !launches.by_ticket[&ticket]
            ._worker
            .as_ref()
            .unwrap()
            .is_finished()
        {
            assert!(Instant::now() < deadline, "old watcher did not finish");
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            launches.recover_stop("launch-operation", game_id, 30, vec![restarted])["state"],
            "stopping"
        );
        assert!(launches.by_ticket[&ticket].stop_sender.is_some());
    }

    #[test]
    fn recover_stop_does_not_claim_stopping_when_the_bounded_channel_is_full() {
        let game_id = "a1b2c3d4-1234-5678-9abc-0123456789ab";
        let original = process(41, r"C:\Program Files\Playnite\Playnite.DesktopApp.exe");
        let refreshed = process(73, r"C:\Program Files\Playnite\Playnite.FullscreenApp.exe");
        let (stop_sender, stop_receiver) = mpsc::sync_channel(1);
        let queued = StopRequest {
            timeout_seconds: 15,
            processes: vec![original.clone()],
        };
        stop_sender.try_send(queued.clone()).unwrap();
        let (_event_sender, events) = mpsc::channel();
        let mut launches = Launches::default();
        launches.by_ticket.insert(
            "ab".repeat(24),
            Launch {
                operation_id: "launch-operation".into(),
                game_id: game_id.into(),
                processes: vec![original.clone()],
                trusted_install_roots: installation_roots(&[original]),
                focus_policy: FocusPolicy::default(),
                ticket: "ab".repeat(24),
                state: State::Running,
                stop_requested: false,
                changed_at: Instant::now(),
                stop_sender: Some(stop_sender),
                events: Some(events),
                _worker: Some(thread::spawn(|| {})),
            },
        );

        let response = launches.recover_stop("launch-operation", game_id, 30, vec![refreshed]);
        assert_eq!(response["state"], "cleanup_failed");
        let launch = launches.by_ticket.values().next().unwrap();
        assert!(!launch.stop_requested);
        assert_eq!(launch.state, State::CleanupFailed);
        assert_eq!(stop_receiver.try_recv().unwrap(), queued);
    }

    #[test]
    fn start_requires_membership_in_the_latest_complete_selected_snapshot() {
        let mut launches = Launches::default();
        let result = launches.start(
            "operation.1",
            "a1b2c3d4-1234-5678-9abc-0123456789ab",
            &BTreeSet::new(),
            Vec::new(),
            FocusPolicy::default(),
        );
        assert_eq!(result["code"], "game_not_in_complete_snapshot");
        let malformed = launches.start(
            "operation.2",
            "not-a-guid",
            &BTreeSet::from(["not-a-guid".into()]),
            Vec::new(),
            FocusPolicy::default(),
        );
        assert_eq!(malformed["code"], "invalid_request");
        assert!(launches.by_ticket.is_empty());
    }

    #[test]
    fn operation_id_is_idempotent_but_cannot_retarget_another_guid() {
        let game_id = "a1b2c3d4-1234-5678-9abc-0123456789ab";
        let other_id = "b1b2c3d4-1234-5678-9abc-0123456789ab";
        let (stop_sender, _) = mpsc::sync_channel(1);
        let (_event_sender, events) = mpsc::channel();
        let worker = thread::spawn(|| {});
        let mut launches = Launches::default();
        launches.by_ticket.insert(
            "ab".repeat(24),
            Launch {
                operation_id: "operation.3".into(),
                game_id: game_id.into(),
                processes: Vec::new(),
                trusted_install_roots: BTreeSet::new(),
                focus_policy: FocusPolicy::default(),
                ticket: "ab".repeat(24),
                state: State::Running,
                stop_requested: false,
                changed_at: Instant::now(),
                stop_sender: Some(stop_sender),
                events: Some(events),
                _worker: Some(worker),
            },
        );
        let same = launches.start(
            "operation.3",
            game_id,
            &BTreeSet::from([game_id.into()]),
            Vec::new(),
            FocusPolicy::default(),
        );
        assert_eq!(same["ticket"], "ab".repeat(24));
        let changed = launches.start(
            "operation.3",
            other_id,
            &BTreeSet::from([other_id.into()]),
            Vec::new(),
            FocusPolicy::default(),
        );
        assert_eq!(changed["code"], "operation_conflict");
    }
}
