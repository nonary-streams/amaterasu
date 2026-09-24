//! Authenticated, bounded client for the Playnite extension named-pipe protocol.
//!
//! The extension pipe name alone is not trusted. Each connection verifies the
//! kernel-reported server PID and image against current Playnite discovery, and
//! the negotiated data pipe must belong to the same process. This does not
//! protect against a compromised Playnite process on the same host.

use crate::discovery::ProcessImage;
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
        OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
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
    // SAFETY: process is owned by this helper.
    unsafe { CloseHandle(process) };
    if queried == 0 {
        return Err("Playnite pipe server image could not be read".into());
    }
    let actual_path = String::from_utf16_lossy(&path[..length as usize]);
    if !actual_path.eq_ignore_ascii_case(&expected_image.path) {
        return Err("Playnite pipe server path changed after process discovery".into());
    }
    Ok(process_id)
}

#[cfg(windows)]
fn connect_role(
    expected_processes: &[ProcessImage],
    role: &str,
    game_id: Option<&str>,
) -> Result<Pipe, String> {
    use std::thread;

    if expected_processes.is_empty() {
        return Err("Playnite process discovery returned no trusted process IDs".into());
    }
    let connect_deadline = Instant::now() + CONNECT_TIMEOUT;
    let control = Pipe::open(CONTROL_PIPE, connect_deadline)?;
    let control_server = validated_server_process(&control, expected_processes)?;
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
    if validated_server_process(&data_pipe, expected_processes)? != control_server {
        return Err(
            "Playnite data pipe server did not match the authenticated control server".into(),
        );
    }
    let mut hello = serde_json::json!({"type":"hello","role":role,"pid":std::process::id()});
    if let Some(game_id) = game_id {
        hello["gameId"] = game_id.into();
    }
    let mut line = serde_json::to_vec(&hello).map_err(|_| "Playnite hello serialization failed")?;
    line.push(b'\n');
    data_pipe.write_all(&line)?;
    Ok(data_pipe)
}

#[cfg(windows)]
pub fn request_snapshot(expected_processes: &[ProcessImage]) -> Result<Snapshot, String> {
    let data_pipe = connect_role(expected_processes, "sunshine", None)?;
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
    stop_requests: Receiver<u64>,
    events: Sender<super::launch::State>,
    recovery: bool,
) {
    use serde_json::json;

    let fail = |state| {
        let _ = events.send(state);
    };
    let mut line = Vec::new();
    let mut buffer = [0u8; 4096];
    let mut received = 0usize;
    let mut started = false;
    let mut stopping = recovery;
    let mut stop_deadline = None;
    let launch_deadline = Instant::now() + LAUNCH_START_TIMEOUT;

    let pipe = match connect_role(expected_processes, "launcher", Some(game_id)) {
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
        let timeout_seconds = match stop_requests.try_recv() {
            Ok(timeout_seconds) => timeout_seconds,
            Err(_) => {
                fail(super::launch::State::CleanupFailed);
                return;
            }
        };
        if send_command(expected_processes, &json!({"command":"stop","id":game_id})).is_err() {
            fail(super::launch::State::CleanupFailed);
            return;
        }
        stop_deadline = Some(Instant::now() + Duration::from_secs(timeout_seconds));
        let _ = events.send(super::launch::State::Stopping);
    } else if send_command(
        expected_processes,
        &json!({"command":"launch","id":game_id}),
    )
    .is_err()
    {
        fail(state_after_start_failure(true));
        return;
    }

    loop {
        match stop_requests.try_recv() {
            Ok(timeout_seconds) if !stopping => {
                if send_command(expected_processes, &json!({"command":"stop","id":game_id}))
                    .is_err()
                {
                    fail(super::launch::State::CleanupFailed);
                    return;
                }
                stopping = true;
                stop_deadline = Some(Instant::now() + Duration::from_secs(timeout_seconds));
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
                if let Some((name, status_game_id)) = parse_game_status(&line)
                    && status_game_id.eq_ignore_ascii_case(game_id)
                    && let Some(state) = apply_game_status(&name, &mut started, stopping)
                {
                    let _ = events.send(state);
                    if state == super::launch::State::Stopped {
                        return;
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
    _stop_requests: Receiver<u64>,
    events: Sender<super::launch::State>,
    recovery: bool,
) {
    let _ = events.send(if recovery {
        super::launch::State::CleanupFailed
    } else {
        super::launch::State::LaunchFailed
    });
}

#[cfg(windows)]
fn send_command(
    expected_processes: &[ProcessImage],
    command: &serde_json::Value,
) -> Result<(), String> {
    let pipe = connect_role(expected_processes, "sunshine", None)?;
    let mut line =
        serde_json::to_vec(command).map_err(|_| "Playnite command serialization failed")?;
    line.push(b'\n');
    pipe.write_all(&line)
}

fn parse_game_status(line: &[u8]) -> Option<(String, String)> {
    let message: serde_json::Value = serde_json::from_slice(line).ok()?;
    if message["type"] != "status" {
        return None;
    }
    if message.get("ticket").is_some() || message.pointer("/status/ticket").is_some() {
        return None;
    }
    let name = message.pointer("/status/name")?.as_str()?;
    let game_id = message.pointer("/status/id")?.as_str()?;
    if !valid_playnite_id(game_id) {
        return None;
    }
    Some((name.into(), game_id.into()))
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
    use super::{apply_game_status, parse_game_status, state_after_start_failure};
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
        let valid = br#"{"type":"status","status":{"name":"gameStarted","id":"a1b2c3d4-1234-5678-9abc-0123456789ab"}}"#;
        assert_eq!(
            parse_game_status(valid),
            Some((
                "gameStarted".into(),
                "a1b2c3d4-1234-5678-9abc-0123456789ab".into()
            ))
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
