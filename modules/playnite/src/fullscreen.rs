//! Ticketed Playnite Fullscreen UI sessions bound to one trusted installation.

use crate::discovery::ProcessImage;
use crate::focus::{FocusPolicy, FocusRetry};
use crate::installation::Installation;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

const START_TIMEOUT: Duration = Duration::from_secs(30);
const STARTUP_SETTLE_GRACE: Duration = Duration::from_secs(2);
const CLOSE_RETRY_INTERVAL: Duration = Duration::from_secs(1);
const SESSION_RETENTION: Duration = Duration::from_secs(300);
const MAX_SESSIONS: usize = 32;
const MAX_EXIT_TIMEOUT_SECONDS: u64 = 300;
const ZERO_TIMEOUT_CLOSE_GRACE: Duration = Duration::from_secs(20);
const MAX_TICKET_BYTES: usize = 48;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum State {
    Starting,
    Running,
    Stopping,
    Stopped,
    LaunchFailed,
    CleanupFailed,
}

impl State {
    fn terminal(self) -> bool {
        self == Self::Stopped
    }

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
}

struct Session {
    operation_id: String,
    ticket: String,
    installation: Installation,
    launched_process_id: u32,
    state: State,
    started_at: Instant,
    state_changed_at: Instant,
    stop_deadline: Option<Instant>,
    stop_requested_at: Option<Instant>,
    last_close_request_at: Option<Instant>,
    focus_retry: FocusRetry,
}

#[derive(Default)]
pub struct FullscreenSessions {
    by_ticket: BTreeMap<String, Session>,
}

impl FullscreenSessions {
    pub fn active_count(&self) -> usize {
        self.by_ticket
            .values()
            .filter(|session| !session.state.terminal())
            .count()
    }

    pub fn advance(&mut self, now: Instant, processes: &[ProcessImage]) {
        self.prune(now);
        for session in self.by_ticket.values_mut() {
            match session.state {
                State::Starting => {
                    session
                        .focus_retry
                        .tick_with(now, FocusRetry::attempt_windows_focus);
                    if !session
                        .installation
                        .fullscreen_processes(processes)
                        .is_empty()
                    {
                        session.transition(State::Running, now);
                    } else if now.saturating_duration_since(session.started_at) >= START_TIMEOUT {
                        session.transition(State::LaunchFailed, now);
                    }
                }
                State::Running => {
                    session
                        .focus_retry
                        .tick_with(now, FocusRetry::attempt_windows_focus);
                    if session
                        .installation
                        .fullscreen_processes(processes)
                        .is_empty()
                    {
                        // The helper's active-game watcher is not implemented yet.
                        // Treat disappearance as ambiguous and retain cleanup ownership.
                        session.transition(State::CleanupFailed, now);
                    }
                }
                State::LaunchFailed => {
                    if !session
                        .installation
                        .fullscreen_processes(processes)
                        .is_empty()
                    {
                        session.transition(State::Running, now);
                    }
                }
                State::CleanupFailed => {
                    // An empty discovery can be transient. If no stop was
                    // requested and the exact UI reappears, resume tracking;
                    // otherwise retain the ticket for explicit recovery.
                    if session.stop_requested_at.is_none()
                        && !session
                            .installation
                            .fullscreen_processes(processes)
                            .is_empty()
                    {
                        session.transition(State::Running, now);
                    }
                }
                State::Stopping => {
                    session.focus_retry.cancel();
                    let fullscreen_processes = session.installation.fullscreen_processes(processes);
                    if fullscreen_processes.is_empty() {
                        let launch_process_alive = session
                            .installation
                            .launch_process_is_running(session.launched_process_id, processes);
                        let settle_elapsed = session.stop_requested_at.is_some_and(|requested| {
                            now.saturating_duration_since(requested) >= STARTUP_SETTLE_GRACE
                        });
                        let deadline_elapsed = session
                            .stop_deadline
                            .is_some_and(|deadline| now >= deadline);
                        if !launch_process_alive && (settle_elapsed || deadline_elapsed) {
                            session.transition(State::Stopped, now);
                        } else if deadline_elapsed {
                            session.transition(State::CleanupFailed, now);
                        }
                    } else {
                        let should_retry_close = session.last_close_request_at.is_none_or(|last| {
                            now.saturating_duration_since(last) >= CLOSE_RETRY_INTERVAL
                        });
                        if should_retry_close {
                            if session
                                .installation
                                .request_fullscreen_close(processes)
                                .is_err()
                            {
                                session.transition(State::CleanupFailed, now);
                            } else {
                                session.last_close_request_at = Some(now);
                            }
                        }
                        if session
                            .stop_deadline
                            .is_some_and(|deadline| now >= deadline)
                        {
                            session.transition(State::CleanupFailed, now);
                        }
                    }
                }
                State::Stopped => {}
            }
        }
    }

    pub fn start(
        &mut self,
        operation_id: &str,
        installation: Option<Installation>,
        processes: &[ProcessImage],
        focus_policy: FocusPolicy,
    ) -> Value {
        self.start_with(
            operation_id,
            installation,
            processes,
            focus_policy,
            Instant::now(),
            Installation::launch_fullscreen,
        )
    }

    fn start_with(
        &mut self,
        operation_id: &str,
        installation: Option<Installation>,
        processes: &[ProcessImage],
        focus_policy: FocusPolicy,
        now: Instant,
        launch: impl FnOnce(&Installation) -> Result<u32, &'static str>,
    ) -> Value {
        self.prune(now);
        if !valid_operation_id(operation_id) {
            return crate::launch::not_started_error("invalid_request");
        }
        if let Some(existing) = self
            .by_ticket
            .values()
            .find(|session| session.operation_id == operation_id)
        {
            return existing.response();
        }
        if self.active_count() > 0 || self.by_ticket.len() >= MAX_SESSIONS {
            return crate::launch::not_started_error("fullscreen_session_limit");
        }
        let Some(installation) = installation else {
            return crate::launch::not_started_error("playnite_installation_not_found");
        };
        if !installation.matches_running_processes(processes) {
            return crate::launch::not_started_error("playnite_installation_mismatch");
        }
        let ticket = match new_ticket() {
            Ok(ticket) => ticket,
            Err(code) => return crate::launch::not_started_error(code),
        };
        let launched_process_id = match launch(&installation) {
            Ok(process_id) if process_id != 0 => process_id,
            Ok(_) => return crate::launch::not_started_error("fullscreen_launch_failed"),
            Err(code) => return crate::launch::not_started_error(code),
        };
        let mut focus_retry = FocusRetry::default();
        focus_retry.start(focus_policy, installation.focus_target(), now);
        let session = Session {
            operation_id: operation_id.into(),
            ticket: ticket.clone(),
            installation,
            launched_process_id,
            state: State::Starting,
            started_at: now,
            state_changed_at: now,
            stop_deadline: None,
            stop_requested_at: None,
            last_close_request_at: None,
            focus_retry,
        };
        let response = session.response();
        self.by_ticket.insert(ticket, session);
        response
    }

    pub fn lookup(&mut self, operation_id: &str) -> Value {
        self.prune(Instant::now());
        if !valid_operation_id(operation_id) {
            return error("invalid_request");
        }
        self.by_ticket
            .values()
            .find(|session| session.operation_id == operation_id)
            .map_or_else(
                || json!({"ok":true,"found":false}),
                |session| {
                    json!({"ok":true,"found":true,"ticket":session.ticket,"state":session.state.wire()})
                },
            )
    }

    pub fn status(&mut self, ticket: &str) -> Value {
        self.prune(Instant::now());
        if !valid_ticket(ticket) {
            return error("invalid_request");
        }
        self.by_ticket
            .get(ticket)
            .map_or_else(|| error("launch_not_found"), Session::response)
    }

    pub fn stop(
        &mut self,
        ticket: &str,
        exit_timeout_seconds: u64,
        processes: &[ProcessImage],
    ) -> Value {
        self.stop_with(
            ticket,
            exit_timeout_seconds,
            processes,
            Installation::request_fullscreen_close,
        )
    }

    fn stop_with(
        &mut self,
        ticket: &str,
        exit_timeout_seconds: u64,
        processes: &[ProcessImage],
        request_close: impl FnOnce(&Installation, &[ProcessImage]) -> Result<usize, &'static str>,
    ) -> Value {
        self.stop_with_at(
            ticket,
            exit_timeout_seconds,
            processes,
            Instant::now(),
            request_close,
        )
    }

    fn stop_with_at(
        &mut self,
        ticket: &str,
        exit_timeout_seconds: u64,
        processes: &[ProcessImage],
        now: Instant,
        request_close: impl FnOnce(&Installation, &[ProcessImage]) -> Result<usize, &'static str>,
    ) -> Value {
        self.prune(Instant::now());
        if !valid_ticket(ticket) || exit_timeout_seconds > MAX_EXIT_TIMEOUT_SECONDS {
            return error("invalid_request");
        }
        let Some(session) = self.by_ticket.get_mut(ticket) else {
            return error("launch_not_found");
        };
        if !session.state.terminal() && session.state != State::Stopping {
            session.transition(State::Stopping, now);
            session.stop_requested_at = Some(now);
            let timeout = if exit_timeout_seconds == 0 {
                ZERO_TIMEOUT_CLOSE_GRACE
            } else {
                Duration::from_secs(exit_timeout_seconds)
            };
            session.stop_deadline = now.checked_add(timeout);
        }
        if session.state == State::Stopping {
            if request_close(&session.installation, processes).is_err() {
                session.transition(State::CleanupFailed, now);
            } else {
                session.last_close_request_at = Some(now);
            }
        }
        session.response()
    }

    pub fn recover_stop(
        &mut self,
        operation_id: &str,
        exit_timeout_seconds: u64,
        processes: &[ProcessImage],
    ) -> Value {
        self.prune(Instant::now());
        if !valid_operation_id(operation_id) || exit_timeout_seconds > MAX_EXIT_TIMEOUT_SECONDS {
            return error("invalid_request");
        }
        let Some(ticket) = self
            .by_ticket
            .values()
            .find(|session| session.operation_id == operation_id)
            .map(|session| session.ticket.clone())
        else {
            // Operation IDs cannot identify an owner's window after provider
            // restart, so recovery never stops a process based on discovery alone.
            return error("launch_not_found");
        };
        self.stop(&ticket, exit_timeout_seconds, processes)
    }

    fn prune(&mut self, now: Instant) {
        self.by_ticket.retain(|_, session| {
            !session.state.terminal()
                || now.saturating_duration_since(session.state_changed_at) < SESSION_RETENTION
        });
    }
}

impl Session {
    fn transition(&mut self, state: State, now: Instant) {
        if self.state != state {
            self.state = state;
            self.state_changed_at = now;
        }
    }

    fn response(&self) -> Value {
        json!({"ok":true,"started":true,"ticket":self.ticket,"state":self.state.wire()})
    }
}

fn valid_operation_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= 128 && value.bytes().all(|byte| byte.is_ascii_graphic())
}

fn valid_ticket(value: &str) -> bool {
    value.len() == MAX_TICKET_BYTES && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn new_ticket() -> Result<String, &'static str> {
    let mut bytes = [0; 24];
    getrandom::fill(&mut bytes).map_err(|_| "entropy_unavailable")?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn error(code: &str) -> Value {
    json!({"ok":false,"code":code})
}

#[cfg(test)]
mod tests {
    use super::{FullscreenSessions, START_TIMEOUT};
    use crate::discovery::ProcessImage;
    use crate::focus::FocusPolicy;
    use crate::installation::Installation;
    use std::time::{Duration, Instant};

    fn installation() -> Installation {
        Installation::for_test()
    }

    fn fullscreen_process(process_id: u32) -> ProcessImage {
        ProcessImage {
            process_id,
            path: r"C:\Playnite\Playnite.FullscreenApp.exe".into(),
        }
    }

    #[test]
    fn start_is_ticketed_idempotent_and_respects_installation_identity() {
        let now = Instant::now();
        let mut sessions = FullscreenSessions::default();
        let first = sessions.start_with(
            "operation-1",
            Some(installation()),
            &[],
            FocusPolicy::disabled(),
            now,
            |_| Ok(77),
        );
        assert_eq!(first["state"], "starting");
        assert_eq!(first["started"], true);
        let ticket = first["ticket"].as_str().unwrap();
        assert_eq!(sessions.lookup("operation-1")["ticket"], ticket);
        assert_eq!(sessions.status(ticket)["state"], "starting");
        let retry = sessions.start_with(
            "operation-1",
            Some(installation()),
            &[],
            FocusPolicy::disabled(),
            now + Duration::from_secs(1),
            |_| panic!("idempotent retry must not start another process"),
        );
        assert_eq!(retry["ticket"], ticket);
        let mut mismatch = FullscreenSessions::default();
        assert_eq!(
            mismatch.start_with(
                "operation-2",
                Some(installation()),
                &[ProcessImage {
                    process_id: 99,
                    path: r"D:\Other\Playnite.DesktopApp.exe".into(),
                }],
                FocusPolicy::disabled(),
                now,
                |_| panic!("mismatched installation must not launch"),
            )["code"],
            "playnite_installation_mismatch"
        );
    }

    #[test]
    fn missing_install_and_process_creation_failure_are_preflight_errors() {
        let now = Instant::now();
        let mut sessions = FullscreenSessions::default();
        assert_eq!(
            sessions.start_with(
                "operation-1",
                None,
                &[],
                FocusPolicy::disabled(),
                now,
                |_| panic!("missing installation must fail before process creation"),
            )["launch_state"],
            "not_started"
        );
        assert_eq!(
            sessions.start_with(
                "operation-2",
                Some(installation()),
                &[],
                FocusPolicy::disabled(),
                now,
                |_| Err("fullscreen_launch_failed"),
            )["code"],
            "fullscreen_launch_failed"
        );
        assert_eq!(sessions.active_count(), 0);
    }

    #[test]
    fn stop_is_ticketed_and_waits_for_the_fullscreen_process_to_exit() {
        let now = Instant::now();
        let mut sessions = FullscreenSessions::default();
        let started = sessions.start_with(
            "operation-1",
            Some(installation()),
            &[],
            FocusPolicy::disabled(),
            now,
            |_| Ok(77),
        );
        let ticket = started["ticket"].as_str().unwrap().to_owned();
        let running = [fullscreen_process(80)];
        sessions.advance(now + Duration::from_secs(1), &running);
        assert_eq!(sessions.status(&ticket)["state"], "running");
        assert_eq!(
            sessions.stop_with(&ticket, 10, &running, |_, _| Ok(1))["state"],
            "stopping"
        );
        sessions.advance(now + Duration::from_secs(3), &[]);
        assert_eq!(sessions.status(&ticket)["state"], "stopped");
    }

    #[test]
    fn transient_empty_discovery_retains_ticket_until_correlated_stop() {
        let now = Instant::now();
        let mut sessions = FullscreenSessions::default();
        let started = sessions.start_with(
            "operation-1",
            Some(installation()),
            &[],
            FocusPolicy::disabled(),
            now,
            |_| Ok(77),
        );
        let ticket = started["ticket"].as_str().unwrap().to_owned();
        let running = [fullscreen_process(80)];
        sessions.advance(now + Duration::from_secs(1), &running);
        assert_eq!(sessions.status(&ticket)["state"], "running");

        sessions.advance(now + Duration::from_secs(2), &[]);
        assert_eq!(sessions.status(&ticket)["state"], "cleanup_failed");
        assert_eq!(sessions.active_count(), 1);
        assert_eq!(
            sessions.start_with(
                "operation-2",
                Some(installation()),
                &[],
                FocusPolicy::disabled(),
                now + Duration::from_secs(3),
                |_| panic!("uncertain session must keep the single-session slot"),
            )["code"],
            "fullscreen_session_limit"
        );

        sessions.advance(now + Duration::from_secs(3), &running);
        assert_eq!(sessions.status(&ticket)["state"], "running");
        sessions.stop_with(&ticket, 10, &running, |_, _| Ok(1));
        sessions.advance(now + Duration::from_secs(6), &[]);
        assert_eq!(sessions.status(&ticket)["state"], "stopped");
        assert_eq!(sessions.active_count(), 0);
    }

    #[test]
    fn startup_timeout_with_live_launcher_keeps_session_slot_until_stopped() {
        let now = Instant::now();
        let mut sessions = FullscreenSessions::default();
        let started = sessions.start_with(
            "operation-1",
            Some(installation()),
            &[],
            FocusPolicy::disabled(),
            now,
            |_| Ok(77),
        );
        let ticket = started["ticket"].as_str().unwrap().to_owned();
        let launcher = [ProcessImage {
            process_id: 77,
            path: r"C:\Playnite\Playnite.DesktopApp.exe".into(),
        }];
        sessions.advance(now + START_TIMEOUT, &launcher);
        assert_eq!(sessions.status(&ticket)["state"], "launch_failed");
        assert_eq!(sessions.active_count(), 1);
        assert_eq!(
            sessions.start_with(
                "operation-2",
                Some(installation()),
                &[],
                FocusPolicy::disabled(),
                now + START_TIMEOUT,
                |_| panic!("live launch process must keep the single-session slot"),
            )["code"],
            "fullscreen_session_limit"
        );

        sessions.stop_with_at(&ticket, 0, &launcher, now + START_TIMEOUT, |_, _| Ok(0));
        sessions.advance(now + START_TIMEOUT + Duration::from_secs(2), &launcher);
        assert_eq!(sessions.status(&ticket)["state"], "stopping");
        sessions.advance(now + START_TIMEOUT + Duration::from_secs(21), &launcher);
        assert_eq!(sessions.status(&ticket)["state"], "cleanup_failed");
        assert_eq!(sessions.active_count(), 1);

        sessions.stop_with(&ticket, 10, &[], |_, _| Ok(0));
        sessions.advance(now + START_TIMEOUT + Duration::from_secs(5), &[]);
        assert_eq!(sessions.status(&ticket)["state"], "stopped");
        assert_eq!(sessions.active_count(), 0);
    }

    #[test]
    fn stop_timeout_and_unexpected_ui_exit_preserve_cleanup_failure() {
        let now = Instant::now();
        let mut sessions = FullscreenSessions::default();
        let started = sessions.start_with(
            "operation-1",
            Some(installation()),
            &[],
            FocusPolicy::disabled(),
            now,
            |_| Ok(77),
        );
        let ticket = started["ticket"].as_str().unwrap().to_owned();
        let running = [fullscreen_process(80)];
        sessions.advance(now + Duration::from_secs(1), &running);
        sessions.stop_with(&ticket, 1, &running, |_, _| Ok(1));
        sessions.advance(now + Duration::from_secs(2), &running);
        assert_eq!(sessions.status(&ticket)["state"], "cleanup_failed");

        let mut unexpected_exit = FullscreenSessions::default();
        let started = unexpected_exit.start_with(
            "operation-2",
            Some(installation()),
            &[],
            FocusPolicy::disabled(),
            now,
            |_| Ok(81),
        );
        let ticket = started["ticket"].as_str().unwrap();
        unexpected_exit.advance(now + Duration::from_secs(1), &running);
        unexpected_exit.advance(now + Duration::from_secs(2), &[]);
        assert_eq!(unexpected_exit.status(ticket)["state"], "cleanup_failed");
    }

    #[test]
    fn recovery_never_stops_a_fullscreen_process_without_its_ticket() {
        let mut sessions = FullscreenSessions::default();
        let response = sessions.recover_stop("unknown-operation", 10, &[fullscreen_process(80)]);
        assert_eq!(response["code"], "launch_not_found");
    }
}
