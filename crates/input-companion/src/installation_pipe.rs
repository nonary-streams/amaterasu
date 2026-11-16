//! Reuse the authenticated, cancellable local pipe boundary for installation.
//! Larger reviews are fragmented without increasing input-protocol frame limits.
use crate::{
    ProcessIdentity,
    windows_identity::{inspect_process, pipe_peer},
    windows_pipe::{Pipe, PipeKind},
};
use std::{
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};
pub struct InstallationPipe(Pipe);
pub fn installation_identity() -> Result<String, String> {
    inspect_process(std::process::id())
        .map(|peer| peer.principal)
        .map_err(|error| format!("{error:?}"))
}
impl InstallationPipe {
    pub fn connect() -> Result<Self, String> {
        Pipe::connect(PipeKind::Installation)
            .map(Self)
            .map_err(|e| format!("{e:?}"))
    }
    pub fn listen(first: bool) -> Result<Self, String> {
        Pipe::listen(PipeKind::Installation, first)
            .map(Self)
            .map_err(|e| format!("{e:?}"))
    }
    pub fn accept(&mut self, stop: &AtomicBool) -> Result<bool, String> {
        self.0
            .accept_until(Instant::now() + Duration::from_millis(250), stop)
            .map(|connected| connected.unwrap_or(false))
            .map_err(|e| format!("{e:?}"))
    }
    pub fn peer(&self) -> Result<ProcessIdentity, String> {
        pipe_peer(self.0.raw()).map_err(|e| format!("{e:?}"))
    }
    pub fn read(&mut self) -> Result<Option<Vec<u8>>, String> {
        let deadline = Instant::now() + Duration::from_secs(30);
        let Some(header) = self.0.read_line(deadline).map_err(|e| format!("{e:?}"))? else {
            return Ok(None);
        };
        let length = std::str::from_utf8(&header)
            .map_err(|e| e.to_string())?
            .parse::<usize>()
            .map_err(|e| e.to_string())?;
        if length > 256 * 1024 {
            return Err("installation_frame_too_large".into());
        }
        let mut bytes = Vec::with_capacity(length);
        while bytes.len() < length {
            let chunk = self
                .0
                .read_line(deadline)
                .map_err(|e| format!("{e:?}"))?
                .ok_or("installation_frame_truncated")?;
            if chunk.is_empty() || bytes.len() + chunk.len() > length {
                return Err("installation_frame_invalid".into());
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(Some(bytes))
    }
    pub fn write(&self, bytes: &[u8]) -> Result<(), String> {
        if bytes.len() > 256 * 1024 || bytes.contains(&b'\n') {
            return Err("installation_frame_too_large".into());
        }
        let deadline = Instant::now() + Duration::from_secs(30);
        self.0
            .write_line_before(bytes.len().to_string().as_bytes(), deadline)
            .map_err(|e| format!("{e:?}"))?;
        for chunk in bytes.chunks(3000) {
            self.0
                .write_line_before(chunk, deadline)
                .map_err(|e| format!("{e:?}"))?;
        }
        Ok(())
    }
}
