//! Ticketed Playnite Fullscreen UI sessions bound to one trusted installation.

#[cfg(not(test))]
use crate::connector;
use crate::connector::{
    FullscreenStatus, FullscreenStatusKind, FullscreenWatchCommand, FullscreenWatchEvent,
};
use crate::discovery::ProcessImage;
use crate::focus::{FocusPolicy, FocusRetry};
use crate::installation::Installation;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::mpsc::{self, Receiver, Sender};
#[cfg(not(test))]
use std::thread;
use std::time::{Duration, Instant};

const START_TIMEOUT: Duration = Duration::from_secs(30);
const STARTUP_SETTLE_GRACE: Duration = Duration::from_secs(2);
const CLOSE_RETRY_INTERVAL: Duration = Duration::from_secs(1);
const SESSION_RETENTION: Duration = Duration::from_secs(300);
const MAX_SESSIONS: usize = 32;
const MAX_EXIT_TIMEOUT_SECONDS: u64 = 300;
const ZERO_TIMEOUT_CLOSE_GRACE: Duration = Duration::from_secs(20);
const MAX_TICKET_BYTES: usize = 48;
const GAME_EXIT_GRACE: Duration = Duration::from_secs(3);
const GAME_PROCESS_POLL_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum State {
    Starting,
    Running,
    Playing,
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
            Self::Playing => "playing",
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
    launched_process: ProcessImage,
    launch_baseline: Vec<ProcessImage>,
    fullscreen_process: Option<ProcessImage>,
    extension_process: Option<ProcessImage>,
    retired_extension_processes: Vec<ProcessImage>,
    active_game: Option<ActiveGame>,
    next_game_process_scan: Option<Instant>,
    ambiguous_game_ids: BTreeSet<String>,
    playnite_exiting: bool,
    return_allowed: bool,
    focus_policy: FocusPolicy,
    watch_command_sender: Sender<FullscreenWatchCommand>,
    #[cfg(test)]
    watch_event_sender: Sender<FullscreenWatchEvent>,
    watch_event_receiver: Receiver<FullscreenWatchEvent>,
    watch_processes: Vec<ProcessImage>,
    state: State,
    started_at: Instant,
    state_changed_at: Instant,
    stop_deadline: Option<Instant>,
    stop_requested_at: Option<Instant>,
    last_close_request_at: Option<Instant>,
    next_fullscreen_process_scan: Option<Instant>,
    focus_retry: FocusRetry,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ActiveGame {
    id: String,
    install_directory: String,
    executable: String,
    process_observed: bool,
    absent_since: Option<Instant>,
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
        #[cfg(test)]
        self.advance_with(
            now,
            processes,
            None,
            Installation::still_valid,
            Installation::launch_fullscreen,
        );
        #[cfg(not(test))]
        self.advance_with_scanner(
            now,
            processes,
            None,
            Installation::processes_with_fullscreen_image,
            Installation::still_valid,
            Installation::launch_fullscreen,
        );
    }

    #[cfg(test)]
    fn advance_with(
        &mut self,
        now: Instant,
        processes: &[ProcessImage],
        game_processes: Option<&[ProcessImage]>,
        validate_installation: impl Fn(&Installation) -> bool,
        launch: impl FnMut(&Installation) -> Result<ProcessImage, &'static str>,
    ) {
        self.advance_with_scanner(
            now,
            processes,
            game_processes,
            |installation| {
                Some(
                    installation
                        .fullscreen_processes(processes)
                        .into_iter()
                        .cloned()
                        .collect(),
                )
            },
            validate_installation,
            launch,
        );
    }

    fn advance_with_scanner(
        &mut self,
        now: Instant,
        processes: &[ProcessImage],
        game_processes: Option<&[ProcessImage]>,
        mut scan_fullscreen_processes: impl FnMut(&Installation) -> Option<Vec<ProcessImage>>,
        validate_installation: impl Fn(&Installation) -> bool,
        mut launch: impl FnMut(&Installation) -> Result<ProcessImage, &'static str>,
    ) {
        self.prune(now);
        for session in self.by_ticket.values_mut() {
            let trusted_processes = session
                .installation
                .processes_for_installation(processes)
                .into_iter()
                .cloned()
                .collect::<Vec<_>>();
            if trusted_processes != session.watch_processes {
                let _ = session
                    .watch_command_sender
                    .send(FullscreenWatchCommand::UpdateProcesses(
                        trusted_processes.clone(),
                    ));
                session.watch_processes = trusted_processes;
            }
            poll_watch_events(session, now, processes);
            match session.state {
                State::Starting => {
                    if let Some(fullscreen_process) = new_fullscreen_candidate(session, processes) {
                        track_fullscreen_process(session, fullscreen_process, now);
                    } else if now.saturating_duration_since(session.started_at) >= START_TIMEOUT {
                        session.transition(State::LaunchFailed, now);
                    }
                    session
                        .focus_retry
                        .tick_with(now, FocusRetry::attempt_windows_focus);
                }
                State::Running => {
                    session
                        .focus_retry
                        .tick_with(now, FocusRetry::attempt_windows_focus);
                    if session.playnite_exiting {
                        if playnite_exit_is_confirmed(session, now, &mut scan_fullscreen_processes)
                        {
                            session.transition(State::Stopped, now);
                        }
                    } else if session.active_game.is_some() {
                        session.transition(State::Playing, now);
                        advance_active_game(
                            session,
                            now,
                            game_processes,
                            &validate_installation,
                            &mut launch,
                            processes,
                        );
                    } else if !tracked_fullscreen_process_is_running(session, processes) {
                        // Without an authenticated game handoff, disappearance
                        // is ambiguous and must not launch a replacement UI.
                        session.transition(State::CleanupFailed, now);
                    }
                }
                State::Playing => {
                    if session.fullscreen_process.is_none()
                        && let Some(fullscreen_process) =
                            new_fullscreen_candidate(session, processes)
                    {
                        // A handoff may precede discovery. Track the ticket's
                        // post-baseline process for later stop, without focusing
                        // the UI over the active game.
                        session.fullscreen_process = Some(fullscreen_process);
                    }
                    if session.playnite_exiting {
                        if playnite_exit_is_confirmed(session, now, &mut scan_fullscreen_processes)
                        {
                            session.transition(State::Stopped, now);
                        }
                    } else {
                        advance_active_game(
                            session,
                            now,
                            game_processes,
                            &validate_installation,
                            &mut launch,
                            processes,
                        );
                    }
                }
                State::LaunchFailed => {
                    if let Some(fullscreen_process) = new_fullscreen_candidate(session, processes) {
                        track_fullscreen_process(session, fullscreen_process, now);
                    }
                }
                State::CleanupFailed => {
                    if session.ambiguous_game_ids.is_empty()
                        && session.stop_requested_at.is_none()
                        && session.active_game.is_none()
                        && !session.playnite_exiting
                        && tracked_fullscreen_process_is_running(session, processes)
                    {
                        session.transition(State::Running, now);
                    }
                }
                State::Stopping => {
                    session.focus_retry.cancel();
                    let should_scan_processes = session
                        .next_fullscreen_process_scan
                        .is_none_or(|deadline| now >= deadline);
                    let exact_process_scan = if should_scan_processes {
                        session.next_fullscreen_process_scan =
                            now.checked_add(CLOSE_RETRY_INTERVAL);
                        Some(scan_fullscreen_processes(&session.installation))
                    } else {
                        None
                    };
                    if session.fullscreen_process.is_none()
                        && let Some(fullscreen_process) =
                            new_fullscreen_candidate(session, processes).or_else(|| {
                                exact_process_scan
                                    .as_ref()
                                    .and_then(Option::as_deref)
                                    .and_then(|scan| new_fullscreen_candidate(session, scan))
                            })
                    {
                        session.fullscreen_process = Some(fullscreen_process);
                    }
                    let tracked_processes = session
                        .fullscreen_process
                        .as_ref()
                        .filter(|expected| match &exact_process_scan {
                            Some(Some(scan)) => {
                                scan.iter().any(|process| process.same_identity(expected))
                            }
                            Some(None) | None => processes
                                .iter()
                                .any(|process| process.same_identity(expected)),
                        })
                        .cloned()
                        .into_iter()
                        .collect::<Vec<_>>();
                    let absence_confirmed = exact_process_scan
                        .as_ref()
                        .and_then(Option::as_ref)
                        .is_some_and(|scan| {
                            session.fullscreen_process.as_ref().map_or_else(
                                || scan.is_empty(),
                                |expected| {
                                    !scan.iter().any(|process| process.same_identity(expected))
                                },
                            )
                        });
                    if tracked_processes.is_empty() {
                        let launch_process_alive = session
                            .installation
                            .launch_process_is_running(&session.launched_process, processes);
                        let settle_elapsed = session.stop_requested_at.is_some_and(|requested| {
                            now.saturating_duration_since(requested) >= STARTUP_SETTLE_GRACE
                        });
                        let deadline_elapsed = session
                            .stop_deadline
                            .is_some_and(|deadline| now >= deadline);
                        if absence_confirmed
                            && !launch_process_alive
                            && (settle_elapsed || deadline_elapsed)
                        {
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
                                .request_fullscreen_close(&tracked_processes)
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
        launch: impl FnOnce(&Installation) -> Result<ProcessImage, &'static str>,
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
        let launched_process = match launch(&installation) {
            Ok(process) if installation.valid_launch_process_identity(&process) => process,
            Ok(_) => return crate::launch::not_started_error("fullscreen_launch_failed"),
            Err(code) => return crate::launch::not_started_error(code),
        };
        let launch_baseline = installation
            .fullscreen_processes(processes)
            .into_iter()
            .cloned()
            .collect::<Vec<_>>();
        let watch_processes = installation
            .processes_for_installation(processes)
            .into_iter()
            .cloned()
            .collect::<Vec<_>>();
        let (watch_command_sender, watch_commands) = mpsc::channel();
        let (watch_event_sender, watch_event_receiver) = mpsc::channel();
        #[cfg(test)]
        let session_watch_event_sender = watch_event_sender.clone();
        let initial_watch_processes = watch_processes.clone();
        #[cfg(not(test))]
        thread::spawn(move || {
            connector::watch_fullscreen(
                &initial_watch_processes,
                watch_commands,
                watch_event_sender,
            );
        });
        #[cfg(test)]
        drop((watch_commands, initial_watch_processes, watch_event_sender));
        let session = Session {
            operation_id: operation_id.into(),
            ticket: ticket.clone(),
            installation,
            launched_process,
            launch_baseline,
            fullscreen_process: None,
            extension_process: None,
            retired_extension_processes: Vec::new(),
            active_game: None,
            next_game_process_scan: None,
            ambiguous_game_ids: BTreeSet::new(),
            playnite_exiting: false,
            return_allowed: true,
            focus_policy,
            watch_command_sender,
            #[cfg(test)]
            watch_event_sender: session_watch_event_sender,
            watch_event_receiver,
            watch_processes,
            state: State::Starting,
            started_at: now,
            state_changed_at: now,
            stop_deadline: None,
            stop_requested_at: None,
            last_close_request_at: None,
            next_fullscreen_process_scan: None,
            focus_retry: FocusRetry::default(),
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
            if session.fullscreen_process.is_none()
                && let Some(fullscreen_process) = new_fullscreen_candidate(session, processes)
            {
                session.fullscreen_process = Some(fullscreen_process);
            }
            session.transition(State::Stopping, now);
            session.return_allowed = false;
            session.active_game = None;
            session.next_game_process_scan = None;
            let _ = session
                .watch_command_sender
                .send(FullscreenWatchCommand::Cancel);
            session.stop_requested_at = Some(now);
            session.next_fullscreen_process_scan = None;
            let timeout = if exit_timeout_seconds == 0 {
                ZERO_TIMEOUT_CLOSE_GRACE
            } else {
                Duration::from_secs(exit_timeout_seconds)
            };
            session.stop_deadline = now.checked_add(timeout);
        }
        if session.state == State::Stopping {
            let tracked_processes = session
                .fullscreen_process
                .as_ref()
                .filter(|expected| processes.iter().any(|process| process == *expected))
                .cloned()
                .into_iter()
                .collect::<Vec<_>>();
            if request_close(&session.installation, &tracked_processes).is_err() {
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

fn poll_watch_events(session: &mut Session, now: Instant, processes: &[ProcessImage]) {
    while let Ok(event) = session.watch_event_receiver.try_recv() {
        match event {
            FullscreenWatchEvent::Disconnected { extension_process } => {
                if session.extension_process.as_ref() == Some(&extension_process)
                    && !processes
                        .iter()
                        .any(|process| process.same_identity(&extension_process))
                {
                    session.retired_extension_processes.push(extension_process);
                    session.extension_process = None;
                }
            }
            FullscreenWatchEvent::Status {
                status,
                extension_process,
            } => {
                if !session
                    .installation
                    .matches_running_processes(std::slice::from_ref(&extension_process))
                    || session
                        .retired_extension_processes
                        .contains(&extension_process)
                {
                    continue;
                }
                if session.extension_process.as_ref() != Some(&extension_process)
                    && let Some(previous) =
                        session.extension_process.replace(extension_process.clone())
                {
                    session.retired_extension_processes.push(previous);
                }
                apply_fullscreen_status(session, status, now, processes);
            }
        }
    }
}

fn apply_fullscreen_status(
    session: &mut Session,
    status: FullscreenStatus,
    now: Instant,
    _processes: &[ProcessImage],
) {
    if !session.return_allowed
        || session.state == State::Stopping
        || session.state == State::Stopped
    {
        return;
    }
    match status.kind {
        FullscreenStatusKind::GameStarted => {
            let (Some(game_id), Some(install_directory), Some(executable)) = (
                status.game_id,
                status.install_directory.as_deref(),
                status.executable.as_deref(),
            ) else {
                return;
            };
            let Some((install_directory, executable)) =
                validated_game_paths(install_directory, executable)
            else {
                return;
            };
            if let Some(active_game) = &session.active_game {
                if active_game.id == game_id
                    && active_game.install_directory == install_directory
                    && active_game.executable == executable
                {
                    return;
                }
                session.ambiguous_game_ids.insert(active_game.id.clone());
                session.ambiguous_game_ids.insert(game_id);
                session.transition(State::CleanupFailed, now);
                return;
            }
            if !session.ambiguous_game_ids.is_empty() {
                session.ambiguous_game_ids.insert(game_id);
                return;
            }
            session.active_game = Some(ActiveGame {
                id: game_id,
                install_directory,
                executable,
                process_observed: false,
                absent_since: None,
            });
            session.next_game_process_scan = None;
            session.focus_retry.cancel();
            session.transition(State::Playing, now);
        }
        FullscreenStatusKind::GameStopped => {
            if session.active_game.as_ref().is_some_and(|active_game| {
                Some(active_game.id.as_str()) == status.game_id.as_deref()
            }) && session
                .next_game_process_scan
                .is_none_or(|deadline| now >= deadline)
            {
                // The extension's stop status is only a prompt to verify now;
                // the one-second scan limit still applies, and it cannot prove
                // this executable belongs to the reported GUID.
                session.next_game_process_scan = None;
            }
        }
        FullscreenStatusKind::PlayniteExiting => {
            session.playnite_exiting = true;
            session.return_allowed = false;
            session.active_game = None;
            session.next_game_process_scan = None;
            session.focus_retry.cancel();
            let _ = session
                .watch_command_sender
                .send(FullscreenWatchCommand::Cancel);
        }
    }
}

fn validated_game_paths(install_directory: &str, executable: &str) -> Option<(String, String)> {
    let target =
        crate::focus::FocusTarget::from_status_paths(Some(install_directory), Some(executable));
    let install_directory = target.install_directory?;
    let executable = target.executable?;
    let normalized_directory = normalize_status_path(&install_directory);
    let normalized_executable = normalize_status_path(&executable);
    if normalized_executable
        .strip_prefix(&normalized_directory)
        .is_none_or(|suffix| !suffix.starts_with('\\') || suffix.len() <= 1)
        || install_directory
            .split(['\\', '/'])
            .any(|component| component == "." || component == "..")
        || executable
            .split(['\\', '/'])
            .any(|component| component == "." || component == "..")
    {
        return None;
    }
    Some((install_directory, executable))
}

fn normalize_status_path(path: &str) -> String {
    let normalized = path.replace('/', "\\");
    let normalized = normalized.strip_prefix(r"\\?\").unwrap_or(&normalized);
    normalized.trim_end_matches('\\').to_ascii_lowercase()
}

fn new_fullscreen_candidate(session: &Session, processes: &[ProcessImage]) -> Option<ProcessImage> {
    let mut candidates = session
        .installation
        .fullscreen_processes(processes)
        .into_iter()
        .filter(|process| {
            !session
                .launch_baseline
                .iter()
                .any(|previous| previous.same_identity(process))
                && process.creation_time >= session.launched_process.creation_time
        })
        .cloned();
    let candidate = candidates.next()?;
    candidates.next().is_none().then_some(candidate)
}

fn track_fullscreen_process(session: &mut Session, process: ProcessImage, now: Instant) {
    session.focus_retry.start(
        session.focus_policy,
        crate::focus::FocusTarget::for_process(
            &process.path,
            process.process_id,
            process.creation_time,
        ),
        now,
    );
    session.fullscreen_process = Some(process);
    session.transition(State::Running, now);
}

fn tracked_fullscreen_process_is_running(session: &Session, processes: &[ProcessImage]) -> bool {
    session.fullscreen_process.as_ref().is_some_and(|expected| {
        processes
            .iter()
            .any(|process| process.same_identity(expected))
    })
}

fn playnite_exit_is_confirmed(
    session: &mut Session,
    now: Instant,
    scan_fullscreen_processes: &mut impl FnMut(&Installation) -> Option<Vec<ProcessImage>>,
) -> bool {
    if session
        .next_fullscreen_process_scan
        .is_some_and(|deadline| now < deadline)
    {
        return false;
    }
    session.next_fullscreen_process_scan = now.checked_add(CLOSE_RETRY_INTERVAL);
    scan_fullscreen_processes(&session.installation).is_some_and(|processes| processes.is_empty())
}

fn advance_active_game(
    session: &mut Session,
    now: Instant,
    supplied_game_processes: Option<&[ProcessImage]>,
    validate_installation: &impl Fn(&Installation) -> bool,
    launch: &mut impl FnMut(&Installation) -> Result<ProcessImage, &'static str>,
    processes: &[ProcessImage],
) {
    let Some(active_game) = session.active_game.as_mut() else {
        session.transition(State::CleanupFailed, now);
        return;
    };
    if !session.ambiguous_game_ids.is_empty() {
        session.transition(State::CleanupFailed, now);
        return;
    }
    if session
        .next_game_process_scan
        .is_some_and(|deadline| now < deadline)
    {
        return;
    }
    session.next_game_process_scan = now.checked_add(GAME_PROCESS_POLL_INTERVAL);
    let game_processes = match supplied_game_processes {
        Some(processes) => Some(processes.to_vec()),
        None => session
            .installation
            .processes_with_exact_image(&active_game.executable),
    };
    let Some(game_processes) = game_processes else {
        // Access denial or a process-list failure is not evidence that the game
        // exited. Retain the ticket and wait for another bounded poll.
        active_game.absent_since = None;
        return;
    };
    if !game_processes.is_empty() {
        active_game.process_observed = true;
        active_game.absent_since = None;
        return;
    }
    if !active_game.process_observed {
        return;
    }
    let absent_since = *active_game.absent_since.get_or_insert(now);
    if now.saturating_duration_since(absent_since) < GAME_EXIT_GRACE {
        return;
    }
    return_to_fullscreen(session, now, validate_installation, launch, processes);
}

fn return_to_fullscreen(
    session: &mut Session,
    now: Instant,
    validate_installation: &impl Fn(&Installation) -> bool,
    launch: &mut impl FnMut(&Installation) -> Result<ProcessImage, &'static str>,
    processes: &[ProcessImage],
) {
    if !session.return_allowed
        || session.stop_requested_at.is_some()
        || !session.ambiguous_game_ids.is_empty()
    {
        return;
    }
    if let Some(expected) = &session.fullscreen_process {
        if processes
            .iter()
            .any(|process| process.same_identity(expected))
        {
            session.focus_retry.start(
                session.focus_policy,
                crate::focus::FocusTarget::for_process(
                    &expected.path,
                    expected.process_id,
                    expected.creation_time,
                ),
                now,
            );
            session.active_game = None;
            session.next_game_process_scan = None;
            session.transition(State::Running, now);
            return;
        }
        if processes
            .iter()
            .any(|process| process.process_id == expected.process_id)
            || !session
                .installation
                .fullscreen_processes(processes)
                .is_empty()
        {
            session.transition(State::CleanupFailed, now);
            return;
        }
    } else if let Some(candidate) = new_fullscreen_candidate(session, processes) {
        // Status may beat process discovery. Adopt only one exact-install
        // process created after this ticket's launch baseline.
        track_fullscreen_process(session, candidate, now);
        session.active_game = None;
        return;
    } else if !session
        .installation
        .fullscreen_processes(processes)
        .is_empty()
    {
        // A baseline or ambiguous process cannot be claimed by this ticket.
        session.transition(State::CleanupFailed, now);
        return;
    }
    if !validate_installation(&session.installation) {
        session.transition(State::CleanupFailed, now);
        return;
    }
    let baseline = session
        .installation
        .fullscreen_processes(processes)
        .into_iter()
        .cloned()
        .collect::<Vec<_>>();
    match launch(&session.installation) {
        Ok(launched_process)
            if session
                .installation
                .valid_launch_process_identity(&launched_process) =>
        {
            session.launched_process = launched_process;
            session.launch_baseline = baseline;
            session.fullscreen_process = None;
            session.active_game = None;
            session.next_game_process_scan = None;
            session.started_at = now;
            session.focus_retry.cancel();
            session.transition(State::Starting, now);
        }
        _ => session.transition(State::CleanupFailed, now),
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
    use super::*;
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
            creation_time: u64::from(process_id),
        }
    }

    fn launch_process(process_id: u32) -> ProcessImage {
        ProcessImage {
            process_id,
            path: r"C:\Playnite\Playnite.DesktopApp.exe".into(),
            creation_time: u64::from(process_id),
        }
    }

    fn extension_process(process_id: u32, creation_time: u64) -> ProcessImage {
        ProcessImage {
            process_id,
            path: r"C:\Playnite\Playnite.DesktopApp.exe".into(),
            creation_time,
        }
    }

    fn game_process(process_id: u32) -> ProcessImage {
        ProcessImage {
            process_id,
            path: r"C:\Games\Sample\game.exe".into(),
            creation_time: u64::from(process_id),
        }
    }

    fn begin_running_session(sessions: &mut FullscreenSessions, now: Instant) -> String {
        let started = sessions.start_with(
            "fullscreen-operation",
            Some(installation()),
            &[],
            FocusPolicy::disabled(),
            now,
            |_| Ok(launch_process(77)),
        );
        let ticket = started["ticket"].as_str().unwrap().to_owned();
        let fullscreen = [fullscreen_process(80)];
        sessions.advance_with(
            now + Duration::from_secs(1),
            &fullscreen,
            Some(&[]),
            |_| true,
            |_| panic!("startup must not relaunch"),
        );
        assert_eq!(sessions.status(&ticket)["state"], "running");
        ticket
    }

    fn queue_status(
        sessions: &FullscreenSessions,
        ticket: &str,
        status: FullscreenStatus,
        extension: ProcessImage,
    ) {
        sessions
            .by_ticket
            .get(ticket)
            .unwrap()
            .watch_event_sender
            .send(FullscreenWatchEvent::Status {
                status,
                extension_process: extension,
            })
            .unwrap();
    }

    fn status(kind: FullscreenStatusKind, game_id: Option<&str>) -> FullscreenStatus {
        FullscreenStatus {
            kind,
            game_id: game_id.map(str::to_owned),
            install_directory: Some(r"C:\Games\Sample".into()),
            executable: Some(r"C:\Games\Sample\game.exe".into()),
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
            |_| Ok(launch_process(77)),
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
                    creation_time: 99,
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
            |_| Ok(launch_process(77)),
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
            |_| Ok(launch_process(77)),
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
            |_| Ok(launch_process(77)),
        );
        let ticket = started["ticket"].as_str().unwrap().to_owned();
        let launcher = [ProcessImage {
            process_id: 77,
            path: r"C:\Playnite\Playnite.DesktopApp.exe".into(),
            creation_time: 77,
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
            |_| Ok(launch_process(77)),
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
            |_| Ok(launch_process(70)),
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

    #[test]
    fn idle_fullscreen_ticket_does_not_launch_a_second_ui() {
        let now = Instant::now();
        let mut sessions = FullscreenSessions::default();
        let ticket = begin_running_session(&mut sessions, now);
        let fullscreen = [fullscreen_process(80)];
        sessions.advance_with(
            now + Duration::from_secs(2),
            &fullscreen,
            Some(&[]),
            |_| true,
            |_| panic!("idle sessions do not relaunch"),
        );
        assert_eq!(sessions.status(&ticket)["state"], "running");
    }

    #[test]
    fn status_before_fullscreen_discovery_adopts_only_the_unique_new_process() {
        let now = Instant::now();
        let mut sessions = FullscreenSessions::default();
        let ticket = sessions.start_with(
            "status-before-discovery",
            Some(installation()),
            &[],
            FocusPolicy::disabled(),
            now,
            |_| Ok(launch_process(77)),
        )["ticket"]
            .as_str()
            .unwrap()
            .to_owned();
        let extension = extension_process(31, 310);
        queue_status(
            &sessions,
            &ticket,
            status(
                FullscreenStatusKind::GameStarted,
                Some("a1b2c3d4-1234-5678-9abc-0123456789ab"),
            ),
            extension.clone(),
        );
        let extension_only = [extension.clone()];
        sessions.advance_with(
            now + Duration::from_secs(1),
            &extension_only,
            Some(&[game_process(44)]),
            |_| true,
            |_| panic!("game start must not launch another fullscreen process"),
        );
        queue_status(
            &sessions,
            &ticket,
            status(
                FullscreenStatusKind::GameStopped,
                Some("a1b2c3d4-1234-5678-9abc-0123456789ab"),
            ),
            extension.clone(),
        );
        sessions.advance_with(
            now + Duration::from_secs(2),
            &extension_only,
            Some(&[]),
            |_| true,
            |_| panic!("wait for the exit grace"),
        );
        sessions.advance_with(
            now + Duration::from_secs(5),
            &[fullscreen_process(95), extension],
            Some(&[]),
            |_| true,
            |_| panic!("adopt the one new exact-install process"),
        );
        assert_eq!(sessions.status(&ticket)["state"], "running");
        assert_eq!(
            sessions.by_ticket[&ticket].fullscreen_process,
            Some(fullscreen_process(95))
        );
    }

    #[test]
    fn status_before_discovery_still_stops_the_ticket_owned_fullscreen_process() {
        let now = Instant::now();
        let mut sessions = FullscreenSessions::default();
        let ticket = sessions.start_with(
            "status-before-discovery-stop",
            Some(installation()),
            &[],
            FocusPolicy::disabled(),
            now,
            |_| Ok(launch_process(77)),
        )["ticket"]
            .as_str()
            .unwrap()
            .to_owned();
        let extension = extension_process(31, 310);
        queue_status(
            &sessions,
            &ticket,
            status(
                FullscreenStatusKind::GameStarted,
                Some("a1b2c3d4-1234-5678-9abc-0123456789ab"),
            ),
            extension.clone(),
        );
        sessions.advance_with(
            now + Duration::from_secs(1),
            std::slice::from_ref(&extension),
            Some(&[game_process(44)]),
            |_| true,
            |_| panic!("game start must not launch another fullscreen process"),
        );
        assert_eq!(sessions.status(&ticket)["state"], "playing");
        assert!(sessions.by_ticket[&ticket].fullscreen_process.is_none());

        let discovered_fullscreen = fullscreen_process(95);
        let discovered_processes = [discovered_fullscreen.clone(), extension.clone()];
        assert_eq!(
            sessions.stop_with_at(
                &ticket,
                10,
                &discovered_processes,
                now + Duration::from_secs(2),
                |_, owned| {
                    assert_eq!(owned, std::slice::from_ref(&discovered_fullscreen));
                    Ok(1)
                }
            )["state"],
            "stopping"
        );
        assert_eq!(
            sessions.by_ticket[&ticket].fullscreen_process,
            Some(discovered_fullscreen)
        );
        sessions.advance_with(
            now + Duration::from_secs(4),
            &[extension],
            Some(&[]),
            |_| true,
            |_| panic!("stopped tickets cannot relaunch"),
        );
        assert_eq!(sessions.status(&ticket)["state"], "stopped");
    }

    #[test]
    fn stop_retains_ticket_when_fullscreen_candidates_are_ambiguous() {
        let now = Instant::now();
        let mut sessions = FullscreenSessions::default();
        let ticket = sessions.start_with(
            "ambiguous-fullscreen-stop",
            Some(installation()),
            &[],
            FocusPolicy::disabled(),
            now,
            |_| Ok(launch_process(77)),
        )["ticket"]
            .as_str()
            .unwrap()
            .to_owned();
        let ambiguous_fullscreen = [fullscreen_process(90), fullscreen_process(91)];
        sessions.stop_with_at(
            &ticket,
            1,
            &ambiguous_fullscreen,
            now + Duration::from_secs(1),
            |_, owned| {
                assert!(owned.is_empty());
                Ok(0)
            },
        );
        sessions.advance_with(
            now + Duration::from_secs(3),
            &ambiguous_fullscreen,
            Some(&[]),
            |_| true,
            |_| panic!("stop never relaunches"),
        );
        assert_eq!(sessions.status(&ticket)["state"], "cleanup_failed");
        assert_eq!(sessions.active_count(), 1);
        assert!(sessions.by_ticket[&ticket].fullscreen_process.is_none());
    }

    #[test]
    fn stop_does_not_release_ticket_when_fullscreen_process_scan_is_unknown() {
        let now = Instant::now();
        let mut sessions = FullscreenSessions::default();
        let ticket = sessions.start_with(
            "unknown-fullscreen-stop",
            Some(installation()),
            &[],
            FocusPolicy::disabled(),
            now,
            |_| Ok(launch_process(77)),
        )["ticket"]
            .as_str()
            .unwrap()
            .to_owned();
        sessions.stop_with_at(&ticket, 1, &[], now + Duration::from_secs(1), |_, owned| {
            assert!(owned.is_empty());
            Ok(0)
        });
        sessions.advance_with_scanner(
            now + Duration::from_secs(3),
            &[],
            Some(&[]),
            |_| None,
            |_| true,
            |_| panic!("unknown process scans never relaunch"),
        );
        assert_eq!(sessions.status(&ticket)["state"], "cleanup_failed");
        assert_eq!(sessions.active_count(), 1);
    }

    #[test]
    fn matching_game_start_and_exit_restore_the_ticket_owned_fullscreen_process() {
        let now = Instant::now();
        let mut sessions = FullscreenSessions::default();
        let ticket = begin_running_session(&mut sessions, now);
        let fullscreen = fullscreen_process(80);
        let extension = extension_process(31, 310);
        let running = [fullscreen.clone(), extension.clone()];
        queue_status(
            &sessions,
            &ticket,
            status(
                FullscreenStatusKind::GameStarted,
                Some("a1b2c3d4-1234-5678-9abc-0123456789ab"),
            ),
            extension.clone(),
        );
        sessions.advance_with(
            now + Duration::from_secs(2),
            &running,
            Some(&[game_process(44)]),
            |_| true,
            |_| panic!("game start does not launch another fullscreen process"),
        );
        assert_eq!(sessions.status(&ticket)["state"], "playing");

        queue_status(
            &sessions,
            &ticket,
            status(
                FullscreenStatusKind::GameStopped,
                Some("a1b2c3d4-1234-5678-9abc-0123456789ab"),
            ),
            extension,
        );
        for second in 3..=6 {
            sessions.advance_with(
                now + Duration::from_secs(second),
                &running,
                Some(&[]),
                |_| true,
                |_| panic!("the verified live fullscreen process should be focused"),
            );
        }
        assert_eq!(sessions.status(&ticket)["state"], "running");
        assert!(sessions.by_ticket[&ticket].active_game.is_none());
        assert_eq!(
            sessions.by_ticket[&ticket].fullscreen_process,
            Some(fullscreen)
        );
    }

    #[test]
    fn missing_game_stopped_uses_observed_exact_image_and_throttled_exit_scans() {
        let now = Instant::now();
        let mut sessions = FullscreenSessions::default();
        let ticket = begin_running_session(&mut sessions, now);
        let extension = extension_process(31, 310);
        queue_status(
            &sessions,
            &ticket,
            status(
                FullscreenStatusKind::GameStarted,
                Some("a1b2c3d4-1234-5678-9abc-0123456789ab"),
            ),
            extension.clone(),
        );
        let running = [fullscreen_process(80), extension];
        sessions.advance_with(
            now + Duration::from_secs(2),
            &running,
            Some(&[game_process(44)]),
            |_| true,
            |_| panic!("game start does not relaunch"),
        );
        let next_scan = sessions.by_ticket[&ticket].next_game_process_scan.unwrap();
        sessions.advance_with(
            now + Duration::from_millis(2_500),
            &running,
            Some(&[]),
            |_| true,
            |_| panic!("subsecond ticks must not run another process scan"),
        );
        assert_eq!(
            sessions.by_ticket[&ticket].next_game_process_scan,
            Some(next_scan)
        );
        for second in 3..=6 {
            sessions.advance_with(
                now + Duration::from_secs(second),
                &running,
                Some(&[]),
                |_| true,
                |_| panic!("the tracked fullscreen process should be restored"),
            );
        }
        assert_eq!(sessions.status(&ticket)["state"], "running");
        assert!(sessions.by_ticket[&ticket].active_game.is_none());
    }

    #[test]
    fn transient_game_stopped_before_first_process_observation_never_returns_early() {
        let now = Instant::now();
        let mut sessions = FullscreenSessions::default();
        let ticket = begin_running_session(&mut sessions, now);
        let extension = extension_process(31, 310);
        let game_id = "a1b2c3d4-1234-5678-9abc-0123456789ab";
        queue_status(
            &sessions,
            &ticket,
            status(FullscreenStatusKind::GameStarted, Some(game_id)),
            extension.clone(),
        );
        queue_status(
            &sessions,
            &ticket,
            status(FullscreenStatusKind::GameStopped, Some(game_id)),
            extension.clone(),
        );
        let running = [fullscreen_process(80), extension];
        sessions.advance_with(
            now + Duration::from_secs(2),
            &running,
            Some(&[]),
            |_| true,
            |_| panic!("a stop record cannot prove the game process exited"),
        );
        for second in 3..=8 {
            sessions.advance_with(
                now + Duration::from_secs(second),
                &running,
                Some(&[]),
                |_| true,
                |_| panic!("never-observed process identity remains unverified"),
            );
        }
        assert_eq!(sessions.status(&ticket)["state"], "playing");
        let active_game = sessions.by_ticket[&ticket].active_game.as_ref().unwrap();
        assert!(!active_game.process_observed);
        assert!(active_game.absent_since.is_none());
    }

    #[test]
    fn duplicate_game_events_are_idempotent_and_multiple_initial_games_fail_closed() {
        let now = Instant::now();
        let mut sessions = FullscreenSessions::default();
        let ticket = begin_running_session(&mut sessions, now);
        let extension = extension_process(31, 310);
        let started = status(
            FullscreenStatusKind::GameStarted,
            Some("a1b2c3d4-1234-5678-9abc-0123456789ab"),
        );
        queue_status(&sessions, &ticket, started.clone(), extension.clone());
        queue_status(&sessions, &ticket, started.clone(), extension.clone());
        sessions.advance_with(
            now + Duration::from_secs(2),
            &[fullscreen_process(80), extension.clone()],
            Some(&[game_process(44)]),
            |_| true,
            |_| panic!("duplicate status must not relaunch"),
        );
        assert_eq!(sessions.status(&ticket)["state"], "playing");
        assert!(sessions.by_ticket[&ticket].ambiguous_game_ids.is_empty());

        let second_game = status(
            FullscreenStatusKind::GameStarted,
            Some("b1b2c3d4-1234-5678-9abc-0123456789ab"),
        );
        queue_status(&sessions, &ticket, second_game, extension.clone());
        sessions.advance_with(
            now + Duration::from_secs(3),
            &[fullscreen_process(80), extension],
            Some(&[game_process(44)]),
            |_| true,
            |_| panic!("ambiguous active-game sync must not relaunch"),
        );
        assert_eq!(sessions.status(&ticket)["state"], "cleanup_failed");
        assert_eq!(sessions.by_ticket[&ticket].ambiguous_game_ids.len(), 2);
    }

    #[test]
    fn explicit_stop_cancels_automatic_return_and_ignores_queued_statuses() {
        let now = Instant::now();
        let mut sessions = FullscreenSessions::default();
        let ticket = begin_running_session(&mut sessions, now);
        let extension = extension_process(31, 310);
        queue_status(
            &sessions,
            &ticket,
            status(
                FullscreenStatusKind::GameStarted,
                Some("a1b2c3d4-1234-5678-9abc-0123456789ab"),
            ),
            extension.clone(),
        );
        let fullscreen = [fullscreen_process(80), extension.clone()];
        sessions.advance_with(
            now + Duration::from_secs(2),
            &fullscreen,
            Some(&[game_process(44)]),
            |_| true,
            |_| panic!("game start must not relaunch"),
        );
        assert_eq!(
            sessions.stop_with_at(
                &ticket,
                10,
                &fullscreen,
                now + Duration::from_secs(2),
                |_, owned| {
                    assert_eq!(owned, &[fullscreen_process(80)]);
                    Ok(1)
                }
            )["state"],
            "stopping"
        );
        queue_status(
            &sessions,
            &ticket,
            status(
                FullscreenStatusKind::GameStopped,
                Some("a1b2c3d4-1234-5678-9abc-0123456789ab"),
            ),
            extension,
        );
        sessions.advance_with(
            now + Duration::from_secs(5),
            &[],
            Some(&[]),
            |_| true,
            |_| panic!("cancelled tickets never relaunch"),
        );
        assert_eq!(sessions.status(&ticket)["state"], "stopped");
    }

    #[test]
    fn playnite_restart_reauthenticates_new_identity_and_rejects_old_generation_events() {
        let now = Instant::now();
        let mut sessions = FullscreenSessions::default();
        let ticket = begin_running_session(&mut sessions, now);
        let old_extension = extension_process(31, 310);
        let new_extension = extension_process(32, 320);
        let game_id = "a1b2c3d4-1234-5678-9abc-0123456789ab";
        let running_game = [game_process(44)];
        queue_status(
            &sessions,
            &ticket,
            status(FullscreenStatusKind::GameStarted, Some(game_id)),
            old_extension.clone(),
        );
        sessions.advance_with(
            now + Duration::from_secs(2),
            &[fullscreen_process(80), old_extension.clone()],
            Some(&running_game),
            |_| true,
            |_| panic!("active game does not relaunch"),
        );
        sessions
            .by_ticket
            .get(&ticket)
            .unwrap()
            .watch_event_sender
            .send(FullscreenWatchEvent::Disconnected {
                extension_process: old_extension.clone(),
            })
            .unwrap();
        sessions.advance_with(
            now + Duration::from_secs(3),
            &[fullscreen_process(80), new_extension.clone()],
            Some(&running_game),
            |_| true,
            |_| panic!("reauthentication does not relaunch"),
        );
        queue_status(
            &sessions,
            &ticket,
            status(FullscreenStatusKind::GameStarted, Some(game_id)),
            new_extension.clone(),
        );
        sessions.advance_with(
            now + Duration::from_secs(4),
            &[fullscreen_process(80), new_extension.clone()],
            Some(&running_game),
            |_| true,
            |_| panic!("duplicate sync after restart does not relaunch"),
        );
        queue_status(
            &sessions,
            &ticket,
            status(FullscreenStatusKind::GameStopped, Some(game_id)),
            old_extension.clone(),
        );
        sessions.advance_with(
            now + Duration::from_secs(5),
            &[fullscreen_process(80), new_extension.clone()],
            Some(&running_game),
            |_| true,
            |_| panic!("old process generation cannot stop the active game"),
        );
        assert_eq!(sessions.status(&ticket)["state"], "playing");
    }

    #[test]
    fn transient_disconnect_keeps_the_same_live_extension_identity_authorized() {
        let now = Instant::now();
        let mut sessions = FullscreenSessions::default();
        let ticket = begin_running_session(&mut sessions, now);
        let extension = extension_process(31, 310);
        let game_id = "a1b2c3d4-1234-5678-9abc-0123456789ab";
        sessions
            .by_ticket
            .get(&ticket)
            .unwrap()
            .watch_event_sender
            .send(FullscreenWatchEvent::Disconnected {
                extension_process: extension.clone(),
            })
            .unwrap();
        sessions.advance_with(
            now + Duration::from_secs(2),
            &[fullscreen_process(80), extension.clone()],
            Some(&[]),
            |_| true,
            |_| panic!("transient pipe recovery does not relaunch"),
        );
        queue_status(
            &sessions,
            &ticket,
            status(FullscreenStatusKind::GameStarted, Some(game_id)),
            extension.clone(),
        );
        sessions.advance_with(
            now + Duration::from_secs(3),
            &[fullscreen_process(80), extension.clone()],
            Some(&[game_process(44)]),
            |_| true,
            |_| panic!("the same authenticated extension can reconnect"),
        );
        assert_eq!(sessions.status(&ticket)["state"], "playing");
        assert!(
            sessions.by_ticket[&ticket]
                .retired_extension_processes
                .is_empty()
        );
    }

    #[test]
    fn queued_shutdown_handoff_precedes_disconnect_and_later_old_events_are_discarded() {
        let now = Instant::now();
        let mut sessions = FullscreenSessions::default();
        let ticket = begin_running_session(&mut sessions, now);
        let old_extension = extension_process(31, 310);
        let new_extension = extension_process(32, 320);
        let game_id = "a1b2c3d4-1234-5678-9abc-0123456789ab";
        queue_status(
            &sessions,
            &ticket,
            status(FullscreenStatusKind::GameStarted, Some(game_id)),
            old_extension.clone(),
        );
        sessions
            .by_ticket
            .get(&ticket)
            .unwrap()
            .watch_event_sender
            .send(FullscreenWatchEvent::Disconnected {
                extension_process: old_extension.clone(),
            })
            .unwrap();
        queue_status(
            &sessions,
            &ticket,
            status(FullscreenStatusKind::GameStopped, Some(game_id)),
            old_extension.clone(),
        );
        queue_status(
            &sessions,
            &ticket,
            status(FullscreenStatusKind::GameStarted, Some(game_id)),
            new_extension.clone(),
        );
        sessions.advance_with(
            now + Duration::from_secs(2),
            &[fullscreen_process(80), new_extension.clone()],
            Some(&[game_process(44)]),
            |_| true,
            |_| panic!("a shutdown handoff does not relaunch"),
        );
        assert_eq!(sessions.status(&ticket)["state"], "playing");
        assert!(
            sessions.by_ticket[&ticket]
                .retired_extension_processes
                .contains(&old_extension)
        );
        assert_eq!(
            sessions.by_ticket[&ticket].extension_process,
            Some(new_extension)
        );
    }

    #[test]
    fn playnite_exiting_prevents_automatic_return_and_marks_closed_ticket_stopped() {
        let now = Instant::now();
        let mut sessions = FullscreenSessions::default();
        let ticket = begin_running_session(&mut sessions, now);
        let extension = extension_process(31, 310);
        queue_status(
            &sessions,
            &ticket,
            status(FullscreenStatusKind::PlayniteExiting, None),
            extension.clone(),
        );
        sessions.advance_with(
            now + Duration::from_secs(2),
            &[fullscreen_process(80), extension.clone()],
            Some(&[]),
            |_| true,
            |_| panic!("intentional UI exit must not relaunch"),
        );
        assert!(!sessions.by_ticket[&ticket].return_allowed);
        sessions.advance_with(
            now + Duration::from_secs(3),
            &[extension],
            Some(&[]),
            |_| true,
            |_| panic!("closed fullscreen UI must not relaunch"),
        );
        assert_eq!(sessions.status(&ticket)["state"], "stopped");
    }

    #[test]
    fn playnite_exiting_with_unknown_fullscreen_scan_retains_running_ticket() {
        let now = Instant::now();
        let mut sessions = FullscreenSessions::default();
        let ticket = begin_running_session(&mut sessions, now);
        let extension = extension_process(31, 310);
        queue_status(
            &sessions,
            &ticket,
            status(FullscreenStatusKind::PlayniteExiting, None),
            extension.clone(),
        );

        sessions.advance_with_scanner(
            now + Duration::from_secs(2),
            &[extension],
            Some(&[]),
            |_| None,
            |_| true,
            |_| panic!("intentional exit must not relaunch"),
        );

        assert_eq!(sessions.status(&ticket)["state"], "running");
        assert_eq!(sessions.active_count(), 1);
    }

    #[test]
    fn playnite_exiting_keeps_playing_ticket_when_exact_scan_finds_ui() {
        let now = Instant::now();
        let mut sessions = FullscreenSessions::default();
        let ticket = begin_running_session(&mut sessions, now);
        let extension = extension_process(31, 310);
        queue_status(
            &sessions,
            &ticket,
            status(
                FullscreenStatusKind::GameStarted,
                Some("a1b2c3d4-1234-5678-9abc-0123456789ab"),
            ),
            extension.clone(),
        );
        sessions.advance_with(
            now + Duration::from_secs(2),
            &[fullscreen_process(80), extension.clone()],
            Some(&[game_process(44)]),
            |_| true,
            |_| panic!("active game must not relaunch"),
        );
        assert_eq!(sessions.status(&ticket)["state"], "playing");

        queue_status(
            &sessions,
            &ticket,
            status(FullscreenStatusKind::PlayniteExiting, None),
            extension.clone(),
        );
        sessions.advance_with_scanner(
            now + Duration::from_secs(3),
            &[extension],
            Some(&[]),
            |_| Some(vec![fullscreen_process(80)]),
            |_| true,
            |_| panic!("intentional exit must not relaunch"),
        );

        assert_eq!(sessions.status(&ticket)["state"], "playing");
        assert_eq!(sessions.active_count(), 1);
    }

    #[test]
    fn reused_fullscreen_pid_or_changed_image_never_receives_focus_or_relaunch() {
        let now = Instant::now();
        let mut sessions = FullscreenSessions::default();
        let ticket = begin_running_session(&mut sessions, now);
        let extension = extension_process(31, 310);
        let game_id = "a1b2c3d4-1234-5678-9abc-0123456789ab";
        queue_status(
            &sessions,
            &ticket,
            status(FullscreenStatusKind::GameStarted, Some(game_id)),
            extension.clone(),
        );
        let reused_fullscreen = ProcessImage {
            process_id: 80,
            path: r"C:\Playnite\Playnite.FullscreenApp.exe".into(),
            creation_time: 800,
        };
        sessions.advance_with(
            now + Duration::from_secs(2),
            &[reused_fullscreen.clone(), extension.clone()],
            Some(&[game_process(44)]),
            |_| true,
            |_| panic!("active game does not relaunch"),
        );
        queue_status(
            &sessions,
            &ticket,
            status(FullscreenStatusKind::GameStopped, Some(game_id)),
            extension.clone(),
        );
        sessions.advance_with(
            now + Duration::from_secs(3),
            &[reused_fullscreen.clone(), extension.clone()],
            Some(&[]),
            |_| true,
            |_| panic!("exit confirmation remains grace bounded"),
        );
        sessions.advance_with(
            now + Duration::from_secs(6),
            &[reused_fullscreen, extension],
            Some(&[]),
            |_| true,
            |_| panic!("a reused PID must never focus or relaunch"),
        );
        assert_eq!(sessions.status(&ticket)["state"], "cleanup_failed");
    }

    #[test]
    fn game_exit_relaunches_only_the_canonical_install_and_tracks_its_new_process() {
        let now = Instant::now();
        let mut sessions = FullscreenSessions::default();
        let ticket = begin_running_session(&mut sessions, now);
        let extension = extension_process(31, 310);
        let game_id = "a1b2c3d4-1234-5678-9abc-0123456789ab";
        let no_fullscreen = [extension.clone()];
        queue_status(
            &sessions,
            &ticket,
            status(FullscreenStatusKind::GameStarted, Some(game_id)),
            extension.clone(),
        );
        sessions.advance_with(
            now + Duration::from_secs(2),
            &no_fullscreen,
            Some(&[game_process(44)]),
            |_| true,
            |_| panic!("game start does not relaunch"),
        );
        queue_status(
            &sessions,
            &ticket,
            status(FullscreenStatusKind::GameStopped, Some(game_id)),
            extension.clone(),
        );
        sessions.advance_with(
            now + Duration::from_secs(3),
            &no_fullscreen,
            Some(&[game_process(44)]),
            |_| true,
            |_| panic!("game process still runs"),
        );
        sessions.advance_with(
            now + Duration::from_secs(4),
            &no_fullscreen,
            Some(&[]),
            |_| true,
            |_| panic!("wait for the exact-image absence grace"),
        );
        let mut relaunches = 0;
        sessions.advance_with(
            now + Duration::from_secs(7),
            &no_fullscreen,
            Some(&[]),
            |_| true,
            |_| {
                relaunches += 1;
                Ok(launch_process(90))
            },
        );
        assert_eq!(relaunches, 1);
        assert_eq!(sessions.status(&ticket)["state"], "starting");
        sessions.advance_with(
            now + Duration::from_secs(8),
            &[fullscreen_process(95), extension],
            Some(&[]),
            |_| true,
            |_| panic!("the new verified process is already present"),
        );
        assert_eq!(sessions.status(&ticket)["state"], "running");
        assert_eq!(
            sessions.by_ticket[&ticket].fullscreen_process,
            Some(fullscreen_process(95))
        );
    }
}
