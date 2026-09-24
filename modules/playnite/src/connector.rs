//! Bounded client for Playnite's extension snapshot named-pipe handshake.
//!
//! The pinned extension grants broad local pipe access, so the fixed pipe name
//! is not treated as peer authentication. The kernel-reported server PID and
//! queried image path must match a currently discovered Playnite process. This
//! does not protect against a compromised Playnite process on the same host.

use crate::discovery::ProcessImage;
use crate::snapshot::{Snapshot, SnapshotAssembler};
#[cfg(windows)]
use std::time::Duration;

#[cfg(windows)]
const CONTROL_PIPE: &str = r"\\.\pipe\Sunshine.PlayniteExtension";
#[cfg(windows)]
const CONNECT_TIMEOUT: Duration = Duration::from_secs(8);
#[cfg(windows)]
const SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(30);
#[cfg(windows)]
const MAX_TOTAL_BYTES: usize = 64 * 1024 * 1024;

#[cfg(windows)]
pub fn request_snapshot(expected_processes: &[ProcessImage]) -> Result<Snapshot, String> {
    use std::thread;
    use std::time::Instant;
    use windows_sys::Win32::Foundation::{
        CloseHandle, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_ATTRIBUTE_NORMAL, OPEN_EXISTING, ReadFile, WriteFile,
    };
    use windows_sys::Win32::System::Pipes::{
        GetNamedPipeServerProcessId, PeekNamedPipe, WaitNamedPipeW,
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
    };

    struct Pipe(HANDLE);

    impl Pipe {
        fn open(path: &str, deadline: Instant) -> Result<Self, String> {
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

    fn validated_server_process(pipe: &Pipe, expected: &[ProcessImage]) -> Result<u32, String> {
        let mut process_id = 0;
        // SAFETY: the owned pipe handle and writable process ID output are valid.
        if unsafe { GetNamedPipeServerProcessId(pipe.0, &mut process_id) } == 0 {
            return Err("Playnite pipe server identity was unavailable".into());
        }
        let Some(expected_image) = expected.iter().find(|image| image.process_id == process_id)
        else {
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
        let queried =
            unsafe { QueryFullProcessImageNameW(process, 0, path.as_mut_ptr(), &mut length) };
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

    impl Drop for Pipe {
        fn drop(&mut self) {
            // SAFETY: this wrapper uniquely owns the handle.
            unsafe { CloseHandle(self.0) };
        }
    }

    let connect_deadline = Instant::now() + CONNECT_TIMEOUT;
    if expected_processes.is_empty() {
        return Err("Playnite process discovery returned no trusted process IDs".into());
    }
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
    let process_id = std::process::id();
    data_pipe.write_all(
        format!("{{\"type\":\"hello\",\"role\":\"sunshine\",\"pid\":{process_id}}}\n").as_bytes(),
    )?;
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
        let requested = available.min(buffer.len());
        let count = data_pipe.read_some(&mut buffer[..requested])?;
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
        Err("Playnite handshake pipe name was not a local named pipe".into())
    }
}

#[cfg(test)]
mod tests {
    #[cfg(windows)]
    use super::decode_pipe_name;

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
}
