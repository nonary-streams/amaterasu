//! Polling process ownership shared by the Unix helper image implementations.
use super::*;
use std::{
    fs::File,
    io::{Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::process::CommandExt,
    },
    process::{Child, Command, Stdio},
};

#[cfg(any(not(target_os = "linux"), test))]
#[path = "unix/fifo.rs"]
pub(super) mod fifo;

#[cfg(target_os = "linux")]
pub(super) fn spawn(path: &Path, request: &[u8], arguments: &[String]) -> io::Result<Process> {
    spawn_with_pipes(path, request, arguments, pipe()?, pipe()?)
}

#[cfg(any(not(target_os = "linux"), test))]
pub(super) fn spawn_with_fifos(
    path: &Path,
    request: &[u8],
    arguments: &[String],
    directory: &File,
) -> io::Result<Process> {
    spawn_with_pipes(
        path,
        request,
        arguments,
        fifo::pair(directory)?,
        fifo::pair(directory)?,
    )
}

fn spawn_with_pipes(
    path: &Path,
    request: &[u8],
    arguments: &[String],
    input_pipe: (File, File),
    output_pipe: (File, File),
) -> io::Result<Process> {
    let (input, mut request_pipe) = input_pipe;
    set_nonblocking(&request_pipe)?;
    // The complete request fits the portable minimum PIPE_BUF, so this empty
    // nonblocking pipe is filled before any helper can consume it.
    request_pipe.write_all(request)?;
    drop(request_pipe);
    let (output, child_output) = output_pipe;
    set_nonblocking(&output)?;
    set_blocking(&input)?;
    set_blocking(&child_output)?;
    let child = Command::new(path)
        .arg0("amaterasu-network-resolver")
        .args(["--parent-pid", &std::process::id().to_string()])
        .args(arguments)
        .process_group(0)
        .stdin(Stdio::from(input))
        .stdout(Stdio::from(child_output))
        .stderr(Stdio::null())
        .spawn()?;
    Ok(Process {
        child,
        output,
        exit: None,
    })
}

pub(super) struct Process {
    child: Child,
    output: File,
    exit: Option<ExitStatus>,
}

impl Process {
    pub(super) fn try_read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        self.output.read(output)
    }

    pub(super) fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        if self.exit.is_some() {
            return Ok(self.exit);
        }
        if !self.observe_owned_child()? {
            return Ok(None);
        }
        // The unreaped group leader still reserves its identity when the final
        // containment signal is delivered. Reap only after this signal.
        self.signal_group()?;
        self.exit = self.child.try_wait()?;
        Ok(self.exit)
    }

    pub(super) fn kill(&mut self) -> io::Result<()> {
        if self.exit.is_some() {
            return Ok(());
        }
        self.observe_owned_child()?;
        self.signal_group()
    }

    fn observe_owned_child(&self) -> io::Result<bool> {
        // SAFETY: zero initializes the output record; waitid with WNOHANG and
        // WNOWAIT observes only this child without blocking or reaping it.
        let mut information: libc::siginfo_t = unsafe { std::mem::zeroed() };
        if unsafe {
            libc::waitid(
                libc::P_PID,
                self.child.id(),
                &mut information,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        } != 0
        {
            // In particular ECHILD does not authorize a numeric group signal.
            return Err(io::Error::last_os_error());
        }
        #[cfg(target_os = "linux")]
        let child = unsafe { information.si_pid() };
        #[cfg(not(target_os = "linux"))]
        let child = information.si_pid;
        Ok(child != 0)
    }

    fn signal_group(&self) -> io::Result<()> {
        let group = libc::pid_t::try_from(self.child.id())
            .map_err(|_| io::Error::other("resolver process group is invalid"))?;
        // SAFETY: process_group(0) created this exact group and waitid just
        // confirmed unreaped child ownership. Never signal after reaping.
        if unsafe { libc::kill(-group, libc::SIGKILL) } != 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(error);
            }
        }
        Ok(())
    }
}

#[cfg(target_os = "linux")]
fn pipe() -> io::Result<(File, File)> {
    let mut descriptors = [-1; 2];
    let result = unsafe { libc::pipe2(descriptors.as_mut_ptr(), libc::O_CLOEXEC) };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful pipe/pipe2 transferred two distinct descriptors.
    let pipe = unsafe {
        (
            File::from_raw_fd(descriptors[0]),
            File::from_raw_fd(descriptors[1]),
        )
    };
    Ok(pipe)
}

fn set_blocking(file: &File) -> io::Result<()> {
    // SAFETY: this owned endpoint has its own file description. Clearing its IO
    // mode leaves its atomic close-on-exec descriptor flag unchanged.
    let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
    if flags < 0
        || unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETFL, flags & !libc::O_NONBLOCK) } < 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn set_nonblocking(file: &File) -> io::Result<()> {
    // SAFETY: fcntl operates on this owned descriptor without moving it.
    let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
    if flags < 0
        || unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
