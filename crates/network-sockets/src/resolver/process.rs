//! Exact helper execution and nonblocking parent-side process ownership.
//!
//! The executable is embedded in the signed provider. No resolver thread runs in
//! this DLL: even an uncancellable NSS call stays inside the owned child process.
use std::{io, path::Path, process::ExitStatus, sync::Arc};

const HELPER_BYTES: &[u8] = include_bytes!(env!("AMATERASU_NETWORK_RESOLVER_HELPER_PATH"));
const MAX_REQUEST_BYTES: usize = 512;
const MAX_HELPER_BYTES: usize = 64 * 1024 * 1024;

pub(super) struct HelperExecutable(Arc<platform::Executable>);

impl HelperExecutable {
    /// `private_state_root` comes from the trusted host, never module settings.
    /// Linux executes sealed memory only. Windows retains verified, read-locked
    /// extraction bytes and every ancestor until the final child has retired.
    pub(super) fn new(private_state_root: Option<&Path>) -> io::Result<Self> {
        Self::from_bytes(HELPER_BYTES, private_state_root)
    }

    fn from_bytes(bytes: &[u8], private_state_root: Option<&Path>) -> io::Result<Self> {
        if bytes.is_empty() || bytes.len() > MAX_HELPER_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "embedded resolver helper has an invalid size",
            ));
        }
        platform::Executable::new(bytes, private_state_root)
            .map(Arc::new)
            .map(Self)
    }

    #[cfg(test)]
    pub(super) fn for_test_fixture(bytes: &[u8], arguments: &[&str]) -> io::Result<Self> {
        let mut executable = platform::Executable::new(bytes, None)?;
        executable.set_test_arguments(arguments);
        Ok(Self(Arc::new(executable)))
    }

    pub(super) fn spawn(&self, request: &[u8]) -> io::Result<ResolverChild> {
        if request.is_empty() || request.len() > MAX_REQUEST_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "resolver helper request exceeds its pipe bound",
            ));
        }
        Ok(ResolverChild {
            process: self.0.spawn(request)?,
            _executable: self.0.clone(),
        })
    }
}

pub(super) struct ResolverChild {
    process: platform::Process,
    _executable: Arc<platform::Executable>,
}

impl ResolverChild {
    /// Returns WouldBlock when no bytes are available, and zero on pipe EOF.
    pub(super) fn try_read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        self.process.try_read(output)
    }

    /// Observes actual exit/reaping, never a successful termination request.
    pub(super) fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.process.try_wait()
    }

    pub(super) fn kill(&mut self) -> io::Result<()> {
        self.process.kill()
    }
}

impl Drop for ResolverChild {
    fn drop(&mut self) {
        // Final containment only. The resolver registry retains live children
        // until try_wait proves retirement; destruction creates no new budget.
        let _ = self.process.kill();
        let _ = self.process.try_wait();
    }
}

#[cfg(target_os = "linux")]
#[path = "process/linux.rs"]
mod platform;

#[cfg(unix)]
#[path = "process/unix.rs"]
mod unix;

#[cfg(all(test, target_os = "linux"))]
#[allow(dead_code)]
#[path = "process/macos.rs"]
mod private_unix_test;

#[cfg(all(unix, not(target_os = "linux")))]
#[path = "process/macos.rs"]
mod platform;

#[cfg(windows)]
#[path = "process/windows.rs"]
mod platform;

#[cfg(not(any(unix, windows)))]
mod platform {
    use super::*;
    pub(super) struct Executable;
    impl Executable {
        pub(super) fn new(_bytes: &[u8], _root: Option<&Path>) -> io::Result<Self> {
            Err(io::ErrorKind::Unsupported.into())
        }
        pub(super) fn spawn(&self, _request: &[u8]) -> io::Result<Process> {
            Err(io::ErrorKind::Unsupported.into())
        }
        #[cfg(test)]
        pub(super) fn set_test_arguments(&mut self, _arguments: &[&str]) {}
    }
    pub(super) struct Process;
    impl Process {
        pub(super) fn try_read(&mut self, _output: &mut [u8]) -> io::Result<usize> {
            Err(io::ErrorKind::Unsupported.into())
        }
        pub(super) fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
            Err(io::ErrorKind::Unsupported.into())
        }
        pub(super) fn kill(&mut self) -> io::Result<()> {
            Err(io::ErrorKind::Unsupported.into())
        }
    }
}

#[cfg(test)]
mod tests;
