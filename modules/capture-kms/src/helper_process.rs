//! Ownership and verified startup of the provider's embedded readback helper.
//!
//! A receipt is reserved before spawning. It remains in the backend registry
//! until the kernel has reported the exact child reaped, including failed
//! startup and protocol errors. Killing a process is not retirement evidence.
use std::{
    fmt,
    io::{self, Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
        unix::net::UnixStream,
    },
    sync::{
        Arc, Mutex, TryLockError,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

const MAX_CHILDREN: usize = 8;
const FD_FLOOR: RawFd = 10;
const IO_TIMEOUT: Duration = Duration::from_secs(3);
const SHUTDOWN_GRACE: Duration = Duration::from_millis(250);

#[derive(Debug)]
struct LaunchError {
    stage: &'static str,
    source: io::Error,
}

impl fmt::Display for LaunchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "KMS helper {} failed: {}",
            self.stage, self.source
        )
    }
}

impl std::error::Error for LaunchError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

fn launch_error(stage: &'static str, source: io::Error) -> io::Error {
    io::Error::new(source.kind(), LaunchError { stage, source })
}

#[derive(Default)]
struct ChildState {
    child: Option<OwnedFd>,
    starting: bool,
    kill_sent: bool,
    retired: bool,
    identity_lost: bool,
    error: Option<String>,
    shutdown_deadline: Option<Instant>,
}

pub(crate) struct Receipt(Mutex<ChildState>, AtomicBool);

impl Receipt {
    fn new() -> Self {
        Self(
            Mutex::new(ChildState {
                starting: true,
                ..ChildState::default()
            }),
            AtomicBool::new(false),
        )
    }

    fn adopt(&self, child: OwnedFd) {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        state.child = Some(child);
        state.starting = false;
    }

    fn spawn_failed(&self) {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        state.starting = false;
        state.retired = true;
    }

    pub(crate) fn stop(&self) {
        self.1.store(true, Ordering::Release);
    }

    #[cfg(test)]
    pub(crate) fn is_stopping(&self) -> bool {
        self.1.load(Ordering::Acquire)
    }

    /// A nonblocking poll: errors and lost PID ownership never become success.
    pub(crate) fn poll(&self) -> io::Result<bool> {
        let mut state = match self.0.try_lock() {
            Ok(state) => state,
            Err(TryLockError::WouldBlock) => return Ok(false),
            Err(TryLockError::Poisoned(_)) => {
                return Err(io::Error::other("helper state poisoned"));
            }
        };
        if state.identity_lost {
            return Err(io::Error::other("helper process identity lost"));
        }
        if state.retired {
            return state
                .error
                .as_ref()
                .map_or(Ok(true), |e| Err(io::Error::other(e.clone())));
        }
        if state.starting {
            return Ok(false);
        }
        let Some(child) = state.child.as_mut() else {
            return Err(io::Error::other("helper process owner missing"));
        };
        match poll_pidfd(child.as_raw_fd(), false) {
            Ok(true) => {
                state.child = None;
                state.retired = true;
                return state
                    .error
                    .as_ref()
                    .map_or(Ok(true), |e| Err(io::Error::other(e.clone())));
            }
            Ok(false) => {}
            Err(error) if error.kind() == io::ErrorKind::Interrupted => return Ok(false),
            Err(error) => {
                if error.raw_os_error() == Some(libc::ECHILD) {
                    state.identity_lost = true;
                }
                state
                    .error
                    .get_or_insert_with(|| format!("helper wait failed: {error}"));
                return Err(error);
            }
        }
        if self.1.load(Ordering::Acquire) && !state.kill_sent {
            let deadline = *state
                .shutdown_deadline
                .get_or_insert_with(|| Instant::now() + SHUTDOWN_GRACE);
            if Instant::now() < deadline {
                return Ok(false);
            }
            match signal_pidfd(state.child.as_ref().expect("child retained").as_raw_fd()) {
                Ok(()) => state.kill_sent = true,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => return Ok(false),
                Err(error) if error.raw_os_error() == Some(libc::ESRCH) => {
                    // The child may have exited between waitid and signal.
                    // Only a subsequent waitid can establish retirement.
                }
                Err(error) => {
                    state
                        .error
                        .get_or_insert_with(|| format!("helper termination failed: {error}"));
                    return Err(error);
                }
            }
        }
        Ok(false)
    }
}

impl Drop for Receipt {
    fn drop(&mut self) {
        let state = self.0.get_mut().unwrap_or_else(|e| e.into_inner());
        if state.identity_lost {
            return; // A reusable PID must never be signalled after ECHILD.
        }
        if let Some(child) = state.child.as_ref() {
            let _ = signal_pidfd(child.as_raw_fd());
            while let Err(error) = poll_pidfd(child.as_raw_fd(), true) {
                if error.kind() != io::ErrorKind::Interrupted {
                    break;
                }
            }
        }
    }
}

#[derive(Clone, Default)]
pub(crate) struct Registry(Arc<Mutex<Vec<Arc<Receipt>>>>);

impl Registry {
    fn reserve(&self) -> io::Result<Arc<Receipt>> {
        self.poll()?;
        let mut entries = match self.0.try_lock() {
            Ok(entries) => entries,
            Err(TryLockError::WouldBlock) => {
                return Err(io::Error::from(io::ErrorKind::WouldBlock));
            }
            Err(TryLockError::Poisoned(_)) => {
                return Err(io::Error::other("helper registry poisoned"));
            }
        };
        if entries.len() >= MAX_CHILDREN {
            return Err(io::Error::other("helper retirement capacity exhausted"));
        }
        let receipt = Arc::new(Receipt::new());
        entries.push(receipt.clone());
        Ok(receipt)
    }

    pub(crate) fn poll(&self) -> io::Result<bool> {
        let mut entries = match self.0.try_lock() {
            Ok(entries) => entries,
            Err(TryLockError::WouldBlock) => return Ok(false),
            Err(TryLockError::Poisoned(_)) => {
                return Err(io::Error::other("helper registry poisoned"));
            }
        };
        let mut pending = false;
        let mut failure = None;
        entries.retain(|receipt| match receipt.poll() {
            Ok(true) => false,
            Ok(false) => {
                pending = true;
                true
            }
            Err(error) => {
                failure.get_or_insert(error);
                true
            }
        });
        if let Some(error) = failure {
            Err(error)
        } else {
            Ok(!pending)
        }
    }
}

/// The helper bytes are part of the signed native provider; never resolve an
/// executable from PATH, the current directory or mutable configuration.
pub(crate) fn spawn(
    registry: &Registry,
    executable: &[u8],
) -> io::Result<(UnixStream, Arc<Receipt>)> {
    spawn_with_fault(registry, executable, Fault::None)
}

pub(crate) fn spawn_cancellable(
    registry: &Registry,
    executable: &[u8],
    cancellation: &crate::open_task::Cancellation,
) -> io::Result<(UnixStream, Arc<Receipt>)> {
    spawn_reserved(registry, executable, Fault::None, Some(cancellation))
}

fn spawn_with_fault(
    registry: &Registry,
    executable: &[u8],
    fault: Fault,
) -> io::Result<(UnixStream, Arc<Receipt>)> {
    spawn_reserved(registry, executable, fault, None)
}

fn spawn_reserved(
    registry: &Registry,
    executable: &[u8],
    fault: Fault,
    cancellation: Option<&crate::open_task::Cancellation>,
) -> io::Result<(UnixStream, Arc<Receipt>)> {
    let receipt = registry.reserve()?;
    if let Some(cancellation) = cancellation
        && let Err(error) = cancellation.register_receipt(receipt.clone())
    {
        receipt.spawn_failed();
        return Err(error);
    }
    // Keep the creating pthread alive on unwind until any adopted child is
    // reaped; otherwise PDEATHSIG registration could race that thread exit.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        spawn_inner(executable, &receipt, fault)
    }))
    .unwrap_or_else(|_| Err(io::Error::other("KMS helper startup panicked")));
    match result {
        Ok(channel) => Ok((channel, receipt)),
        Err(error) => {
            let has_child = receipt
                .0
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .child
                .is_some();
            if has_child {
                receipt.stop();
                // The creator pthread must not return while its child may not
                // yet have armed PDEATHSIG. Keep the owner alive until exact
                // process retirement is established, even on cancellation.
                loop {
                    let _ = receipt.poll();
                    if receipt.0.try_lock().is_ok_and(|state| state.retired) {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
            } else {
                receipt.spawn_failed();
            }
            Err(error)
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // Injection variants are constructed by tests only.
enum Fault {
    None,
    Clone,
    Memfd,
    CloseRange,
    Exec,
    HoldBeforeExec,
    PanicAfterClone,
}

/// Owns the interval after clone3 and before the registry adopts the pidfd.
/// Even unwinding in that interval cannot let the creator thread exit while
/// an untracked child is still establishing its parent-death protection.
struct PendingChild(Option<OwnedFd>);

impl Drop for PendingChild {
    fn drop(&mut self) {
        let Some(child) = self.0.as_ref() else {
            return;
        };
        let _ = signal_pidfd(child.as_raw_fd());
        loop {
            if matches!(poll_pidfd(child.as_raw_fd(), true), Ok(true)) {
                break;
            }
            // Lost or unconfirmed child identity is not retirement. Retain
            // this owner and the provider code until exact reaping succeeds.
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

fn spawn_inner(executable: &[u8], receipt: &Receipt, fault: Fault) -> io::Result<UnixStream> {
    if receipt.1.load(Ordering::Acquire) {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "helper startup cancelled",
        ));
    }
    if executable.is_empty() || executable.len() > 32 * 1024 * 1024 {
        return Err(io::Error::other("invalid embedded helper size"));
    }
    // SAFETY: literal is NUL terminated and the flags request a new owned fd.
    if fault == Fault::Memfd {
        return Err(launch_error(
            "memfd_create",
            io::Error::from_raw_os_error(libc::EPERM),
        ));
    }
    let fd = unsafe {
        libc::memfd_create(
            c"amaterasu-kms-helper".as_ptr(),
            libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
        )
    };
    if fd < 0 {
        return Err(launch_error("memfd_create", io::Error::last_os_error()));
    }
    // SAFETY: successful memfd_create returned a uniquely owned descriptor.
    let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
    file.write_all(executable)
        .map_err(|error| launch_error("memfd write", error))?;
    // SAFETY: fchmod/fcntl act on our live descriptor and use scalar arguments.
    if unsafe { libc::fchmod(fd, 0o500) } != 0 {
        return Err(launch_error("memfd chmod", io::Error::last_os_error()));
    }
    if unsafe {
        libc::fcntl(
            fd,
            libc::F_ADD_SEALS,
            libc::F_SEAL_SEAL | libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_WRITE,
        )
    } != 0
    {
        return Err(launch_error("memfd seal", io::Error::last_os_error()));
    }
    let executable = duplicate(file.as_raw_fd())?;
    let (mut channel, child_channel) = UnixStream::pair()?;
    channel.set_read_timeout(Some(IO_TIMEOUT))?;
    channel.set_write_timeout(Some(IO_TIMEOUT))?;
    let child_channel = duplicate(child_channel.as_raw_fd())?;
    // The pidfd identifies this exact host process, independently of the
    // short-lived thread which creates the helper. The helper watches it and
    // terminates itself if the host exits.
    // SAFETY: pidfd_open takes scalar arguments and returns a new owned fd.
    let host_fd = unsafe { libc::syscall(libc::SYS_pidfd_open, libc::getpid(), 0) };
    if host_fd < 0 {
        return Err(launch_error("host pidfd_open", io::Error::last_os_error()));
    }
    // SAFETY: successful pidfd_open returned one uniquely owned descriptor.
    let host_fd = unsafe { OwnedFd::from_raw_fd(host_fd as RawFd) };
    let host_fd = duplicate(host_fd.as_raw_fd())?;
    let (mut errors, error_writer) = error_pipe()?;
    if receipt.1.load(Ordering::Acquire) {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "helper startup cancelled",
        ));
    }
    let mut child = PendingChild(Some(
        spawn_pidfd(
            executable.as_raw_fd(),
            child_channel.as_raw_fd(),
            host_fd.as_raw_fd(),
            error_writer.as_raw_fd(),
            fault,
        )
        .map_err(|error| launch_error("clone3", error))?,
    ));
    if fault == Fault::PanicAfterClone {
        panic!("injected panic after clone");
    }
    receipt.adopt(child.0.take().expect("new child"));
    drop(error_writer);
    await_exec(&mut errors, receipt)?;
    await_ready(&mut channel, receipt)?;
    Ok(channel)
}

fn await_ready(channel: &mut UnixStream, receipt: &Receipt) -> io::Result<()> {
    channel.set_nonblocking(true)?;
    let deadline = Instant::now() + IO_TIMEOUT;
    let mut message = [0u8; 8];
    let mut used = 0;
    while used < message.len() {
        if receipt.1.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "helper startup cancelled",
            ));
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "KMS helper readiness timed out",
            ));
        }
        match channel.read(&mut message[used..]) {
            Ok(0) => return Err(io::Error::other("KMS helper exited before readiness")),
            Ok(count) => used += count,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                let mut descriptor = libc::pollfd {
                    fd: channel.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                };
                // SAFETY: descriptor points to a single initialized pollfd.
                let result = unsafe { libc::poll(&mut descriptor, 1, 20) };
                if result < 0 && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
                    return Err(io::Error::last_os_error());
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    if &message != b"AMKREADY" {
        return Err(io::Error::other("invalid KMS helper readiness response"));
    }
    Ok(())
}

fn duplicate(fd: RawFd) -> io::Result<OwnedFd> {
    // SAFETY: fcntl duplicates a live descriptor into a uniquely owned fd.
    let duplicated = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, FD_FLOOR) };
    if duplicated < 0 {
        Err(io::Error::last_os_error())
    } else {
        // SAFETY: successful fcntl returned a uniquely owned descriptor.
        Ok(unsafe { OwnedFd::from_raw_fd(duplicated) })
    }
}

fn error_pipe() -> io::Result<(std::fs::File, OwnedFd)> {
    let mut fds = [-1; 2];
    // SAFETY: fds is writable storage for two new descriptors.
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: pipe2 returned unique descriptors owned by this scope.
    let read = unsafe { std::fs::File::from_raw_fd(fds[0]) };
    let write = unsafe { OwnedFd::from_raw_fd(fds[1]) };
    Ok((read, duplicate(write.as_raw_fd())?))
}

fn await_exec(errors: &mut std::fs::File, receipt: &Receipt) -> io::Result<()> {
    let deadline = Instant::now() + IO_TIMEOUT;
    let mut message = [0u8; 8];
    let mut used = 0;
    loop {
        if receipt.1.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "helper startup cancelled",
            ));
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "helper exec timed out",
            ));
        }
        match errors.read(&mut message[used..]) {
            Ok(0) if used == 0 => return Ok(()),
            Ok(0) => return Err(io::Error::other("truncated helper setup error")),
            Ok(count) => {
                used += count;
                if used == message.len() {
                    let stage = u32::from_ne_bytes(message[..4].try_into().unwrap());
                    let errno = i32::from_ne_bytes(message[4..].try_into().unwrap());
                    let operation = match stage {
                        1 => "dup2",
                        2 => "close_range",
                        3 => "chdir",
                        4 => "execveat",
                        5 => "parent-death signal",
                        6 => "parent identity",
                        _ => "unknown",
                    };
                    return Err(io::Error::other(format!(
                        "helper {operation} failed: {}",
                        io::Error::from_raw_os_error(errno)
                    )));
                }
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                let mut fd = libc::pollfd {
                    fd: errors.as_raw_fd(),
                    events: libc::POLLIN | libc::POLLHUP,
                    revents: 0,
                };
                // SAFETY: fd points to a single initialized pollfd; timeout bounds cancellation latency.
                let result = unsafe { libc::poll(&mut fd, 1, 20) };
                if result < 0 && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
                    return Err(io::Error::last_os_error());
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
}

#[repr(C)]
#[derive(Default)]
struct CloneArgs {
    flags: u64,
    pidfd: u64,
    child_tid: u64,
    parent_tid: u64,
    exit_signal: u64,
    stack: u64,
    stack_size: u64,
    tls: u64,
    set_tid: u64,
    set_tid_size: u64,
    cgroup: u64,
}

/// Atomically creates a pidfd with the child. No reusable numeric PID is
/// ever used for signalling or reaping, even if another thread reaps it.
fn spawn_pidfd(
    executable: RawFd,
    socket: RawFd,
    host_fd: RawFd,
    error_fd: RawFd,
    fault: Fault,
) -> io::Result<OwnedFd> {
    if fault == Fault::Clone {
        return Err(io::Error::from_raw_os_error(libc::ENOSYS));
    }
    let mut pidfd: libc::c_int = -1;
    // The creating pthread stays alive through readiness or confirmed reap.
    // Capture the thread-group PID to detect a host death before prctl.
    let parent_pid = unsafe { libc::getpid() };
    let args = CloneArgs {
        flags: libc::CLONE_PIDFD as u64,
        pidfd: (&mut pidfd as *mut libc::c_int) as u64,
        exit_signal: libc::SIGCHLD as u64,
        ..CloneArgs::default()
    };
    // SAFETY: clone_args and pidfd storage remain valid across the syscall.
    // The child performs only raw async-signal-safe syscalls before exec/_exit.
    let result =
        unsafe { libc::syscall(libc::SYS_clone3, &args, std::mem::size_of::<CloneArgs>()) };
    if result < 0 {
        return Err(io::Error::last_os_error());
    }
    if result == 0 {
        // SAFETY: this is the single-threaded post-clone child. The inherited
        // descriptors and static argv/env strings are valid until exec.
        unsafe {
            if libc::syscall(
                libc::SYS_prctl,
                libc::PR_SET_PDEATHSIG,
                libc::SIGKILL,
                0,
                0,
                0,
            ) < 0
            {
                child_failure(error_fd, 5, *libc::__errno_location());
            }
            if libc::syscall(libc::SYS_getppid) != parent_pid as libc::c_long {
                child_failure(error_fd, 6, libc::ESRCH);
            }
            if fault == Fault::HoldBeforeExec {
                let marker = b"KMSARMED";
                let _ = libc::syscall(libc::SYS_write, error_fd, marker.as_ptr(), marker.len());
                loop {
                    libc::syscall(
                        libc::SYS_ppoll,
                        std::ptr::null::<libc::pollfd>(),
                        0,
                        std::ptr::null::<libc::timespec>(),
                        std::ptr::null::<libc::sigset_t>(),
                        0,
                    );
                }
            }
            if libc::syscall(libc::SYS_dup2, socket, 3) < 0 {
                child_failure(error_fd, 1, *libc::__errno_location());
            }
            if libc::syscall(libc::SYS_dup2, host_fd, 4) < 0 {
                child_failure(error_fd, 1, *libc::__errno_location());
            }
            if fault == Fault::CloseRange {
                child_failure(error_fd, 2, libc::ENOSYS);
            }
            if libc::syscall(
                libc::SYS_close_range,
                5u32,
                u32::MAX,
                libc::CLOSE_RANGE_CLOEXEC,
            ) < 0
            {
                child_failure(error_fd, 2, *libc::__errno_location());
            }
            if libc::syscall(libc::SYS_chdir, c"/".as_ptr()) < 0 {
                child_failure(error_fd, 3, *libc::__errno_location());
            }
            libc::syscall(libc::SYS_close, 0);
            libc::syscall(libc::SYS_close, 1);
            libc::syscall(libc::SYS_close, 2);
            let argv = [c"amaterasu-kms-helper".as_ptr(), std::ptr::null()];
            let env = [std::ptr::null::<libc::c_char>()];
            if fault == Fault::Exec {
                child_failure(error_fd, 4, libc::EPERM);
            }
            libc::syscall(
                libc::SYS_execveat,
                executable,
                c"".as_ptr(),
                argv.as_ptr(),
                env.as_ptr(),
                libc::AT_EMPTY_PATH,
            );
            child_failure(error_fd, 4, *libc::__errno_location());
        }
    }
    if pidfd < 0 {
        // CLONE_PIDFD requires the kernel to produce a descriptor on success.
        // No PID signalling fallback is safe if this invariant is violated.
        return Err(io::Error::other("clone3 returned no pidfd"));
    }
    // SAFETY: successful clone3 created one uniquely owned pidfd.
    Ok(unsafe { OwnedFd::from_raw_fd(pidfd) })
}

unsafe fn child_failure(fd: RawFd, stage: u32, errno: libc::c_int) -> ! {
    let values = [stage, errno as u32];
    // SAFETY: the post-clone child writes one bounded PIPE_BUF-sized message,
    // then exits without running destructors or touching shared runtime state.
    unsafe {
        let mut offset = 0;
        while offset < std::mem::size_of_val(&values) {
            let count = libc::write(
                fd,
                values.as_ptr().cast::<u8>().add(offset).cast(),
                std::mem::size_of_val(&values) - offset,
            );
            if count > 0 {
                offset += count as usize;
            } else if count < 0 && *libc::__errno_location() == libc::EINTR {
                continue;
            } else {
                break;
            }
        }
        libc::_exit(127);
    }
}

fn signal_pidfd(pidfd: RawFd) -> io::Result<()> {
    // SAFETY: syscall only borrows the live pidfd; null siginfo is permitted.
    if unsafe {
        libc::syscall(
            libc::SYS_pidfd_send_signal,
            pidfd,
            libc::SIGKILL,
            std::ptr::null::<libc::siginfo_t>(),
            0,
        )
    } < 0
    {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn poll_pidfd(pidfd: RawFd, block: bool) -> io::Result<bool> {
    // SAFETY: zero-initialized siginfo is writable for waitid.
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    let flags = libc::WEXITED | if block { 0 } else { libc::WNOHANG };
    // SAFETY: P_PIDFD identifies the exact child and info points to valid storage.
    if unsafe { libc::waitid(libc::P_PIDFD, pidfd as libc::id_t, &mut info, flags) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: waitid initialized siginfo; si_pid is zero when WNOHANG finds no exit.
    Ok(unsafe { info.si_pid() } != 0)
}

#[cfg(test)]
pub(crate) fn test_receipt() -> Arc<Receipt> {
    Arc::new(Receipt::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::time::Instant;

    fn launch_os_error(error: &io::Error) -> Option<i32> {
        error.raw_os_error().or_else(|| {
            error
                .get_ref()
                .and_then(|source| source.downcast_ref::<LaunchError>())
                .and_then(|source| source.source.raw_os_error())
        })
    }

    #[test]
    fn registry_reserves_capacity_before_process_creation() {
        let registry = Registry::default();
        let mut receipts = Vec::new();
        for _ in 0..MAX_CHILDREN {
            receipts.push(registry.reserve().unwrap());
        }
        assert!(registry.reserve().is_err());
        receipts[0].spawn_failed();
        assert!(registry.reserve().is_ok());
    }

    #[test]
    fn cancellation_before_spawn_retires_the_unstarted_reservation() {
        let registry = Registry::default();
        let cancellation = crate::open_task::Cancellation::default();
        assert!(cancellation.cancel());
        let error = spawn_cancellable(&registry, b"unused", &cancellation)
            .err()
            .unwrap();
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        assert!(registry.poll().unwrap());
    }

    #[test]
    fn panic_between_clone_and_adoption_reaps_before_creator_returns() {
        let registry = Registry::default();
        let image = include_bytes!(concat!(env!("OUT_DIR"), "/amaterasu-kms-helper"));
        let error = match spawn_with_fault(&registry, image, Fault::PanicAfterClone) {
            Ok(_) => panic!("injected panic accepted"),
            Err(error)
                if matches!(
                    launch_os_error(&error),
                    Some(libc::ENOSYS) | Some(libc::EPERM)
                ) =>
            {
                return;
            }
            Err(error) => error,
        };
        assert!(error.to_string().contains("startup panicked"));
        let entries = registry.0.lock().unwrap();
        assert_eq!(entries.len(), 1);
        let state = entries[0].0.lock().unwrap();
        // No child was adopted: PendingChild reaped it before the caught
        // panic allowed the creator to mark its reservation as unstarted.
        assert!(state.retired);
        assert!(state.child.is_none());
    }

    #[test]
    fn failed_handoff_waits_for_exact_child_reap_before_returning() {
        let registry = Registry::default();
        let image = include_bytes!(concat!(env!("OUT_DIR"), "/amaterasu-kms-helper"));
        let error = match spawn_with_fault(&registry, image, Fault::HoldBeforeExec) {
            Ok(_) => panic!("stalled handoff accepted"),
            Err(error)
                if matches!(
                    launch_os_error(&error),
                    Some(libc::ENOSYS) | Some(libc::EPERM)
                ) =>
            {
                return;
            }
            Err(error) => error,
        };
        assert!(error.to_string().contains("helper"));
        let entries = registry.0.lock().unwrap();
        assert_eq!(entries.len(), 1);
        let state = entries[0].0.lock().unwrap();
        assert!(state.retired);
        assert!(state.child.is_none());
    }

    /// Invoked only in a dedicated subprocess, never by the ordinary suite.
    #[test]
    #[ignore = "subprocess fixture for pre-exec parent-death regression"]
    fn preexec_creator_fixture() {
        if std::env::var_os("AM_KMS_CREATOR_FIXTURE").is_none() {
            return;
        }
        let executable = std::fs::File::open(std::env::current_exe().unwrap()).unwrap();
        let (_parent, child) = UnixStream::pair().unwrap();
        let socket = duplicate(child.as_raw_fd()).unwrap();
        // SAFETY: pidfd_open targets the current process and returns an owned fd.
        let host = unsafe { libc::syscall(libc::SYS_pidfd_open, libc::getpid(), 0) };
        assert!(host >= 0);
        // SAFETY: a successful pidfd_open returned a unique descriptor.
        let host = unsafe { OwnedFd::from_raw_fd(host as RawFd) };
        let host = duplicate(host.as_raw_fd()).unwrap();
        let (mut reader, writer) = error_pipe().unwrap();
        let helper = match spawn_pidfd(
            executable.as_raw_fd(),
            socket.as_raw_fd(),
            host.as_raw_fd(),
            writer.as_raw_fd(),
            Fault::HoldBeforeExec,
        ) {
            Ok(helper) => helper,
            Err(error)
                if matches!(error.raw_os_error(), Some(libc::ENOSYS) | Some(libc::EPERM)) =>
            {
                println!("KMS_SKIP");
                return;
            }
            Err(error) => panic!("clone3 fixture failed: {error}"),
        };
        drop(writer);
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut marker = [0u8; 8];
        loop {
            match reader.read(&mut marker) {
                Ok(8) => break,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                result => panic!("arming message failed: {result:?}"),
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(&marker, b"KMSARMED");
        let info =
            std::fs::read_to_string(format!("/proc/self/fdinfo/{}", helper.as_raw_fd())).unwrap();
        let pid = info
            .lines()
            .find_map(|line| line.strip_prefix("Pid:"))
            .unwrap()
            .trim();
        println!("KMS_CHILD_PID={pid}");
        std::io::stdout().flush().unwrap();
        // Remain the creating thread until the verifier kills this process.
        loop {
            std::thread::sleep(Duration::from_secs(1));
        }
    }

    #[test]
    #[ignore = "subprocess verifier for pre-exec parent-death regression"]
    fn preexec_verifier_fixture() {
        use std::{
            io::{BufRead, BufReader},
            process::{Command, Stdio},
            sync::mpsc,
        };
        if std::env::var_os("AM_KMS_VERIFIER_FIXTURE").is_none() {
            return;
        }
        // SAFETY: this isolated test subprocess becomes the grandchild's reaper.
        assert_eq!(
            unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) },
            0
        );
        let mut creator = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "helper_process::tests::preexec_creator_fixture",
                "--ignored",
                "--nocapture",
            ])
            .env("AM_KMS_CREATOR_FIXTURE", "1")
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = creator.stdout.take().unwrap();
        let (send, receive) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if line.contains("KMS_CHILD_PID=") || line.contains("KMS_SKIP") {
                    let _ = send.send(line);
                    return;
                }
            }
        });
        let line = match receive.recv_timeout(Duration::from_secs(5)) {
            Ok(line) => line,
            Err(error) => {
                let _ = creator.kill();
                let _ = creator.wait();
                panic!("creator did not arm: {error}");
            }
        };
        if line.contains("KMS_SKIP") {
            creator.wait().unwrap();
            println!("KMS_SKIP: clone3 unavailable");
            return;
        }
        let pid: i32 = line
            .split("KMS_CHILD_PID=")
            .nth(1)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        // Obtain both exact pidfds while their owned processes are still alive.
        let helper_fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
        let creator_fd = unsafe { libc::syscall(libc::SYS_pidfd_open, creator.id(), 0) };
        assert!(helper_fd >= 0 && creator_fd >= 0);
        // SAFETY: each successful pidfd_open returned one unique descriptor.
        let helper_fd = unsafe { OwnedFd::from_raw_fd(helper_fd as RawFd) };
        let creator_fd = unsafe { OwnedFd::from_raw_fd(creator_fd as RawFd) };
        signal_pidfd(creator_fd.as_raw_fd()).unwrap();
        creator.wait().unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
            // SAFETY: helper pidfd identifies the exact reparented grandchild.
            let result = unsafe {
                libc::waitid(
                    libc::P_PIDFD,
                    helper_fd.as_raw_fd() as libc::id_t,
                    &mut info,
                    libc::WEXITED | libc::WNOHANG,
                )
            };
            if result == 0 && unsafe { info.si_pid() } != 0 {
                assert_eq!(info.si_code, libc::CLD_KILLED);
                assert_eq!(unsafe { info.si_status() }, libc::SIGKILL);
                println!("KMS_PARENT_DEATH_VERIFIED");
                break;
            }
            if Instant::now() >= deadline {
                let _ = signal_pidfd(helper_fd.as_raw_fd());
                let _ = poll_pidfd(helper_fd.as_raw_fd(), true);
                panic!("pre-exec helper survived creator death");
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn creator_death_before_exec_kills_and_reaps_exact_helper() {
        use std::process::Command;
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "helper_process::tests::preexec_verifier_fixture",
                "--ignored",
                "--nocapture",
            ])
            .env("AM_KMS_VERIFIER_FIXTURE", "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "verifier failed: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        if stdout.contains("KMS_SKIP") {
            eprintln!("{stdout}");
        } else {
            assert!(stdout.contains("KMS_PARENT_DEATH_VERIFIED"), "{stdout}");
        }
    }

    #[test]
    fn cooperative_poll_and_stop_never_wait_for_owned_locks() {
        let registry = Registry::default();
        let receipt = registry.reserve().unwrap();
        let guard = receipt.0.lock().unwrap();
        receipt.stop();
        assert!(!receipt.poll().unwrap());
        assert!(receipt.1.load(Ordering::Acquire));
        drop(guard);
        let entries = registry.0.lock().unwrap();
        assert!(!registry.poll().unwrap());
        assert!(
            matches!(registry.reserve(), Err(error) if error.kind() == io::ErrorKind::WouldBlock)
        );
        drop(entries);
        receipt.spawn_failed();
        assert!(registry.poll().unwrap());
    }

    #[test]
    fn startup_error_pipe_honors_cancellation_without_waiting_for_eof() {
        let receipt = Receipt::new();
        let (mut reader, _writer) = error_pipe().unwrap();
        receipt.stop();
        assert_eq!(
            await_exec(&mut reader, &receipt).unwrap_err().kind(),
            io::ErrorKind::Interrupted
        );
    }

    #[test]
    fn stop_is_not_retirement_until_the_child_is_reaped() {
        let registry = Registry::default();
        let image = include_bytes!(concat!(env!("OUT_DIR"), "/amaterasu-kms-helper"));
        let (mut channel, receipt) = match spawn(&registry, image) {
            Ok(value) => value,
            Err(error)
                if matches!(
                    launch_os_error(&error),
                    Some(libc::ENOSYS) | Some(libc::EPERM)
                ) =>
            {
                eprintln!("clone3 unavailable: {error}");
                assert!(registry.poll().unwrap());
                return;
            }
            Err(error) => panic!("helper spawn failed: {error}"),
        };
        channel.set_nonblocking(false).unwrap();
        let mut request = [0u8; 200];
        request[..4].copy_from_slice(b"AMK1");
        request[4] = 1;
        request[8..12].copy_from_slice(b"bad!");
        channel.write_all(&request).unwrap();
        let mut response = [0u8; 24];
        channel.read_exact(&mut response).unwrap();
        assert_eq!(&response[..4], b"AMR1");
        assert!(!receipt.poll().unwrap());
        receipt.stop();
        let deadline = Instant::now() + Duration::from_secs(3);
        while !receipt.poll().unwrap() {
            assert!(Instant::now() < deadline, "helper was not reaped");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(receipt.0.lock().unwrap().kill_sent);
        assert!(registry.poll().unwrap());
    }

    #[test]
    fn closed_channel_allows_graceful_exit_before_forced_termination() {
        let registry = Registry::default();
        let image = include_bytes!(concat!(env!("OUT_DIR"), "/amaterasu-kms-helper"));
        let (channel, receipt) = match spawn(&registry, image) {
            Ok(value) => value,
            Err(error)
                if matches!(
                    launch_os_error(&error),
                    Some(libc::ENOSYS) | Some(libc::EPERM)
                ) =>
            {
                return;
            }
            Err(error) => panic!("helper spawn failed: {error}"),
        };
        drop(channel);
        receipt.stop();
        let deadline = Instant::now() + Duration::from_secs(3);
        while !receipt.poll().unwrap() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(!receipt.0.lock().unwrap().kill_sent);
    }

    #[test]
    fn external_reap_never_becomes_success_or_numeric_pid_signal() {
        let registry = Registry::default();
        let image = include_bytes!(concat!(env!("OUT_DIR"), "/amaterasu-kms-helper"));
        let (_channel, receipt) = match spawn(&registry, image) {
            Ok(value) => value,
            Err(error)
                if matches!(
                    launch_os_error(&error),
                    Some(libc::ENOSYS) | Some(libc::EPERM)
                ) =>
            {
                return;
            }
            Err(error) => panic!("helper spawn failed: {error}"),
        };
        {
            let state = receipt.0.lock().unwrap();
            let pidfd = state.child.as_ref().unwrap().as_raw_fd();
            signal_pidfd(pidfd).unwrap();
            assert!(poll_pidfd(pidfd, true).unwrap());
        }
        receipt.stop();
        assert_eq!(
            receipt.poll().unwrap_err().raw_os_error(),
            Some(libc::ECHILD)
        );
        assert!(receipt.poll().is_err());
        assert!(registry.poll().is_err());
    }

    #[test]
    fn failed_embedded_executable_is_reaped_before_retirement() {
        let registry = Registry::default();
        let error = match spawn(&registry, b"not an executable") {
            Ok(_) => panic!("invalid executable accepted"),
            Err(error)
                if matches!(
                    launch_os_error(&error),
                    Some(libc::ENOSYS) | Some(libc::EPERM)
                ) =>
            {
                return;
            }
            Err(error) => error,
        };
        assert!(error.to_string().contains("execveat"));
        let deadline = Instant::now() + Duration::from_secs(3);
        while !registry.poll().unwrap() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(registry.poll().unwrap());
    }

    #[test]
    fn injected_setup_failures_fail_closed_and_reap_children() {
        let image = include_bytes!(concat!(env!("OUT_DIR"), "/amaterasu-kms-helper"));
        for (fault, expected) in [
            (Fault::Clone, "clone3"),
            (Fault::Memfd, "memfd_create"),
            (Fault::CloseRange, "close_range"),
            (Fault::Exec, "execveat"),
        ] {
            let registry = Registry::default();
            let error = match spawn_with_fault(&registry, image, fault) {
                Ok(_) => panic!("injected failure accepted"),
                Err(error) => error,
            };
            if matches!(fault, Fault::CloseRange | Fault::Exec)
                && matches!(
                    launch_os_error(&error),
                    Some(libc::ENOSYS) | Some(libc::EPERM)
                )
            {
                continue;
            }
            assert!(
                error.to_string().to_lowercase().contains(expected),
                "{error}"
            );
            if matches!(fault, Fault::CloseRange | Fault::Exec) {
                let entries = registry.0.lock().unwrap();
                let state = entries[0].0.lock().unwrap();
                assert!(state.child.is_none());
                assert!(state.retired);
            }
            let deadline = Instant::now() + Duration::from_secs(3);
            while !registry.poll().unwrap() {
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }
}
