//! Authenticated, bounded client for the Playnite extension named-pipe protocol.
//!
//! The extension pipe name alone is not trusted. Each connection verifies the
//! kernel-reported server PID and image against current Playnite discovery, and
//! the negotiated data pipe must belong to the same process. This does not
//! protect against a compromised Playnite process on the same host.

use crate::discovery::ProcessImage;
use crate::focus::FocusPolicy;
#[cfg(windows)]
use crate::focus::FocusRetry;
#[cfg(any(windows, test))]
use crate::focus::FocusTarget;
use crate::launch::StopRequest;
#[cfg(windows)]
use crate::launch::{installation_roots, refresh_processes_for_install};
use crate::snapshot::Snapshot;
#[cfg(windows)]
use crate::snapshot::SnapshotAssembler;
#[cfg(any(windows, test))]
use crate::snapshot::valid_playnite_id;
#[cfg(windows)]
use std::sync::mpsc::TryRecvError;
use std::sync::mpsc::{Receiver, Sender};
#[cfg(windows)]
use std::sync::{Arc, Mutex, OnceLock, mpsc};
#[cfg(windows)]
use std::thread;
use std::time::Duration;
#[cfg(windows)]
use std::time::Instant;

#[cfg(windows)]
const CONTROL_PIPE: &str = r"\\.\pipe\Sunshine.PlayniteExtension";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(8);
pub(crate) const OBSERVE_TIMEOUT: Duration = CONNECT_TIMEOUT;
#[cfg(windows)]
const SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(30);
#[cfg(windows)]
const LAUNCH_START_TIMEOUT: Duration = Duration::from_secs(60);
#[cfg(windows)]
const MAX_TOTAL_BYTES: usize = 64 * 1024 * 1024;
#[cfg(any(windows, test))]
const MAX_STATUS_LINE_BYTES: usize = 8 * 1024;

#[cfg(windows)]
fn fresh_connection_nonce() -> Result<String, String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|_| "Playnite connector nonce entropy unavailable")?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(
    not(any(windows, test)),
    expect(
        dead_code,
        reason = "native status events are produced only by the Windows Playnite connector"
    )
)]
pub(crate) enum FullscreenStatusKind {
    GameStarted,
    GameStopped,
    PlayniteExiting,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct FullscreenStatus {
    pub(crate) kind: FullscreenStatusKind,
    pub(crate) game_id: Option<String>,
    pub(crate) install_directory: Option<String>,
    pub(crate) executable: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(
    not(any(windows, test)),
    expect(
        dead_code,
        reason = "native status events are produced only by the Windows Playnite connector"
    )
)]
pub(crate) enum FullscreenWatchEvent {
    Status {
        status: FullscreenStatus,
        extension_process: ProcessImage,
    },
    Disconnected {
        extension_process: ProcessImage,
    },
}

#[cfg_attr(test, allow(dead_code))]
pub(crate) enum FullscreenWatchCommand {
    #[cfg_attr(
        not(windows),
        expect(
            dead_code,
            reason = "process updates are consumed by the Windows Playnite watcher"
        )
    )]
    UpdateProcesses(Vec<ProcessImage>),
    Cancel,
}

#[cfg(windows)]
struct Pipe(windows_sys::Win32::Foundation::HANDLE);

#[cfg(windows)]
impl Pipe {
    fn open(path: &str, deadline: Instant) -> Result<Self, String> {
        use windows_sys::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE};
        use windows_sys::Win32::Storage::FileSystem::{
            CreateFileW, FILE_ATTRIBUTE_NORMAL, OPEN_EXISTING,
        };
        use windows_sys::Win32::System::Pipes::WaitNamedPipeW;

        let wide = path.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
        while Instant::now() < deadline {
            // SAFETY: wide is NUL terminated and the handle is owned on success.
            let opened = unsafe {
                CreateFileW(
                    wide.as_ptr(),
                    GENERIC_READ | GENERIC_WRITE,
                    0,
                    std::ptr::null(),
                    OPEN_EXISTING,
                    FILE_ATTRIBUTE_NORMAL,
                    std::ptr::null_mut(),
                )
            };
            if opened != INVALID_HANDLE_VALUE {
                return Ok(Self(opened));
            }
            // SAFETY: the same NUL-terminated pipe path is valid for the wait API.
            unsafe { WaitNamedPipeW(wide.as_ptr(), 100) };
        }
        Err("Playnite extension pipe did not become available before timeout".into())
    }

    fn available(&self) -> Result<u32, String> {
        use windows_sys::Win32::System::Pipes::PeekNamedPipe;
        let mut bytes = 0;
        // SAFETY: the pipe handle is owned and the out parameter is valid.
        let result = unsafe {
            PeekNamedPipe(
                self.0,
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                &mut bytes,
                std::ptr::null_mut(),
            )
        };
        if result == 0 {
            Err("Playnite extension pipe disconnected".into())
        } else {
            Ok(bytes)
        }
    }

    fn read_some(&self, output: &mut [u8]) -> Result<usize, String> {
        use windows_sys::Win32::Storage::FileSystem::ReadFile;
        let mut read = 0;
        // SAFETY: output is writable; this synchronous read is limited to bytes
        // already reported by PeekNamedPipe, so it cannot wait for future data.
        let result = unsafe {
            ReadFile(
                self.0,
                output.as_mut_ptr(),
                output.len() as u32,
                &mut read,
                std::ptr::null_mut(),
            )
        };
        if result == 0 {
            Err("Playnite extension pipe read failed".into())
        } else {
            Ok(read as usize)
        }
    }

    fn write_all(&self, input: &[u8]) -> Result<(), String> {
        use windows_sys::Win32::Storage::FileSystem::WriteFile;
        let mut written = 0;
        // SAFETY: input remains alive for the synchronous write.
        let result = unsafe {
            WriteFile(
                self.0,
                input.as_ptr(),
                input.len() as u32,
                &mut written,
                std::ptr::null_mut(),
            )
        };
        if result == 0 || written as usize != input.len() {
            Err("Playnite extension pipe write failed".into())
        } else {
            Ok(())
        }
    }
}

#[cfg(windows)]
impl Drop for Pipe {
    fn drop(&mut self) {
        use windows_sys::Win32::Foundation::CloseHandle;
        // SAFETY: this wrapper uniquely owns the handle.
        unsafe { CloseHandle(self.0) };
    }
}

#[cfg(windows)]
fn validated_server_process(pipe: &Pipe, expected: &[ProcessImage]) -> Result<u32, String> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Pipes::GetNamedPipeServerProcessId;
    use windows_sys::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
    };

    let mut process_id = 0;
    // SAFETY: the owned pipe handle and writable process ID output are valid.
    if unsafe { GetNamedPipeServerProcessId(pipe.0, &mut process_id) } == 0 {
        return Err("Playnite pipe server identity was unavailable".into());
    }
    let Some(expected_image) = expected.iter().find(|image| image.process_id == process_id) else {
        return Err("Playnite pipe was served by an unexpected process".into());
    };
    // SAFETY: the process ID came from the kernel pipe handle; query access is read-only.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, process_id) };
    if process.is_null() {
        return Err("Playnite pipe server image could not be verified".into());
    }
    let mut path = [0u16; 32_768];
    let mut length = path.len() as u32;
    // SAFETY: process is valid and path is a writable buffer for length UTF-16 units.
    let queried = unsafe { QueryFullProcessImageNameW(process, 0, path.as_mut_ptr(), &mut length) };
    let mut creation_time = windows_sys::Win32::Foundation::FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    // SAFETY: process is valid and creation_time is writable output.
    let has_creation_time = unsafe {
        GetProcessTimes(
            process,
            &mut creation_time,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    } != 0;
    // SAFETY: process is owned by this helper.
    unsafe { CloseHandle(process) };
    if queried == 0 || !has_creation_time || length as usize > path.len() {
        return Err("Playnite pipe server image could not be read".into());
    }
    let actual_path = String::from_utf16_lossy(&path[..length as usize]);
    let actual_creation_time = crate::discovery::filetime_value(creation_time);
    if !actual_path.eq_ignore_ascii_case(&expected_image.path)
        || actual_creation_time != expected_image.creation_time
    {
        return Err("Playnite pipe server identity changed after process discovery".into());
    }
    Ok(process_id)
}

#[cfg(windows)]
fn connect_role(
    expected_processes: &[ProcessImage],
    role: &str,
    game_id: Option<&str>,
    mode: Option<&str>,
    nonce: Option<&str>,
) -> Result<(Pipe, ProcessImage), String> {
    use std::thread;

    if expected_processes.is_empty() {
        return Err("Playnite process discovery returned no trusted process IDs".into());
    }
    let connect_deadline = Instant::now() + CONNECT_TIMEOUT;
    let control = Pipe::open(CONTROL_PIPE, connect_deadline)?;
    let control_server_id = validated_server_process(&control, expected_processes)?;
    let control_server = expected_processes
        .iter()
        .find(|process| process.process_id == control_server_id)
        .cloned()
        .ok_or_else(|| "Playnite pipe server identity was not discovered".to_string())?;
    let mut handshake = [0u8; 80];
    let mut handshake_bytes = 0;
    while handshake_bytes < handshake.len() && Instant::now() < connect_deadline {
        let available = control.available()? as usize;
        if available == 0 {
            thread::sleep(Duration::from_millis(10));
            continue;
        }
        let count = (handshake.len() - handshake_bytes).min(available);
        handshake_bytes +=
            control.read_some(&mut handshake[handshake_bytes..handshake_bytes + count])?;
    }
    if handshake_bytes != handshake.len() {
        return Err("Playnite extension did not complete its pipe handshake".into());
    }
    let pipe_name = decode_pipe_name(&handshake)?;
    control.write_all(&[0x02])?;

    let data_pipe = Pipe::open(&pipe_name, Instant::now() + CONNECT_TIMEOUT)?;
    if validated_server_process(&data_pipe, expected_processes)? != control_server_id {
        return Err(
            "Playnite data pipe server did not match the authenticated control server".into(),
        );
    }
    let mut hello = serde_json::json!({"type":"hello","role":role,"pid":std::process::id()});
    if let Some(game_id) = game_id {
        hello["gameId"] = game_id.into();
    }
    if let Some(mode) = mode {
        hello["mode"] = mode.into();
    }
    if let Some(nonce) = nonce {
        hello["nonce"] = nonce.into();
    }
    let mut line = serde_json::to_vec(&hello).map_err(|_| "Playnite hello serialization failed")?;
    line.push(b'\n');
    data_pipe.write_all(&line)?;
    Ok((data_pipe, control_server))
}

#[cfg(windows)]
pub fn request_snapshot(expected_processes: &[ProcessImage]) -> Result<Snapshot, String> {
    let (reply, response) = mpsc::channel();
    sunshine_actor_channels()
        .ok_or("Playnite Sunshine connector is not running")?
        .0
        .try_send(SunshineCommand::Snapshot {
            processes: expected_processes.to_vec(),
            reply,
        })
        .map_err(|_| "Playnite Sunshine connector stopped".to_owned())?;
    response
        .recv_timeout(SNAPSHOT_TIMEOUT + CONNECT_TIMEOUT)
        .map_err(|_| "Playnite extension snapshot timed out".to_owned())?
}

#[cfg(not(windows))]
pub fn request_snapshot(_expected_processes: &[ProcessImage]) -> Result<Snapshot, String> {
    Err("Playnite integration is available only on Windows".into())
}

#[derive(Clone)]
pub(crate) struct SnapshotEvent {
    pub(crate) snapshot: Snapshot,
    pub(crate) processes: Vec<ProcessImage>,
    pub(crate) settings_epoch: u64,
}

#[cfg(windows)]
enum SunshineCommand {
    Observe {
        processes: Vec<ProcessImage>,
        enabled: bool,
        settings_epoch: u64,
        reply: Sender<()>,
    },
    Snapshot {
        processes: Vec<ProcessImage>,
        reply: Sender<Result<Snapshot, String>>,
    },
    Send {
        processes: Vec<ProcessImage>,
        line: Vec<u8>,
        command: String,
        request_id: String,
        deadline: Instant,
        reply: Sender<Result<(), String>>,
    },
    Shutdown,
}

#[cfg(windows)]
struct SunshineActor {
    commands: mpsc::SyncSender<SunshineCommand>,
    snapshots: SharedSnapshots,
    worker: thread::JoinHandle<()>,
}

#[cfg(windows)]
type SharedSnapshots = Arc<Mutex<Receiver<SnapshotEvent>>>;

#[cfg(windows)]
fn sunshine_actor_slot() -> &'static Mutex<Option<SunshineActor>> {
    static ACTOR: OnceLock<Mutex<Option<SunshineActor>>> = OnceLock::new();
    ACTOR.get_or_init(|| Mutex::new(None))
}

#[cfg(windows)]
fn sunshine_actor_channels() -> Option<(mpsc::SyncSender<SunshineCommand>, SharedSnapshots)> {
    let actor = sunshine_actor_slot().lock().ok()?;
    let actor = actor.as_ref()?;
    Some((actor.commands.clone(), Arc::clone(&actor.snapshots)))
}

pub(crate) fn start_sunshine_actor() {
    #[cfg(windows)]
    if let Ok(mut slot) = sunshine_actor_slot().lock()
        && slot.is_none()
    {
        let (commands, requests) = mpsc::sync_channel(8);
        let (snapshots, events) = mpsc::sync_channel(1);
        let worker = thread::spawn(move || run_sunshine_actor(requests, snapshots));
        *slot = Some(SunshineActor {
            commands,
            snapshots: Arc::new(Mutex::new(events)),
            worker,
        });
    }
}

pub(crate) fn stop_sunshine_actor() {
    #[cfg(windows)]
    if let Ok(mut slot) = sunshine_actor_slot().lock()
        && let Some(actor) = slot.take()
    {
        let _ = actor.commands.try_send(SunshineCommand::Shutdown);
        let _ = actor.worker.join();
    }
}

pub(crate) fn observe_sunshine_processes(
    processes: &[ProcessImage],
    enabled: bool,
    settings_epoch: u64,
) -> Option<Receiver<()>> {
    #[cfg(windows)]
    {
        let (commands, _) = sunshine_actor_channels()?;
        let (reply, observed) = mpsc::channel();
        commands
            .try_send(SunshineCommand::Observe {
                processes: processes.to_vec(),
                enabled,
                settings_epoch,
                reply,
            })
            .ok()?;
        Some(observed)
    }
    #[cfg(not(windows))]
    {
        let _ = (processes, enabled, settings_epoch);
        let (reply, observed) = std::sync::mpsc::channel();
        let _ = reply.send(());
        Some(observed)
    }
}

pub(crate) fn take_unsolicited_snapshot() -> Option<SnapshotEvent> {
    #[cfg(windows)]
    {
        let (_, snapshots) = sunshine_actor_channels()?;
        let snapshots = snapshots.lock().ok()?;
        let mut latest = None;
        while let Ok(snapshot) = snapshots.try_recv() {
            latest = Some(snapshot);
        }
        latest
    }
    #[cfg(not(windows))]
    {
        None
    }
}

#[cfg(windows)]
struct SunshineState {
    processes: Vec<ProcessImage>,
    enabled: bool,
    settings_epoch: u64,
    pipe: Option<Pipe>,
    server_process: Option<ProcessImage>,
    connection_nonce: Option<String>,
    assembler: SnapshotAssembler,
    line: Vec<u8>,
    received: usize,
    pending_snapshot: Option<PendingSnapshot>,
    pending_command: Option<PendingCommand>,
}

#[cfg(windows)]
struct PendingSnapshot {
    reply: Sender<Result<Snapshot, String>>,
    deadline: Instant,
    request_id: Option<String>,
}

#[cfg(windows)]
struct PendingCommand {
    reply: Sender<Result<(), String>>,
    command: String,
    request_id: String,
    deadline: Instant,
}

#[cfg(windows)]
impl SunshineState {
    fn new() -> Self {
        Self {
            processes: Vec::new(),
            enabled: false,
            settings_epoch: 0,
            pipe: None,
            server_process: None,
            connection_nonce: None,
            assembler: SnapshotAssembler::default(),
            line: Vec::new(),
            received: 0,
            pending_snapshot: None,
            pending_command: None,
        }
    }

    fn disconnect(&mut self, reason: &str) {
        self.pipe = None;
        self.server_process = None;
        self.connection_nonce = None;
        self.assembler = SnapshotAssembler::default();
        self.line.clear();
        self.received = 0;
        if let Some(pending) = self.pending_snapshot.take() {
            let _ = pending.reply.send(Err(reason.into()));
        }
        if let Some(pending) = self.pending_command.take() {
            let _ = pending.reply.send(Err(reason.into()));
        }
    }

    fn connect(&mut self) -> Result<bool, String> {
        if self.pipe.is_some() {
            return Ok(false);
        }
        let nonce = fresh_connection_nonce()?;
        let (pipe, server_process) =
            connect_role(&self.processes, "sunshine", None, None, Some(&nonce))?;
        self.server_process = Some(server_process);
        self.pipe = Some(pipe);
        self.assembler = SnapshotAssembler::for_connection(nonce.clone());
        self.connection_nonce = Some(nonce);
        Ok(true)
    }

    fn verify_server(&self) -> Result<(), String> {
        let pipe = self
            .pipe
            .as_ref()
            .ok_or("Playnite Sunshine pipe is not connected")?;
        let server = self
            .server_process
            .as_ref()
            .ok_or("Playnite Sunshine server identity is missing")?;
        if !self
            .processes
            .iter()
            .any(|process| process.same_identity(server))
            || validated_server_process(pipe, std::slice::from_ref(server))? != server.process_id
        {
            return Err("Playnite Sunshine server identity changed".into());
        }
        Ok(())
    }

    fn assembler_nonce_matches(&self, nonce: Option<&str>) -> bool {
        self.connection_nonce.as_deref() == nonce
    }

    fn handle(&mut self, command: SunshineCommand) {
        match command {
            SunshineCommand::Observe {
                processes,
                enabled,
                settings_epoch,
                reply,
            } => {
                if self.processes != processes || self.enabled != enabled {
                    self.disconnect("Playnite process or enabled state changed");
                }
                self.processes = processes;
                self.enabled = enabled;
                self.settings_epoch = settings_epoch;
                let _ = reply.send(());
            }
            SunshineCommand::Snapshot { processes, reply } => {
                if !self.enabled || self.processes != processes {
                    let _ = reply.send(Err("Playnite snapshot instance changed".into()));
                    return;
                }
                match self.connect() {
                    Ok(new_connection) => {
                        let request_id = if new_connection {
                            None
                        } else {
                            match fresh_connection_nonce() {
                                Ok(id) => Some(id),
                                Err(error) => {
                                    let _ = reply.send(Err(error));
                                    return;
                                }
                            }
                        };
                        if !new_connection
                            && let Some(pipe) = &self.pipe
                            && let Err(error) = pipe.write_all(
                                &extension_command_line_with_request(
                                    "snapshot",
                                    None,
                                    request_id.as_deref(),
                                )
                                .unwrap(),
                            )
                        {
                            self.disconnect(&error);
                            let _ = reply.send(Err(error));
                            return;
                        }
                        let pending = PendingSnapshot {
                            reply,
                            deadline: Instant::now() + SNAPSHOT_TIMEOUT,
                            request_id,
                        };
                        if let Some(previous) = self.pending_snapshot.replace(pending) {
                            let _ = previous
                                .reply
                                .send(Err("Playnite snapshot request superseded".into()));
                        }
                    }
                    Err(error) => {
                        let _ = reply.send(Err(error));
                    }
                }
            }
            SunshineCommand::Send {
                processes,
                line,
                command,
                request_id,
                deadline,
                reply,
            } => {
                if Instant::now() >= deadline {
                    let _ = reply.send(Err("Playnite command expired before dispatch".into()));
                    return;
                }
                if self.pending_command.is_some() {
                    let _ = reply.send(Err("Playnite command already pending".into()));
                    return;
                }
                if self.processes != processes || (!self.enabled && command != "stop") {
                    let _ = reply.send(Err("Playnite command instance changed".into()));
                    return;
                }
                let result = self.connect().and_then(|_| {
                    if Instant::now() >= deadline {
                        return Err("Playnite command expired before pipe write".into());
                    }
                    let pipe = self.pipe.as_ref().unwrap();
                    self.verify_server()?;
                    pipe.write_all(&line)
                });
                if result.is_err() {
                    self.disconnect("Playnite Sunshine command connection failed");
                    let _ = reply.send(result);
                } else {
                    self.pending_command = Some(PendingCommand {
                        reply,
                        command,
                        request_id,
                        deadline: Instant::now() + CONNECT_TIMEOUT,
                    });
                }
            }
            SunshineCommand::Shutdown => self.disconnect("Playnite module stopped"),
        }
    }

    fn read_available(&mut self, events: &mpsc::SyncSender<SnapshotEvent>) -> Result<(), String> {
        let Some(pipe) = &self.pipe else {
            return Ok(());
        };
        self.verify_server()?;
        let available = pipe.available()? as usize;
        if available == 0 {
            return Ok(());
        }
        let mut buffer = [0u8; 8192];
        let read_length = available.min(buffer.len());
        let count = pipe.read_some(&mut buffer[..read_length])?;
        self.consume_bytes(&buffer[..count], events)
    }

    fn consume_bytes(
        &mut self,
        bytes: &[u8],
        events: &mpsc::SyncSender<SnapshotEvent>,
    ) -> Result<(), String> {
        self.received = self.received.saturating_add(bytes.len());
        if self.received > MAX_TOTAL_BYTES {
            return Err("Playnite extension snapshot exceeded its byte limit".into());
        }
        for byte in bytes {
            if *byte == b'\n' {
                if self.line.last() == Some(&b'\r') {
                    self.line.pop();
                }
                if let Some(result) = parse_command_result(&self.line)
                    && let Some(pending) = self.pending_command.as_ref()
                    && Some(pending.request_id.as_str()) == result.request_id.as_deref()
                    && pending.command == result.command
                    && self.assembler_nonce_matches(result.nonce.as_deref())
                    && let Some(pending) = self.pending_command.take()
                {
                    let _ = pending.reply.send(if result.success {
                        Ok(())
                    } else {
                        Err("Playnite extension rejected command".into())
                    });
                }
                if let Some((request_id, result)) =
                    self.assembler.receive_line_with_request(&self.line)
                {
                    self.received = 0;
                    let matches_pending = self
                        .pending_snapshot
                        .as_ref()
                        .is_some_and(|pending| pending.request_id == request_id);
                    if matches_pending {
                        if let Some(pending) = self.pending_snapshot.take() {
                            let _ = pending.reply.send(result.map_err(str::to_owned));
                        }
                    } else if self.enabled
                        && self.pending_snapshot.is_none()
                        && request_id.is_none()
                        && let Ok(snapshot) = result
                    {
                        let _ = events.try_send(SnapshotEvent {
                            snapshot,
                            processes: self.processes.clone(),
                            settings_epoch: self.settings_epoch,
                        });
                    }
                }
                self.line.clear();
            } else {
                self.line.push(*byte);
                if self.line.len() > crate::snapshot::MAX_LINE_BYTES {
                    return Err("Playnite extension message exceeded the line limit".into());
                }
            }
        }
        Ok(())
    }
}

#[cfg(windows)]
fn run_sunshine_actor(
    commands: Receiver<SunshineCommand>,
    events: mpsc::SyncSender<SnapshotEvent>,
) {
    let mut state = SunshineState::new();
    loop {
        if state
            .pending_snapshot
            .as_ref()
            .is_some_and(|pending| Instant::now() >= pending.deadline)
            && let Some(pending) = state.pending_snapshot.take()
        {
            let _ = pending
                .reply
                .send(Err("Playnite extension snapshot timed out".into()));
        }
        if state
            .pending_command
            .as_ref()
            .is_some_and(|pending| Instant::now() >= pending.deadline)
        {
            state.disconnect("Playnite extension command acknowledgement timed out");
        }
        match commands.recv_timeout(Duration::from_millis(10)) {
            Ok(SunshineCommand::Shutdown) => {
                state.disconnect("Playnite module stopped");
                return;
            }
            Ok(command) => state.handle(command),
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        while let Ok(command) = commands.try_recv() {
            if matches!(command, SunshineCommand::Shutdown) {
                state.disconnect("Playnite module stopped");
                return;
            }
            state.handle(command);
        }
        if let Err(error) = state.read_available(&events) {
            state.disconnect(&error);
        }
    }
}

#[cfg(windows)]
pub fn watch_game(
    expected_processes: &[ProcessImage],
    game_id: &str,
    stop_requests: Receiver<StopRequest>,
    events: Sender<super::launch::State>,
    recovery: bool,
    focus_policy: FocusPolicy,
) {
    let fail = |state| {
        let _ = events.send(state);
    };
    let mut line = Vec::new();
    let mut buffer = [0u8; 4096];
    let mut received = 0usize;
    let mut started = false;
    let mut focus_retry = FocusRetry::default();
    let mut stopping = recovery;
    let mut stop_deadline = None;
    let launch_deadline = Instant::now() + LAUNCH_START_TIMEOUT;
    let mut expected_processes = expected_processes.to_vec();

    let (mut pipe, _) =
        match connect_role(&expected_processes, "launcher", Some(game_id), None, None) {
            Ok(pipe) => pipe,
            Err(_) => {
                fail(if recovery {
                    super::launch::State::CleanupFailed
                } else {
                    state_after_start_failure(false)
                });
                return;
            }
        };
    if recovery {
        let request = match stop_requests.try_recv() {
            Ok(request) => request,
            Err(_) => {
                fail(super::launch::State::CleanupFailed);
                return;
            }
        };
        expected_processes = match refresh_processes_for_install(
            &installation_roots(&expected_processes),
            request.processes,
        ) {
            Ok(processes) => processes,
            Err(_) => {
                fail(super::launch::State::CleanupFailed);
                return;
            }
        };
        if validated_server_process(&pipe, &expected_processes).is_err() {
            (pipe, _) =
                match connect_role(&expected_processes, "launcher", Some(game_id), None, None) {
                    Ok(pipe) => pipe,
                    Err(_) => {
                        fail(super::launch::State::CleanupFailed);
                        return;
                    }
                };
        }
        if send_command(&expected_processes, "stop", game_id).is_err() {
            fail(super::launch::State::CleanupFailed);
            return;
        }
        stop_deadline = Some(Instant::now() + Duration::from_secs(request.timeout_seconds));
        let _ = events.send(super::launch::State::Stopping);
    } else if send_command(&expected_processes, "launch", game_id).is_err() {
        fail(state_after_start_failure(true));
        return;
    }

    loop {
        match stop_requests.try_recv() {
            Ok(request) if !stopping => {
                expected_processes = match refresh_processes_for_install(
                    &installation_roots(&expected_processes),
                    request.processes,
                ) {
                    Ok(processes) => processes,
                    Err(_) => {
                        fail(super::launch::State::CleanupFailed);
                        return;
                    }
                };
                if validated_server_process(&pipe, &expected_processes).is_err() {
                    (pipe, _) = match connect_role(
                        &expected_processes,
                        "launcher",
                        Some(game_id),
                        None,
                        None,
                    ) {
                        Ok(pipe) => pipe,
                        Err(_) => {
                            fail(super::launch::State::CleanupFailed);
                            return;
                        }
                    };
                }
                if send_command(&expected_processes, "stop", game_id).is_err() {
                    fail(super::launch::State::CleanupFailed);
                    return;
                }
                stopping = true;
                focus_retry.cancel();
                stop_deadline = Some(Instant::now() + Duration::from_secs(request.timeout_seconds));
                let _ = events.send(super::launch::State::Stopping);
            }
            Ok(_) | Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => return,
        }
        if stop_deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            fail(super::launch::State::CleanupFailed);
            return;
        }
        if !started && !stopping && Instant::now() >= launch_deadline {
            fail(super::launch::State::CleanupFailed);
            return;
        }
        focus_retry.tick_with(Instant::now(), FocusRetry::attempt_windows_focus);

        let available = match pipe.available() {
            Ok(available) => available as usize,
            Err(_) => {
                fail(super::launch::State::CleanupFailed);
                return;
            }
        };
        if available == 0 {
            thread::sleep(Duration::from_millis(10));
            continue;
        }
        let read_length = available.min(buffer.len());
        let count = match pipe.read_some(&mut buffer[..read_length]) {
            Ok(count) => count,
            Err(_) => {
                fail(super::launch::State::CleanupFailed);
                return;
            }
        };
        received = received.saturating_add(count);
        if received > MAX_TOTAL_BYTES {
            fail(super::launch::State::CleanupFailed);
            return;
        }
        for byte in &buffer[..count] {
            if *byte == b'\n' {
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                if let Some(status) = parse_game_status(&line)
                    && status.game_id.eq_ignore_ascii_case(game_id)
                {
                    if let Some(state) = stop_signal_state(&status.name, stopping, recovery) {
                        focus_retry.cancel();
                        let _ = events.send(state);
                        return;
                    }
                    let was_started = started;
                    let state = apply_game_status(&status.name, &mut started, stopping);
                    if status.name == "gameStarted" && !stopping {
                        if was_started {
                            focus_retry.observe_target(status.focus_target);
                        } else {
                            focus_retry.start(focus_policy, status.focus_target, Instant::now());
                        }
                    } else if started && !stopping {
                        focus_retry.observe_target(status.focus_target);
                    }
                    if let Some(state) = state {
                        if state == super::launch::State::Stopped {
                            focus_retry.cancel();
                        }
                        let _ = events.send(state);
                        if state == super::launch::State::Stopped {
                            return;
                        }
                    }
                }
                line.clear();
            } else {
                line.push(*byte);
                if line.len() > MAX_STATUS_LINE_BYTES {
                    fail(super::launch::State::CleanupFailed);
                    return;
                }
            }
        }
    }
}

#[cfg(not(windows))]
pub fn watch_game(
    _expected_processes: &[ProcessImage],
    _game_id: &str,
    _stop_requests: Receiver<StopRequest>,
    events: Sender<super::launch::State>,
    recovery: bool,
    _focus_policy: FocusPolicy,
) {
    let _ = events.send(if recovery {
        super::launch::State::CleanupFailed
    } else {
        super::launch::State::LaunchFailed
    });
}

#[cfg(not(test))]
pub(crate) fn watch_fullscreen(
    initial_processes: &[ProcessImage],
    commands: Receiver<FullscreenWatchCommand>,
    events: Sender<FullscreenWatchEvent>,
) {
    #[cfg(windows)]
    watch_fullscreen_windows(initial_processes, commands, events);
    #[cfg(not(windows))]
    {
        let _ = (initial_processes, commands, events);
    }
}

#[cfg(all(windows, not(test)))]
fn watch_fullscreen_windows(
    initial_processes: &[ProcessImage],
    commands: Receiver<FullscreenWatchCommand>,
    events: Sender<FullscreenWatchEvent>,
) {
    let mut expected_processes = initial_processes.to_vec();
    let mut pipe = None;
    let mut extension_process: Option<ProcessImage> = None;
    let mut line = Vec::new();
    let mut buffer = [0u8; 4096];

    loop {
        loop {
            match commands.try_recv() {
                Ok(FullscreenWatchCommand::UpdateProcesses(processes)) => {
                    expected_processes = processes;
                }
                Ok(FullscreenWatchCommand::Cancel) | Err(TryRecvError::Disconnected) => return,
                Err(TryRecvError::Empty) => break,
            }
        }

        if pipe.is_none() {
            if expected_processes.is_empty() {
                thread::sleep(Duration::from_millis(100));
                continue;
            }
            match connect_role(
                &expected_processes,
                "launcher",
                None,
                Some("fullscreen"),
                None,
            ) {
                Ok((connected_pipe, server_process)) => {
                    extension_process = Some(server_process);
                    pipe = Some(connected_pipe);
                    line.clear();
                }
                Err(_) => {
                    thread::sleep(Duration::from_millis(100));
                    continue;
                }
            }
        }

        let current_pipe = pipe.as_ref().expect("connected pipe was just initialized");
        let available = match current_pipe.available() {
            Ok(available) => available as usize,
            Err(_) => {
                if let Some(extension_process) = extension_process.clone() {
                    let _ = events.send(FullscreenWatchEvent::Disconnected { extension_process });
                }
                pipe = None;
                extension_process = None;
                line.clear();
                thread::sleep(Duration::from_millis(100));
                continue;
            }
        };
        if available == 0 {
            thread::sleep(Duration::from_millis(10));
            continue;
        }
        let read_length = available.min(buffer.len());
        let count = match current_pipe.read_some(&mut buffer[..read_length]) {
            Ok(count) => count,
            Err(_) => {
                if let Some(extension_process) = extension_process.clone() {
                    let _ = events.send(FullscreenWatchEvent::Disconnected { extension_process });
                }
                pipe = None;
                extension_process = None;
                line.clear();
                thread::sleep(Duration::from_millis(100));
                continue;
            }
        };
        if consume_fullscreen_status_bytes(&mut line, &buffer[..count], |status| {
            if let Some(extension_process) = extension_process.as_ref() {
                let _ = events.send(FullscreenWatchEvent::Status {
                    status,
                    extension_process: extension_process.clone(),
                });
            }
        })
        .is_err()
        {
            // A malformed connection must not permanently end this session's
            // watcher. Retire the pipe and reconnect to a verified process.
            if let Some(extension_process) = extension_process.take() {
                let _ = events.send(FullscreenWatchEvent::Disconnected { extension_process });
            }
            pipe = None;
            line.clear();
        }
    }
}

#[cfg(any(windows, test))]
fn consume_fullscreen_status_bytes(
    line: &mut Vec<u8>,
    bytes: &[u8],
    mut on_status: impl FnMut(FullscreenStatus),
) -> Result<(), ()> {
    for byte in bytes {
        if *byte == b'\n' {
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            if let Some(status) = parse_fullscreen_status(line) {
                on_status(status);
            }
            line.clear();
        } else {
            line.push(*byte);
            if line.len() > MAX_STATUS_LINE_BYTES {
                return Err(());
            }
        }
    }
    Ok(())
}

#[cfg(windows)]
fn send_command(
    expected_processes: &[ProcessImage],
    command: &str,
    game_id: &str,
) -> Result<(), String> {
    let (reply, response) = mpsc::channel();
    let request_id = fresh_connection_nonce()?;
    let deadline = Instant::now() + CONNECT_TIMEOUT + CONNECT_TIMEOUT;
    let deadline_unix_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| "Playnite command clock was unavailable")?
        .as_millis()
        .saturating_add(16_000);
    let deadline_unix_ms =
        u64::try_from(deadline_unix_ms).map_err(|_| "Playnite command deadline was invalid")?;
    let message = serde_json::json!({"type":"command","command":command,"id":game_id,"requestId":request_id,"deadlineUnixMs":deadline_unix_ms});
    let mut line =
        serde_json::to_vec(&message).map_err(|_| "Playnite command serialization failed")?;
    line.push(b'\n');
    sunshine_actor_channels()
        .ok_or("Playnite Sunshine connector is not running")?
        .0
        .try_send(SunshineCommand::Send {
            processes: expected_processes.to_vec(),
            line,
            command: command.to_owned(),
            request_id,
            deadline,
            reply,
        })
        .map_err(|_| "Playnite Sunshine connector stopped".to_owned())?;
    response
        .recv_timeout(CONNECT_TIMEOUT + CONNECT_TIMEOUT)
        .map_err(|_| "Playnite extension command timed out".to_owned())?
}

#[cfg(windows)]
struct CommandResult {
    command: String,
    request_id: Option<String>,
    nonce: Option<String>,
    success: bool,
}

#[cfg(windows)]
fn parse_command_result(line: &[u8]) -> Option<CommandResult> {
    let message: serde_json::Value = serde_json::from_slice(line).ok()?;
    if message.get("type")?.as_str()? != "commandResult" {
        return None;
    }
    Some(CommandResult {
        command: message.get("command")?.as_str()?.to_owned(),
        request_id: message.get("requestId")?.as_str().map(str::to_owned),
        nonce: message.get("nonce")?.as_str().map(str::to_owned),
        success: message.get("success")?.as_bool()?,
    })
}

#[cfg(any(windows, test))]
#[cfg(test)]
fn extension_command_line(command: &str, game_id: Option<&str>) -> Result<Vec<u8>, String> {
    extension_command_line_with_request(command, game_id, None)
}

#[cfg(any(windows, test))]
fn extension_command_line_with_request(
    command: &str,
    game_id: Option<&str>,
    request_id: Option<&str>,
) -> Result<Vec<u8>, String> {
    let mut message = serde_json::json!({"type":"command","command":command});
    if let Some(game_id) = game_id {
        message["id"] = game_id.into();
    }
    if let Some(request_id) = request_id {
        message["requestId"] = request_id.into();
    }
    let mut line = serde_json::to_vec(&message)
        .map_err(|_| "Playnite command serialization failed".to_owned())?;
    line.push(b'\n');
    Ok(line)
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg(any(windows, test))]
struct GameStatus {
    name: String,
    game_id: String,
    focus_target: FocusTarget,
}

#[cfg(any(windows, test))]
fn parse_fullscreen_status(line: &[u8]) -> Option<FullscreenStatus> {
    let message: serde_json::Value = serde_json::from_slice(line).ok()?;
    if message["type"] != "status"
        || message.get("ticket").is_some()
        || message.pointer("/status/ticket").is_some()
    {
        return None;
    }
    let status = message.get("status")?;
    let name = status.get("name")?.as_str()?;
    let (kind, game_id) = match name {
        "gameStarted" | "gameStopped" => {
            let game_id = status.get("id")?.as_str()?;
            if !valid_playnite_id(game_id) {
                return None;
            }
            (
                if name == "gameStarted" {
                    FullscreenStatusKind::GameStarted
                } else {
                    FullscreenStatusKind::GameStopped
                },
                Some(game_id.to_ascii_lowercase()),
            )
        }
        "playniteExiting" => (FullscreenStatusKind::PlayniteExiting, None),
        _ => return None,
    };
    Some(FullscreenStatus {
        kind,
        game_id,
        install_directory: status
            .get("installDir")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
        executable: status
            .get("exe")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
    })
}

#[cfg(any(windows, test))]
fn parse_game_status(line: &[u8]) -> Option<GameStatus> {
    let message: serde_json::Value = serde_json::from_slice(line).ok()?;
    if message["type"] != "status" {
        return None;
    }
    if message.get("ticket").is_some() || message.pointer("/status/ticket").is_some() {
        return None;
    }
    let status = message.get("status")?;
    let name = status.get("name")?.as_str()?;
    let game_id = status.get("id")?.as_str()?;
    if !valid_playnite_id(game_id) {
        return None;
    }
    let focus_target = FocusTarget::from_status_paths(
        status.get("installDir").and_then(serde_json::Value::as_str),
        status.get("exe").and_then(serde_json::Value::as_str),
    );
    Some(GameStatus {
        name: name.into(),
        game_id: game_id.into(),
        focus_target,
    })
}

#[cfg(any(windows, test))]
fn apply_game_status(
    name: &str,
    started: &mut bool,
    stopping: bool,
) -> Option<super::launch::State> {
    match name {
        "gameStarted" => {
            *started = true;
            (!stopping).then_some(super::launch::State::Running)
        }
        "gameStopped" if *started => Some(super::launch::State::Stopped),
        _ => None,
    }
}

#[cfg(any(windows, test))]
fn stop_signal_state(name: &str, stopping: bool, recovery: bool) -> Option<super::launch::State> {
    (name == "stopRequested" && stopping && !recovery).then_some(super::launch::State::Stopped)
}

#[cfg(any(windows, test))]
fn state_after_start_failure(command_may_have_reached_extension: bool) -> super::launch::State {
    if command_may_have_reached_extension {
        super::launch::State::CleanupFailed
    } else {
        super::launch::State::LaunchFailed
    }
}

#[cfg(windows)]
fn decode_pipe_name(bytes: &[u8; 80]) -> Result<String, String> {
    let (pairs, remainder) = bytes.as_chunks::<2>();
    if !remainder.is_empty() {
        return Err("Playnite handshake pipe name had an invalid byte count".into());
    }
    let units = pairs
        .iter()
        .map(|pair| u16::from_ne_bytes(*pair))
        .collect::<Vec<_>>();
    let length = units
        .iter()
        .position(|unit| *unit == 0)
        .ok_or("Playnite handshake pipe name was not terminated")?;
    if length == 0 || units[length..].iter().any(|unit| *unit != 0) {
        return Err("Playnite handshake pipe name had invalid padding".into());
    }
    let name = String::from_utf16(&units[..length])
        .map_err(|_| "Playnite handshake pipe name was invalid UTF-16")?;
    if name.chars().any(char::is_control) || name.len() > 240 {
        return Err("Playnite handshake pipe name exceeded its bounds".into());
    }
    if name.starts_with(r"\\.\pipe\") {
        Ok(name)
    } else if !name.contains(['\\', '/']) && !name.is_empty() {
        Ok(format!(r"\\.\pipe\{name}"))
    } else {
        Err("Playnite data pipe name was not a local named pipe".into())
    }
}

#[cfg(test)]
mod tests {
    #[cfg(windows)]
    use super::decode_pipe_name;
    use super::{FullscreenStatusKind, parse_fullscreen_status};
    use super::{
        apply_game_status, extension_command_line, parse_game_status, state_after_start_failure,
    };
    use crate::focus::FocusTarget;
    use crate::launch::State;

    #[test]
    fn oversized_fullscreen_status_disconnects_without_ending_later_observation() {
        use super::{MAX_STATUS_LINE_BYTES, consume_fullscreen_status_bytes};

        let status = b"{\"type\":\"status\",\"status\":{\"name\":\"playniteExiting\"}}\n";
        let mut line = Vec::new();
        let mut observed = Vec::new();
        consume_fullscreen_status_bytes(&mut line, &status[..12], |event| observed.push(event))
            .unwrap();
        consume_fullscreen_status_bytes(&mut line, &status[12..], |event| observed.push(event))
            .unwrap();
        assert_eq!(observed.len(), 1);
        assert_eq!(observed[0].kind, FullscreenStatusKind::PlayniteExiting);

        assert!(
            consume_fullscreen_status_bytes(
                &mut line,
                &vec![b'x'; MAX_STATUS_LINE_BYTES + 1],
                |event| observed.push(event),
            )
            .is_err()
        );
        assert_eq!(observed.len(), 1);

        // The watcher drops the offending pipe and clears its partial line.
        // A newly authenticated connection can still deliver lifecycle state.
        line.clear();
        consume_fullscreen_status_bytes(&mut line, status, |event| observed.push(event)).unwrap();
        assert_eq!(observed.len(), 2);
        assert_eq!(observed[1].kind, FullscreenStatusKind::PlayniteExiting);
    }

    #[cfg(windows)]
    #[test]
    fn shared_transport_routes_requested_and_unsolicited_snapshots_around_other_messages() {
        use super::{PendingSnapshot, SunshineState};
        use crate::discovery::ProcessImage;
        use std::sync::mpsc;

        let process = ProcessImage {
            process_id: 41,
            path: r"C:\Playnite\Playnite.DesktopApp.exe".into(),
            creation_time: 410,
        };
        let mut state = SunshineState::new();
        state.processes = vec![process.clone()];
        state.enabled = true;
        state.settings_epoch = 7;
        state.assembler =
            crate::snapshot::SnapshotAssembler::for_connection("new-generation".into());
        let (events, received_events) = mpsc::sync_channel(1);
        let (reply, received_reply) = mpsc::channel();
        state.pending_snapshot = Some(PendingSnapshot {
            reply,
            deadline: std::time::Instant::now() + super::SNAPSHOT_TIMEOUT,
            request_id: Some("requested".into()),
        });
        let snapshot = concat!(
            "{\"type\":\"snapshotStart\",\"nonce\":\"new-generation\",\"requestId\":\"requested\"}\n",
            "{\"type\":\"status\",\"status\":{\"name\":\"gameStarted\"}}\n",
            "{\"type\":\"plugins\",\"payload\":[]}\n",
            "{\"type\":\"commandResult\",\"command\":\"set-cover\"}\n",
            "{\"type\":\"categories\",\"payload\":[]}\n",
            "{\"type\":\"games\",\"payload\":[] }\n",
            "{\"type\":\"snapshotComplete\",\"nonce\":\"new-generation\",\"requestId\":\"requested\",\"games\":0}\n"
        );
        let unsolicited = snapshot.replace(",\"requestId\":\"requested\"", "");
        state
            .consume_bytes(unsolicited.as_bytes(), &events)
            .unwrap();
        assert!(received_reply.try_recv().is_err());
        assert!(received_events.try_recv().is_err());

        state.consume_bytes(snapshot.as_bytes(), &events).unwrap();
        assert!(received_reply.try_recv().unwrap().unwrap().games.is_empty());
        state
            .consume_bytes(unsolicited.as_bytes(), &events)
            .unwrap();
        let event = received_events.try_recv().unwrap();
        assert!(event.snapshot.games.is_empty());
        assert_eq!(event.processes, vec![process]);
        assert_eq!(event.settings_epoch, 7);
    }

    #[cfg(windows)]
    #[test]
    fn shared_transport_discards_partial_snapshot_on_disable_and_process_restart() {
        use super::{SunshineCommand, SunshineState};
        use crate::discovery::ProcessImage;
        use std::sync::mpsc;

        let old = ProcessImage {
            process_id: 41,
            path: r"C:\Playnite\Playnite.DesktopApp.exe".into(),
            creation_time: 410,
        };
        let new = ProcessImage {
            process_id: 73,
            path: old.path.clone(),
            creation_time: 730,
        };
        let (events, received) = mpsc::sync_channel(1);
        let mut state = SunshineState::new();
        let (reply, _) = mpsc::channel();
        state.handle(SunshineCommand::Observe {
            processes: vec![old.clone()],
            enabled: true,
            settings_epoch: 7,
            reply,
        });
        state
            .consume_bytes(b"{\"type\":\"snapshotStart\"}\n", &events)
            .unwrap();
        let (reply, _) = mpsc::channel();
        state.handle(SunshineCommand::Observe {
            processes: vec![old],
            enabled: false,
            settings_epoch: 8,
            reply,
        });
        state
            .consume_bytes(b"{\"type\":\"snapshotComplete\",\"games\":0}\n", &events)
            .unwrap();
        assert!(received.try_recv().is_err());

        let (reply, _) = mpsc::channel();
        state.handle(SunshineCommand::Observe {
            processes: vec![new.clone()],
            enabled: true,
            settings_epoch: 9,
            reply,
        });
        state
            .consume_bytes(b"{\"type\":\"snapshotComplete\",\"games\":0}\n", &events)
            .unwrap();
        assert!(received.try_recv().is_err());
        assert_eq!(state.processes, vec![new]);
    }

    #[cfg(windows)]
    #[test]
    fn command_result_requires_current_nonce_request_and_command() {
        use super::{PendingCommand, SunshineState};
        use std::sync::mpsc;

        let mut state = SunshineState::new();
        state.connection_nonce = Some("current".into());
        let (events, _) = mpsc::sync_channel(1);
        let (reply, received) = mpsc::channel();
        state.pending_command = Some(PendingCommand {
            reply,
            command: "launch".into(),
            request_id: "request".into(),
            deadline: std::time::Instant::now() + super::CONNECT_TIMEOUT,
        });
        for (nonce, request_id, command) in [
            ("old", "request", "launch"),
            ("current", "old", "launch"),
            ("current", "request", "stop"),
        ] {
            let line = format!(
                "{{\"type\":\"commandResult\",\"nonce\":\"{nonce}\",\"requestId\":\"{request_id}\",\"command\":\"{command}\",\"success\":true}}\n"
            );
            state.consume_bytes(line.as_bytes(), &events).unwrap();
            assert!(received.try_recv().is_err());
        }
        state.consume_bytes(b"{\"type\":\"commandResult\",\"nonce\":\"current\",\"requestId\":\"request\",\"command\":\"launch\",\"success\":false}\n", &events).unwrap();
        assert!(received.try_recv().unwrap().is_err());
        assert!(state.pending_command.is_none());
        let (reply, received) = mpsc::channel();
        state.pending_command = Some(PendingCommand {
            reply,
            command: "stop".into(),
            request_id: "next".into(),
            deadline: std::time::Instant::now() + super::CONNECT_TIMEOUT,
        });
        state.consume_bytes(b"{\"type\":\"commandResult\",\"nonce\":\"current\",\"requestId\":\"next\",\"command\":\"stop\",\"success\":true}\n", &events).unwrap();
        assert!(received.try_recv().unwrap().is_ok());
    }

    #[test]
    fn stop_signal_ends_only_an_active_nonrecovery_stream_stop() {
        use super::stop_signal_state;
        assert_eq!(
            stop_signal_state("stopRequested", true, false),
            Some(super::super::launch::State::Stopped)
        );
        assert_eq!(stop_signal_state("stopRequested", false, false), None);
        assert_eq!(stop_signal_state("stopRequested", true, true), None);
        assert_eq!(stop_signal_state("gameStopped", true, false), None);
    }

    #[cfg(windows)]
    #[test]
    fn sunshine_actor_joins_before_module_reload_and_can_restart() {
        use super::{start_sunshine_actor, stop_sunshine_actor, sunshine_actor_channels};

        start_sunshine_actor();
        assert!(sunshine_actor_channels().is_some());
        stop_sunshine_actor();
        assert!(sunshine_actor_channels().is_none());
        start_sunshine_actor();
        assert!(sunshine_actor_channels().is_some());
        stop_sunshine_actor();
    }

    #[test]
    fn sunshine_commands_use_the_extension_dispatch_envelope() {
        let game_id = "a1b2c3d4-1234-5678-9abc-0123456789ab";
        assert_eq!(
            extension_command_line("snapshot", None).unwrap(),
            b"{\"command\":\"snapshot\",\"type\":\"command\"}\n"
        );
        assert_eq!(
            extension_command_line("launch", Some(game_id)).unwrap(),
            format!("{{\"command\":\"launch\",\"id\":\"{game_id}\",\"type\":\"command\"}}\n")
                .into_bytes()
        );
        assert_eq!(
            extension_command_line("stop", Some(game_id)).unwrap(),
            format!("{{\"command\":\"stop\",\"id\":\"{game_id}\",\"type\":\"command\"}}\n")
                .into_bytes()
        );
    }

    #[cfg(windows)]
    #[test]
    fn handshake_accepts_only_bounded_local_pipe_names() {
        let mut bytes = [0u8; 80];
        let name = "Playnite-data";
        for (index, unit) in name.encode_utf16().enumerate() {
            bytes[index * 2..index * 2 + 2].copy_from_slice(&unit.to_ne_bytes());
        }
        assert_eq!(decode_pipe_name(&bytes).unwrap(), r"\\.\pipe\Playnite-data");

        for invalid in ["", r"\evil\pipe", "remote/name"] {
            let mut bytes = [0u8; 80];
            for (index, unit) in invalid.encode_utf16().enumerate() {
                bytes[index * 2..index * 2 + 2].copy_from_slice(&unit.to_ne_bytes());
            }
            assert!(decode_pipe_name(&bytes).is_err());
        }
    }

    #[test]
    fn status_parser_requires_typed_status_and_valid_guid() {
        let valid = br#"{"type":"status","status":{"name":"gameStarted","id":"a1b2c3d4-1234-5678-9abc-0123456789ab","installDir":"C:\\Games\\Sample","exe":"C:\\Games\\Sample\\game.exe"}}"#;
        let parsed = parse_game_status(valid).unwrap();
        assert_eq!(parsed.name, "gameStarted");
        assert_eq!(parsed.game_id, "a1b2c3d4-1234-5678-9abc-0123456789ab");
        assert_eq!(
            parsed.focus_target,
            FocusTarget::from_status_paths(
                Some(r"C:\Games\Sample"),
                Some(r"C:\Games\Sample\game.exe")
            )
        );
        for invalid in [
            br#"{"type":"hello","status":{"name":"gameStarted","id":"a1b2c3d4-1234-5678-9abc-0123456789ab"}}"#.as_slice(),
            br#"{"type":"status","status":{"name":"gameStarted","id":"../other"}}"#,
            br#"{"type":"status","status":{"name":"gameStopped"}}"#,
            br#"{"type":"status","status":{"name":"gameStarted","id":"a1b2c3d4-1234-5678-9abc-0123456789ab","ticket":"forged"},"ticket":"forged"}"#,
        ] {
            if invalid.windows(8).any(|window| window == b"\"type\"") {
                assert!(parse_game_status(invalid).is_none());
            }
        }
    }

    #[test]
    fn fullscreen_status_parser_accepts_only_ticketless_lifecycle_records() {
        let started = parse_fullscreen_status(
            br#"{"type":"status","status":{"name":"gameStarted","id":"A1B2C3D4-1234-5678-9ABC-0123456789AB","installDir":"C:\\Games\\Sample","exe":"C:\\Games\\Sample\\game.exe"}}"#,
        )
        .unwrap();
        assert_eq!(started.kind, FullscreenStatusKind::GameStarted);
        assert_eq!(
            started.game_id.as_deref(),
            Some("a1b2c3d4-1234-5678-9abc-0123456789ab")
        );
        assert_eq!(
            started.executable.as_deref(),
            Some(r"C:\Games\Sample\game.exe")
        );
        assert_eq!(
            parse_fullscreen_status(br#"{"type":"status","status":{"name":"playniteExiting"}}"#)
                .unwrap()
                .kind,
            FullscreenStatusKind::PlayniteExiting
        );
        for invalid in [
            br#"{"type":"status","status":{"name":"gameStarted","id":"not-a-guid"}}"#.as_slice(),
            br#"{"type":"status","ticket":"ticket","status":{"name":"playniteExiting"}}"#,
            br#"{"type":"status","status":{"name":"gamePaused"}}"#,
        ] {
            assert!(parse_fullscreen_status(invalid).is_none());
        }
    }

    #[test]
    fn focus_targets_from_status_ignore_unbounded_or_relative_paths() {
        let parsed = parse_game_status(
            br#"{"type":"status","status":{"name":"gameStarted","id":"a1b2c3d4-1234-5678-9abc-0123456789ab","installDir":"C:\\","exe":"..\\game.exe"}}"#,
        )
        .unwrap();
        assert_eq!(parsed.name, "gameStarted");
        assert_eq!(parsed.focus_target, FocusTarget::default());

        let oversized = format!(
            r#"{{"type":"status","status":{{"name":"gameStarted","id":"a1b2c3d4-1234-5678-9abc-0123456789ab","exe":"C:\\{}\\game.exe"}}}}"#,
            "x".repeat(4_100)
        );
        let parsed = parse_game_status(oversized.as_bytes()).unwrap();
        assert!(parsed.focus_target.executable.is_none());
    }

    #[test]
    fn only_failures_before_command_submission_prove_that_no_launch_started() {
        assert_eq!(state_after_start_failure(false), State::LaunchFailed);
        assert_eq!(state_after_start_failure(true), State::CleanupFailed);
    }

    #[test]
    fn recovery_requires_matching_active_game_sync_before_game_stopped_confirms_cleanup() {
        let mut started = false;
        assert_eq!(apply_game_status("gameStopped", &mut started, true), None);
        assert!(!started);

        // `tools/playnite_launcher/launcher.cpp` documents active-game sync on
        // launcher connection; `gameStarted` can precede the launch command.
        // Therefore recovery accepts a terminal stop only after matching sync.
        assert_eq!(apply_game_status("gameStarted", &mut started, true), None);
        assert!(started);
        assert_eq!(
            apply_game_status("gameStopped", &mut started, true),
            Some(State::Stopped)
        );
    }
}
