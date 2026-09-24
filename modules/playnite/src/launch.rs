//! Correlates internal launch requests with authenticated extension sessions.
use crate::{connector, discovery::ProcessImage, snapshot::valid_playnite_id};
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
    ticket: String,
    state: State,
    stop_requested: bool,
    changed_at: Instant,
    stop_sender: Option<SyncSender<u64>>,
    events: Option<Receiver<State>>,
    _worker: Option<JoinHandle<()>>,
}

#[derive(Default)]
pub struct Launches {
    by_ticket: BTreeMap<String, Launch>,
}

impl Launches {
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
                            launch.apply_state(State::CleanupFailed);
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
                existing.processes = processes;
                existing.start_worker(true, Some(timeout_seconds));
            } else if !existing.stop_requested {
                let _ = existing
                    .stop_sender
                    .as_ref()
                    .map(|sender| sender.try_send(timeout_seconds));
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
            launch.processes = processes;
            launch.start_worker(true, Some(timeout_seconds));
            return launch.response();
        }
        if !launch.stop_requested {
            if launch
                .stop_sender
                .as_ref()
                .is_none_or(|sender| sender.try_send(timeout_seconds).is_err())
            {
                launch.apply_state(State::CleanupFailed);
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
            !launch.state.terminal() || now.duration_since(launch.changed_at) < LAUNCH_RETENTION
        });
    }
}

impl Launch {
    fn start_worker(&mut self, recovery: bool, timeout_seconds: Option<u64>) {
        let (stop_sender, stop_receiver) = mpsc::sync_channel(1);
        let (event_sender, events) = mpsc::channel();
        if let Some(timeout_seconds) = timeout_seconds {
            let _ = stop_sender.try_send(timeout_seconds);
        }
        let processes = self.processes.clone();
        let game_id = self.game_id.clone();
        let worker = thread::spawn(move || {
            connector::watch_game(&processes, &game_id, stop_receiver, event_sender, recovery);
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
            ticket: "ab".repeat(24),
            state,
            stop_requested: false,
            changed_at: Instant::now(),
            stop_sender: Some(stop_sender),
            events: Some(events),
            _worker: Some(thread::spawn(|| {})),
        }
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
    fn start_requires_membership_in_the_latest_complete_selected_snapshot() {
        let mut launches = Launches::default();
        let result = launches.start(
            "operation.1",
            "a1b2c3d4-1234-5678-9abc-0123456789ab",
            &BTreeSet::new(),
            Vec::new(),
        );
        assert_eq!(result["code"], "game_not_in_complete_snapshot");
        let malformed = launches.start(
            "operation.2",
            "not-a-guid",
            &BTreeSet::from(["not-a-guid".into()]),
            Vec::new(),
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
        );
        assert_eq!(same["ticket"], "ab".repeat(24));
        let changed = launches.start(
            "operation.3",
            other_id,
            &BTreeSet::from([other_id.into()]),
            Vec::new(),
        );
        assert_eq!(changed["code"], "operation_conflict");
    }
}
