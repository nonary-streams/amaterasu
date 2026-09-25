//! Authenticated, bounded client for the Playnite extension named-pipe protocol.
//!
//! The extension pipe name alone is not trusted. Each connection verifies the
//! kernel-reported server PID and image against current Playnite discovery, and
//! the negotiated data pipe must belong to the same process. This does not
//! protect against a compromised Playnite process on the same host.

use crate::discovery::ProcessImage;
use crate::focus::{FocusPolicy, FocusRetry, FocusTarget};
use crate::launch::{StopRequest, installation_roots, refresh_processes_for_install};
use crate::snapshot::{Snapshot, SnapshotAssembler, valid_playnite_id};
use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

const CONTROL_PIPE: &str = r"\\.\pipe\Sunshine.PlayniteExtension";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(8);
const SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(30);
const LAUNCH_START_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_TOTAL_BYTES: usize = 64 * 1024 * 1024;
const MAX_STATUS_LINE_BYTES: usize = 8 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
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
    let mut line = serde_json::to_vec(&hello).map_err(|_| "Playnite hello serialization failed")?;
    line.push(b'\n');
    data_pipe.write_all(&line)?;
    Ok((data_pipe, control_server))
}

#[cfg(windows)]
pub fn request_snapshot(expected_processes: &[ProcessImage]) -> Result<Snapshot, String> {
    let (data_pipe, _) = connect_role(expected_processes, "sunshine", None, None)?;
    data_pipe.write_all(b"{\"command\":\"snapshot\"}\n")?;

    let mut assembler = SnapshotAssembler::default();
    let mut line = Vec::new();
    let mut received = 0usize;
    let deadline = Instant::now() + SNAPSHOT_TIMEOUT;
    let mut buffer = [0u8; 8192];
    while Instant::now() < deadline && received < MAX_TOTAL_BYTES {
        let available = data_pipe.available()? as usize;
        if available == 0 {
            thread::sleep(Duration::from_millis(10));
            continue;
        }
        let read_length = available.min(buffer.len());
        let count = data_pipe.read_some(&mut buffer[..read_length])?;
        received = received.saturating_add(count);
        for byte in &buffer[..count] {
            if *byte == b'\n' {
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                if let Some(result) = assembler.receive_line(&line) {
                    return result.map_err(str::to_owned);
                }
                line.clear();
            } else {
                line.push(*byte);
                if line.len() > crate::snapshot::MAX_LINE_BYTES {
                    return Err("Playnite extension message exceeded the line limit".into());
                }
            }
        }
    }
    Err("Playnite extension snapshot was incomplete or exceeded its byte limit".into())
}

#[cfg(not(windows))]
pub fn request_snapshot(_expected_processes: &[ProcessImage]) -> Result<Snapshot, String> {
    Err("Playnite integration is available only on Windows".into())
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
    use serde_json::json;

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

    let (mut pipe, _) = match connect_role(&expected_processes, "launcher", Some(game_id), None) {
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
            (pipe, _) = match connect_role(&expected_processes, "launcher", Some(game_id), None) {
                Ok(pipe) => pipe,
                Err(_) => {
                    fail(super::launch::State::CleanupFailed);
                    return;
                }
            };
        }
        if send_command(&expected_processes, &json!({"command":"stop","id":game_id})).is_err() {
            fail(super::launch::State::CleanupFailed);
            return;
        }
        stop_deadline = Some(Instant::now() + Duration::from_secs(request.timeout_seconds));
        let _ = events.send(super::launch::State::Stopping);
    } else if send_command(
        &expected_processes,
        &json!({"command":"launch","id":game_id}),
    )
    .is_err()
    {
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
                    (pipe, _) =
                        match connect_role(&expected_processes, "launcher", Some(game_id), None) {
                            Ok(pipe) => pipe,
                            Err(_) => {
                                fail(super::launch::State::CleanupFailed);
                                return;
                            }
                        };
                }
                if send_command(&expected_processes, &json!({"command":"stop","id":game_id}))
                    .is_err()
                {
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
            match connect_role(&expected_processes, "launcher", None, Some("fullscreen")) {
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
        for byte in &buffer[..count] {
            if *byte == b'\n' {
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                if let (Some(status), Some(extension_process)) =
                    (parse_fullscreen_status(&line), extension_process.as_ref())
                {
                    let _ = events.send(FullscreenWatchEvent::Status {
                        status,
                        extension_process: extension_process.clone(),
                    });
                }
                line.clear();
            } else {
                line.push(*byte);
                if line.len() > MAX_STATUS_LINE_BYTES {
                    return;
                }
            }
        }
    }
}

#[cfg(windows)]
fn send_command(
    expected_processes: &[ProcessImage],
    command: &serde_json::Value,
) -> Result<(), String> {
    let (pipe, _) = connect_role(expected_processes, "sunshine", None, None)?;
    let mut line =
        serde_json::to_vec(command).map_err(|_| "Playnite command serialization failed")?;
    line.push(b'\n');
    pipe.write_all(&line)
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct GameStatus {
    name: String,
    game_id: String,
    focus_target: FocusTarget,
}

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
    use super::{apply_game_status, parse_game_status, state_after_start_failure};
    use crate::focus::FocusTarget;
    use crate::launch::State;

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
