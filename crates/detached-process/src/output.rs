//! Provider-owned output leases keep one opened file identity across commands.
use super::{Registry, Shared};
use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

const MAX_PATH_BYTES: usize = 4096;

struct Lease {
    shared: Arc<Shared>,
    token: u64,
}

impl Drop for Lease {
    fn drop(&mut self) {
        let mut state = self.shared.lock();
        state.sinks.remove(&self.token);
        state.owners -= 1;
        self.shared.wake.notify_one();
    }
}

/// Opaque shared output lease. Closing the last clone releases its capacity;
/// already attached commands keep their own duplicates of the original file.
#[derive(Clone)]
pub struct Sink {
    lease: Arc<Lease>,
}

impl Registry {
    /// Reserve a sink before opening a bounded absolute path. The opened object
    /// must be a regular file. An unsuccessful open returns the reserved slot.
    pub fn open_output(&self, path: &Path) -> io::Result<Sink> {
        if !path.is_absolute()
            || path.as_os_str().len() > MAX_PATH_BYTES
            || path
                .to_str()
                .is_none_or(|path| path.chars().any(char::is_control))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid output path",
            ));
        }
        let token = {
            let mut state = self.shared.lock();
            if state.sinks.len() >= self.shared.sink_capacity {
                return Err(io::Error::other("output sink capacity exhausted"));
            }
            let token = state.next_sink;
            state.next_sink = token
                .checked_add(1)
                .ok_or_else(|| io::Error::other("output sink tokens exhausted"))?;
            state.sinks.insert(token, None);
            token
        };
        match open_regular_output(path) {
            Ok(file) => {
                let mut state = self.shared.lock();
                state.sinks.insert(token, Some(file));
                state.owners += 1;
                drop(state);
                Ok(Sink {
                    lease: Arc::new(Lease {
                        shared: Arc::clone(&self.shared),
                        token,
                    }),
                })
            }
            Err(error) => {
                self.shared.lock().sinks.remove(&token);
                Err(error)
            }
        }
    }
}

impl Sink {
    /// Attach selected output streams to duplicates of the pinned file. Both
    /// duplicates are acquired before mutating the command. A queued spawn must
    /// retain this lease until attachment, even if its caller closes its token.
    pub fn attach(&self, command: &mut Command, stdout: bool, stderr: bool) -> io::Result<()> {
        let state = self.lease.shared.lock();
        let file = state
            .sinks
            .get(&self.lease.token)
            .and_then(Option::as_ref)
            .ok_or_else(|| io::Error::other("output sink unavailable"))?;
        let output = stdout.then(|| file.try_clone()).transpose()?;
        let error = stderr.then(|| file.try_clone()).transpose()?;
        drop(state);
        if let Some(output) = output {
            command.stdout(output);
        }
        if let Some(error) = error {
            command.stderr(error);
        }
        Ok(())
    }
}

fn open_regular_output(path: &Path) -> io::Result<File> {
    if let Ok(metadata) = std::fs::metadata(path)
        && !metadata.is_file()
    {
        return Err(non_regular());
    }
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    // A regular file may be replaced by a FIFO between validation and open.
    // Do not block the provider's worker if that happens.
    #[cfg(unix)]
    options.custom_flags(libc::O_NONBLOCK);
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(non_regular());
    }
    Ok(file)
}

fn non_regular() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, "output must be a regular file")
}
