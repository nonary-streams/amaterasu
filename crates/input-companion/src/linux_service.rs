//! User-session Unix transport and narrowly scoped uinput owner.
//!
//! The host keeps `LinuxInputService` and calls `authorize_worker` only after
//! its signed-set policy permits the worker. The worker receives no device
//! descriptor; it can submit only bounded keyboard and relative or normalized
//! absolute-pointer intent over a same-user Unix socket. This is an API
//! authorization boundary, not a sandbox for trusted native modules running in
//! the host process.
use crate::{
    DeviceSink, GrantRegistry, InputError, InputEvent, InputPacket, InputResult, InputSession,
    MouseButton, PROTOCOL_VERSION, PermitLedger, ProcessIdentity,
};
use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const MAX_CONNECTIONS: usize = 8;
const MAX_FRAME_BYTES: usize = 64 * 1024;
const MAX_SCROLL_DELTA: i32 = 12_000;
const EV_SYN: u16 = 0;
const EV_KEY: u16 = 1;
const EV_REL: u16 = 2;
const EV_ABS: u16 = 3;
const ABS_X: u16 = 0;
const ABS_Y: u16 = 1;
const REL_X: u16 = 0;
const REL_Y: u16 = 1;
const REL_HWHEEL: u16 = 6;
const REL_WHEEL: u16 = 8;
const REL_HWHEEL_HI_RES: u16 = 12;
const REL_WHEEL_HI_RES: u16 = 11;
const KEY_UNKNOWN: u32 = 240;
const INPUT_PROP_POINTER: u32 = 0;
const UI_SET_EVBIT: libc::Ioctl = libc::_IOW::<libc::c_int>(b'U' as u32, 100);
const UI_SET_KEYBIT: libc::Ioctl = libc::_IOW::<libc::c_int>(b'U' as u32, 101);
const UI_SET_RELBIT: libc::Ioctl = libc::_IOW::<libc::c_int>(b'U' as u32, 102);
const UI_SET_ABSBIT: libc::Ioctl = libc::_IOW::<libc::c_int>(b'U' as u32, 103);
const UI_SET_PROPBIT: libc::Ioctl = libc::_IOW::<libc::c_int>(b'U' as u32, 110);
const UI_DEV_SETUP: libc::Ioctl = libc::_IOW::<libc::uinput_setup>(b'U' as u32, 3);
const UI_ABS_SETUP: libc::Ioctl = libc::_IOW::<libc::uinput_abs_setup>(b'U' as u32, 4);
const UI_DEV_CREATE: libc::Ioctl = libc::_IO(b'U' as u32, 1);
const UI_DEV_DESTROY: libc::Ioctl = libc::_IO(b'U' as u32, 2);

#[derive(Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
    WorkerStart,
    WorkerEvent { packet: InputPacket },
    WorkerClose,
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Reply {
    Ok,
    Granted { generation: u64, token: String },
    Denied,
}

/// Host-owned server handle. The host must retain it while a worker can submit
/// input and refresh the permit at least every two seconds.
pub struct LinuxInputService {
    ledger: Arc<Mutex<PermitLedger>>,
    host: ProcessIdentity,
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<InputResult<()>>>,
    _socket_cleanup: BoundSocketPath,
    _socket_lock: SocketPathLock,
}

impl LinuxInputService {
    /// Starts an unprivileged listener using only the current process's
    /// existing access to `uinput_path`. The socket parent must be an existing
    /// directory owned by the effective UID and not writable by group/others.
    /// The socket is created with mode 0600; a same-user lock file remains in
    /// that directory to serialize service instances and permit stale-socket
    /// recovery. `uinput_path` must already be a
    /// writable character device, normally `/dev/uinput`; no permissions are
    /// changed and no service is installed. A host should call this only when
    /// its input companion policy opts in the exact selected component digest.
    pub fn start(socket_path: &Path, uinput_path: &Path) -> InputResult<Self> {
        validate_socket_parent(socket_path)?;
        validate_uinput_access(uinput_path)?;
        let socket_lock = SocketPathLock::acquire(socket_path)?;
        let host = inspect_process(std::process::id())?;
        let registry = GrantRegistry::session_only();
        let ledger = Arc::new(Mutex::new(PermitLedger::new(registry)));
        let stop = Arc::new(AtomicBool::new(false));
        remove_stale_socket(socket_path, &socket_lock)?;
        let listener = UnixListener::bind(socket_path).map_err(system_error)?;
        let mut socket_cleanup = BoundSocketPath::capture(socket_path)?;
        socket_cleanup.ensure_current()?;
        std::fs::set_permissions(socket_path, std::fs::Permissions::from_mode(0o600))
            .map_err(system_error)?;
        socket_cleanup.refresh_change_time()?;
        listener.set_nonblocking(true).map_err(system_error)?;
        socket_cleanup.ensure_current()?;
        let thread_ledger = Arc::clone(&ledger);
        let thread_stop = Arc::clone(&stop);
        let device_path = uinput_path.to_path_buf();
        let join = std::thread::Builder::new()
            .name("amaterasu-input-listener".into())
            .spawn(move || serve(listener, &device_path, thread_ledger, thread_stop))
            .map_err(system_error)?;
        Ok(Self {
            ledger,
            host,
            stop,
            join: Some(join),
            _socket_cleanup: socket_cleanup,
            _socket_lock: socket_lock,
        })
    }

    /// Installs or refreshes one host-approved worker grant. The PID and start
    /// time are re-read from `/proc`; on Linux `creation_time` is stat field 22
    /// (process starttime in clock ticks). Caller-supplied identity strings are
    /// not accepted. `digest` must come from the host's verified active package.
    pub fn authorize_worker(
        &self,
        process_id: u32,
        creation_time: u64,
        digest: &str,
        keyboard: bool,
        mouse: bool,
    ) -> InputResult<()> {
        let process = inspect_process(process_id)?;
        if !process.matches_instance(process_id, creation_time)
            || process.principal != self.host.principal
            || process.session != self.host.session
        {
            return Err(InputError::PermissionDenied);
        }
        self.ledger
            .lock()
            .map_err(|_| InputError::Closed)?
            .refresh_from_host(process, digest.to_owned(), keyboard, mouse, Instant::now())
    }

    /// Revokes immediately; the active worker loop observes this before its
    /// next packet and neutralizes/destroys its owned devices.
    pub fn revoke_worker(&self, process_id: u32, creation_time: u64) -> InputResult<()> {
        if process_id == 0 || creation_time == 0 {
            return Err(InputError::InvalidPeer);
        }
        if let Ok(live_process) = inspect_process(process_id)
            && !live_process.matches_instance(process_id, creation_time)
        {
            return Err(InputError::PermissionDenied);
        }
        self.ledger
            .lock()
            .map_err(|_| InputError::Closed)?
            .revoke_instance(process_id, creation_time);
        Ok(())
    }
}

impl Drop for LinuxInputService {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(join) = self.join.take() {
            if let Some(diagnostic) = listener_exit_diagnostic(join.join()) {
                eprintln!("{diagnostic}");
            }
        }
    }
}

/// Serializes cooperating owners of one socket path. The lock file is kept in
/// the protected socket directory and is deliberately not removed on drop:
/// unlinking a locked file could let another process lock a replacement inode.
struct SocketPathLock {
    _file: File,
}

impl SocketPathLock {
    fn acquire(socket_path: &Path) -> InputResult<Self> {
        let path = socket_lock_path(socket_path)?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)
            .map_err(system_error)?;
        let metadata = file.metadata().map_err(system_error)?;
        if !metadata.file_type().is_file()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.nlink() != 1
            || metadata.mode() & 0o077 != 0
        {
            return Err(InputError::PermissionDenied);
        }
        // SAFETY: flock only changes advisory lock state for this open file
        // description; the descriptor remains owned by this guard.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            let error = std::io::Error::last_os_error();
            return if error.kind() == std::io::ErrorKind::WouldBlock {
                Err(InputError::ResourceExhausted)
            } else {
                Err(system_error(error))
            };
        }
        Ok(Self { _file: file })
    }
}

fn socket_lock_path(socket_path: &Path) -> InputResult<PathBuf> {
    let parent = socket_path.parent().ok_or(InputError::InvalidPeer)?;
    let name = socket_path.file_name().ok_or(InputError::InvalidPeer)?;
    let mut lock_name = OsString::from(".");
    lock_name.push(name);
    lock_name.push(".lock");
    Ok(parent.join(lock_name))
}

/// Removes a stale socket only while holding its path lock. A live endpoint,
/// foreign-owned socket, symlink, or other filesystem object is preserved.
fn remove_stale_socket(socket_path: &Path, _lock: &SocketPathLock) -> InputResult<()> {
    for _ in 0..3 {
        let metadata = match std::fs::symlink_metadata(socket_path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(system_error(error)),
        };
        if !metadata.file_type().is_socket() || metadata.uid() != unsafe { libc::geteuid() } {
            return Err(InputError::PermissionDenied);
        }

        match UnixStream::connect(socket_path) {
            Ok(_connection) => return Err(InputError::ResourceExhausted),
            Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(system_error(error)),
        }

        if unlink_if_same_socket(socket_path, &metadata)? {
            return Ok(());
        }
    }
    Err(InputError::ResourceExhausted)
}

/// The final inode check narrows replacement races between the liveness probe
/// and unlink. The per-path flock serializes all companion instances.
fn unlink_if_same_socket(path: &Path, expected: &std::fs::Metadata) -> InputResult<bool> {
    let current = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(system_error(error)),
    };
    if !current.file_type().is_socket()
        || current.uid() != unsafe { libc::geteuid() }
        || current.dev() != expected.dev()
        || current.ino() != expected.ino()
        || current.ctime() != expected.ctime()
        || current.ctime_nsec() != expected.ctime_nsec()
    {
        return Ok(false);
    }
    match std::fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(system_error(error)),
    }
}

/// Owns the exact filesystem socket inode created by bind. It is installed
/// immediately after bind, so every later startup error path removes only that
/// inode; a replacement path is never unlinked.
struct BoundSocketPath {
    path: PathBuf,
    device: u64,
    inode: u64,
    uid: u32,
    change_time: i64,
    change_time_nanoseconds: i64,
}

impl BoundSocketPath {
    fn capture(path: &Path) -> InputResult<Self> {
        let metadata = std::fs::symlink_metadata(path).map_err(system_error)?;
        let uid = unsafe { libc::geteuid() };
        if !metadata.file_type().is_socket() || metadata.uid() != uid {
            return Err(InputError::PermissionDenied);
        }
        Ok(Self {
            path: path.to_path_buf(),
            device: metadata.dev(),
            inode: metadata.ino(),
            uid,
            change_time: metadata.ctime(),
            change_time_nanoseconds: metadata.ctime_nsec(),
        })
    }

    fn ensure_current(&self) -> InputResult<()> {
        let metadata = std::fs::symlink_metadata(&self.path).map_err(system_error)?;
        if self.matches(&metadata) {
            Ok(())
        } else {
            Err(InputError::PermissionDenied)
        }
    }

    fn refresh_change_time(&mut self) -> InputResult<()> {
        let metadata = std::fs::symlink_metadata(&self.path).map_err(system_error)?;
        if !self.matches_inode(&metadata) {
            return Err(InputError::PermissionDenied);
        }
        self.change_time = metadata.ctime();
        self.change_time_nanoseconds = metadata.ctime_nsec();
        Ok(())
    }

    fn matches(&self, metadata: &std::fs::Metadata) -> bool {
        self.matches_inode(metadata)
            && metadata.ctime() == self.change_time
            && metadata.ctime_nsec() == self.change_time_nanoseconds
    }

    fn matches_inode(&self, metadata: &std::fs::Metadata) -> bool {
        metadata.file_type().is_socket()
            && metadata.uid() == self.uid
            && metadata.dev() == self.device
            && metadata.ino() == self.inode
    }
}

impl Drop for BoundSocketPath {
    fn drop(&mut self) {
        let Ok(metadata) = std::fs::symlink_metadata(&self.path) else {
            return;
        };
        if self.matches(&metadata)
            && let Err(error) = std::fs::remove_file(&self.path)
        {
            eprintln!("could not remove owned input socket: {error}");
        }
    }
}

fn listener_exit_diagnostic(result: std::thread::Result<InputResult<()>>) -> Option<String> {
    match result {
        Ok(Ok(())) => None,
        Ok(Err(error)) => Some(format!("input companion listener ended: {error:?}")),
        Err(_) => Some("input companion listener thread panicked".into()),
    }
}

/// Worker-side client. Dropping its socket is authoritative and causes the
/// server to revoke the lease and release every held key/button.
pub struct LinuxInputClient {
    stream: BufReader<UnixStream>,
    token: String,
    generation: u64,
    next_sequence: u64,
    closed: bool,
}

impl LinuxInputClient {
    pub fn connect(socket_path: &Path) -> InputResult<Self> {
        let stream = UnixStream::connect(socket_path).map_err(system_error)?;
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .map_err(system_error)?;
        stream
            .set_write_timeout(Some(Duration::from_secs(2)))
            .map_err(system_error)?;
        let mut client = Self {
            stream: BufReader::new(stream),
            token: String::new(),
            generation: 0,
            next_sequence: 1,
            closed: false,
        };
        client.write_request(&Request::WorkerStart)?;
        let reply = client.read_reply()?;
        let Reply::Granted { generation, token } = reply else {
            return Err(InputError::PermissionDenied);
        };
        if generation == 0
            || token.len() != 32
            || !token.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(InputError::InvalidGrant);
        }
        client.generation = generation;
        client.token = token;
        Ok(client)
    }

    pub fn send(&mut self, event: InputEvent) -> InputResult<()> {
        if self.closed {
            return Err(InputError::Closed);
        }
        let packet = InputPacket {
            version: PROTOCOL_VERSION,
            generation: self.generation,
            sequence: self.next_sequence,
            lease_token: self.token.clone(),
            event,
        };
        self.next_sequence = self
            .next_sequence
            .checked_add(1)
            .ok_or(InputError::InvalidSequence)?;
        self.write_request(&Request::WorkerEvent { packet })?;
        match self.read_reply()? {
            Reply::Ok => Ok(()),
            _ => Err(InputError::PermissionDenied),
        }
    }

    pub fn close(&mut self) -> InputResult<()> {
        if self.closed {
            return Ok(());
        }
        self.write_request(&Request::WorkerClose)?;
        match self.read_reply()? {
            Reply::Ok => {
                self.closed = true;
                Ok(())
            }
            _ => Err(InputError::Device),
        }
    }

    fn write_request(&mut self, request: &Request) -> InputResult<()> {
        let bytes = serde_json::to_vec(request).map_err(|_| InputError::InvalidEvent)?;
        if bytes.len() > MAX_FRAME_BYTES {
            return Err(InputError::InvalidEvent);
        }
        let stream = self.stream.get_mut();
        stream.write_all(&bytes).map_err(system_error)?;
        stream.write_all(b"\n").map_err(system_error)
    }

    fn read_reply(&mut self) -> InputResult<Reply> {
        let frame = read_frame(&mut self.stream)?.ok_or(InputError::Closed)?;
        serde_json::from_slice(&frame).map_err(|_| InputError::InvalidEvent)
    }
}

impl Drop for LinuxInputClient {
    fn drop(&mut self) {
        if !self.closed {
            let _ = self.write_request(&Request::WorkerClose);
            // Socket close remains authoritative even if the best-effort frame
            // cannot be sent or acknowledged.
        }
    }
}

fn serve(
    listener: UnixListener,
    uinput_path: &Path,
    ledger: Arc<Mutex<PermitLedger>>,
    stop: Arc<AtomicBool>,
) -> InputResult<()> {
    let count = Arc::new(AtomicUsize::new(0));
    let cleanup_failed = Arc::new(AtomicBool::new(false));
    let mut workers: Vec<JoinHandle<()>> = Vec::new();
    let result = (|| {
        while !stop.load(Ordering::Acquire) {
            let mut active_workers = Vec::with_capacity(workers.len());
            for worker in workers.drain(..) {
                if worker.is_finished() {
                    let _ = worker.join();
                } else {
                    active_workers.push(worker);
                }
            }
            workers = active_workers;
            match listener.accept() {
                Ok((stream, _)) if count.fetch_add(1, Ordering::AcqRel) < MAX_CONNECTIONS => {
                    let ledger = Arc::clone(&ledger);
                    let stop = Arc::clone(&stop);
                    let cleanup_failed = Arc::clone(&cleanup_failed);
                    let path = uinput_path.to_path_buf();
                    let count = Arc::clone(&count);
                    workers.push(std::thread::spawn(move || {
                        if let Err(error) =
                            serve_worker(stream, &path, ledger, stop, cleanup_failed)
                        {
                            eprintln!("input companion worker ended: {error:?}");
                        }
                        count.fetch_sub(1, Ordering::AcqRel);
                    }));
                }
                Ok((_stream, _)) => {
                    count.fetch_sub(1, Ordering::AcqRel);
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => return Err(system_error(error)),
            }
        }
        Ok(())
    })();
    stop.store(true, Ordering::Release);
    for worker in workers {
        worker.join().map_err(|_| InputError::Device)?;
    }
    if cleanup_failed.load(Ordering::Acquire) {
        return Err(InputError::Device);
    }
    result
}

fn serve_worker(
    stream: UnixStream,
    uinput_path: &Path,
    ledger: Arc<Mutex<PermitLedger>>,
    stop: Arc<AtomicBool>,
    cleanup_failed: Arc<AtomicBool>,
) -> InputResult<()> {
    let process = peer_process(&stream)?;
    if !ledger
        .lock()
        .map_err(|_| InputError::Closed)?
        .may_open(&process, Instant::now())
    {
        write_reply_to(&stream, &Reply::Denied)?;
        return Err(InputError::PermissionDenied);
    }
    let mut reader = BufReader::new(stream);
    let Some(frame) = read_frame_timeout(&mut reader, Duration::from_secs(2))? else {
        return Err(InputError::Closed);
    };
    if !matches!(
        serde_json::from_slice::<Request>(&frame),
        Ok(Request::WorkerStart)
    ) {
        write_reply_to(reader.get_ref(), &Reply::Denied)?;
        return Err(InputError::PermissionDenied);
    }
    let lease = ledger
        .lock()
        .map_err(|_| InputError::Closed)?
        .claim(&process, Instant::now())?;
    let keyboard = lease.grant.keyboard_enabled();
    let mouse = lease.grant.mouse_enabled();
    let sink = match LinuxUinputSink::create(uinput_path, keyboard, mouse) {
        Ok(sink) => sink,
        Err(error) => {
            ledger
                .lock()
                .map_err(|_| InputError::Closed)?
                .release(&process);
            write_reply_to(reader.get_ref(), &Reply::Denied)?;
            return Err(error);
        }
    };
    let mut session = match InputSession::connect(&lease.peer, lease.grant, sink, Instant::now()) {
        Ok(session) => session,
        Err(error) => {
            ledger
                .lock()
                .map_err(|_| InputError::Closed)?
                .release(&process);
            return Err(error);
        }
    };
    if let Err(error) = write_reply_to(
        reader.get_ref(),
        &Reply::Granted {
            generation: lease.generation,
            token: lease.token,
        },
    ) {
        let cleanup = session.revoke();
        if session.cleanup_required() {
            cleanup_failed.store(true, Ordering::Release);
            stop.store(true, Ordering::Release);
        } else {
            ledger
                .lock()
                .map_err(|_| InputError::Closed)?
                .release(&process);
        }
        cleanup?;
        return Err(error);
    }
    let result = run_worker(&mut reader, &process, &ledger, &mut session, &stop);
    let cleanup = session.revoke();
    if session.cleanup_required() {
        cleanup_failed.store(true, Ordering::Release);
        stop.store(true, Ordering::Release);
    } else {
        ledger
            .lock()
            .map_err(|_| InputError::Closed)?
            .release(&process);
    }
    cleanup?;
    result
}

fn run_worker<S: DeviceSink>(
    reader: &mut BufReader<UnixStream>,
    process: &ProcessIdentity,
    ledger: &Arc<Mutex<PermitLedger>>,
    session: &mut InputSession<S>,
    stop: &AtomicBool,
) -> InputResult<()> {
    reader
        .get_ref()
        .set_read_timeout(Some(Duration::from_millis(50)))
        .map_err(system_error)?;
    while !stop.load(Ordering::Acquire) {
        if &inspect_process(process.process_id)? != process
            || !ledger
                .lock()
                .map_err(|_| InputError::Closed)?
                .is_current(process, Instant::now())
        {
            write_reply_to(reader.get_ref(), &Reply::Denied)?;
            break;
        }
        let now = Instant::now();
        session.refresh_host_authorized_expiry(now)?;
        if session.poll_expiry(now)? {
            break;
        }
        let Some(frame) = read_frame(reader)? else {
            continue;
        };
        // SO_PEERCRED is pinned to the connection. Re-inspecting PID starttime
        // for every frame rejects PID reuse and dead/disconnected workers.
        if &peer_process(reader.get_ref())? != process {
            write_reply_to(reader.get_ref(), &Reply::Denied)?;
            break;
        }
        match serde_json::from_slice::<Request>(&frame) {
            Ok(Request::WorkerEvent { packet }) => {
                // Serialize device writes with host revocation. Once
                // revoke_worker returns, no later packet can pass this lock.
                let permit = ledger.lock().map_err(|_| InputError::Closed)?;
                if !permit.is_current(process, Instant::now()) {
                    drop(permit);
                    write_reply_to(reader.get_ref(), &Reply::Denied)?;
                    break;
                }
                let accepted = session.submit(&packet, Instant::now()).is_ok();
                drop(permit);
                write_reply_to(
                    reader.get_ref(),
                    if accepted { &Reply::Ok } else { &Reply::Denied },
                )?;
                if !accepted {
                    break;
                }
            }
            Ok(Request::WorkerClose) => {
                session.revoke()?;
                write_reply_to(reader.get_ref(), &Reply::Ok)?;
                break;
            }
            _ => {
                write_reply_to(reader.get_ref(), &Reply::Denied)?;
                break;
            }
        }
    }
    Ok(())
}

struct LinuxUinputSink {
    keyboard: Option<UinputDevice>,
    pointer: Option<UinputDevice>,
    vertical_remainder: i32,
    horizontal_remainder: i32,
}

impl LinuxUinputSink {
    fn create(path: &Path, keyboard: bool, mouse: bool) -> InputResult<Self> {
        let keyboard = keyboard
            .then(|| UinputDevice::create(path, true))
            .transpose()?;
        let pointer = match mouse {
            true => match UinputDevice::create(path, false) {
                Ok(device) => Some(device),
                Err(error) => {
                    if let Some(mut keyboard) = keyboard {
                        let _ = keyboard.destroy();
                    }
                    return Err(error);
                }
            },
            false => None,
        };
        Ok(Self {
            keyboard,
            pointer,
            vertical_remainder: 0,
            horizontal_remainder: 0,
        })
    }
}

impl DeviceSink for LinuxUinputSink {
    fn submit(&mut self, event: &InputEvent) -> InputResult<()> {
        event.validate()?;
        match event {
            // Windows scan codes have no stable Linux EV_KEY interpretation;
            // Linux workers must use the explicit evdev key representation.
            InputEvent::Key { .. } => Err(InputError::InvalidEvent),
            InputEvent::EvdevKey { code, pressed } => self
                .keyboard
                .as_ref()
                .ok_or(InputError::PermissionDenied)?
                .emit(&[(EV_KEY, *code, i32::from(*pressed))]),
            InputEvent::EvdevKeyRepeat { code } => self
                .keyboard
                .as_ref()
                .ok_or(InputError::PermissionDenied)?
                .emit(&[(EV_KEY, *code, 2)]),
            InputEvent::Absolute { x, y } => self
                .pointer
                .as_ref()
                .ok_or(InputError::PermissionDenied)?
                .emit(&absolute_pointer_events(*x, *y)),
            InputEvent::MouseButton { button, pressed } => self
                .pointer
                .as_ref()
                .ok_or(InputError::PermissionDenied)?
                .emit(&[(EV_KEY, button_code(*button), i32::from(*pressed))]),
            InputEvent::Relative { dx, dy } => self
                .pointer
                .as_ref()
                .ok_or(InputError::PermissionDenied)?
                .emit(&[
                    (EV_REL, REL_X, i32::from(*dx)),
                    (EV_REL, REL_Y, i32::from(*dy)),
                ]),
            InputEvent::Scroll {
                vertical,
                horizontal,
            } => {
                let (events, vertical_remainder, horizontal_remainder) =
                    self.scroll_frame(i32::from(*vertical), i32::from(*horizontal))?;
                self.pointer
                    .as_ref()
                    .ok_or(InputError::PermissionDenied)?
                    .emit(&events)?;
                self.vertical_remainder = vertical_remainder;
                self.horizontal_remainder = horizontal_remainder;
                Ok(())
            }
        }
    }

    fn release_all(&mut self) -> InputResult<()> {
        let keyboard = self
            .keyboard
            .as_mut()
            .map(UinputDevice::destroy)
            .transpose();
        let pointer = self.pointer.as_mut().map(UinputDevice::destroy).transpose();
        if keyboard.is_ok() {
            self.keyboard = None;
        }
        if pointer.is_ok() {
            self.pointer = None;
        }
        keyboard?;
        pointer?;
        Ok(())
    }

    fn keep_alive(&mut self) -> InputResult<()> {
        Ok(())
    }
}

impl LinuxUinputSink {
    fn scroll_frame(
        &self,
        vertical: i32,
        horizontal: i32,
    ) -> InputResult<(Vec<(u16, u16, i32)>, i32, i32)> {
        if vertical.abs() > MAX_SCROLL_DELTA || horizontal.abs() > MAX_SCROLL_DELTA {
            return Err(InputError::InvalidEvent);
        }
        let vertical_total = self.vertical_remainder + vertical;
        let horizontal_total = self.horizontal_remainder + horizontal;
        let mut events = Vec::with_capacity(4);
        if vertical != 0 {
            events.push((EV_REL, REL_WHEEL_HI_RES, vertical));
            let steps = vertical_total / 120;
            if steps != 0 {
                events.push((EV_REL, REL_WHEEL, steps));
            }
        }
        if horizontal != 0 {
            events.push((EV_REL, REL_HWHEEL_HI_RES, horizontal));
            let steps = horizontal_total / 120;
            if steps != 0 {
                events.push((EV_REL, REL_HWHEEL, steps));
            }
        }
        Ok((events, vertical_total % 120, horizontal_total % 120))
    }
}

struct UinputDevice {
    file: File,
    created: bool,
}

impl UinputDevice {
    fn create(path: &Path, keyboard: bool) -> InputResult<Self> {
        let file = OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)
            .map_err(system_error)?;
        if !file
            .metadata()
            .map_err(system_error)?
            .file_type()
            .is_char_device()
        {
            return Err(InputError::Device);
        }
        let fd = file.as_raw_fd();
        set_bit(fd, UI_SET_EVBIT, u32::from(EV_KEY))?;
        if keyboard {
            for code in 1..=248 {
                if code != KEY_UNKNOWN {
                    set_bit(fd, UI_SET_KEYBIT, code)?;
                }
            }
        } else {
            set_bit(fd, UI_SET_PROPBIT, INPUT_PROP_POINTER)?;
            for code in 0x110..=0x114 {
                set_bit(fd, UI_SET_KEYBIT, code)?;
            }
            set_bit(fd, UI_SET_EVBIT, u32::from(EV_REL))?;
            for code in [
                REL_X,
                REL_Y,
                REL_WHEEL,
                REL_HWHEEL,
                REL_WHEEL_HI_RES,
                REL_HWHEEL_HI_RES,
            ] {
                set_bit(fd, UI_SET_RELBIT, u32::from(code))?;
            }
            set_bit(fd, UI_SET_EVBIT, u32::from(EV_ABS))?;
            configure_absolute_axis(fd, ABS_X)?;
            configure_absolute_axis(fd, ABS_Y)?;
        }
        // SAFETY: uinput_setup contains integer fields and a fixed name buffer;
        // it is fully initialized before the kernel reads it.
        let mut setup: libc::uinput_setup = unsafe { std::mem::zeroed() };
        setup.id.bustype = 0x06;
        let name: &[u8] = if keyboard {
            b"Amaterasu keyboard"
        } else {
            b"Amaterasu pointer"
        };
        for (target, source) in setup.name.iter_mut().zip(name) {
            *target = *source as libc::c_char;
        }
        // SAFETY: fd is a live owned uinput descriptor and setup remains valid
        // for the exact UAPI structure size encoded by UI_DEV_SETUP.
        check(unsafe { libc::ioctl(fd, UI_DEV_SETUP, &setup) })?;
        // SAFETY: fd owns this one configured virtual device.
        check(unsafe { libc::ioctl(fd, UI_DEV_CREATE) })?;
        Ok(Self {
            file,
            created: true,
        })
    }

    fn emit(&self, values: &[(u16, u16, i32)]) -> InputResult<()> {
        let mut frame = values
            .iter()
            .map(|(kind, code, value)| {
                // SAFETY: input_event is an integer-only Linux UAPI record.
                let mut event: libc::input_event = unsafe { std::mem::zeroed() };
                event.type_ = *kind;
                event.code = *code;
                event.value = *value;
                event
            })
            .collect::<Vec<_>>();
        // SAFETY: input_event is integer-only; zero timestamp is valid for uinput.
        let mut sync: libc::input_event = unsafe { std::mem::zeroed() };
        sync.type_ = EV_SYN;
        frame.push(sync);
        let bytes = unsafe {
            // SAFETY: frame is initialized and remains alive for the byte view.
            std::slice::from_raw_parts(
                frame.as_ptr().cast::<u8>(),
                frame.len() * std::mem::size_of::<libc::input_event>(),
            )
        };
        let written =
            unsafe { libc::write(self.file.as_raw_fd(), bytes.as_ptr().cast(), bytes.len()) };
        if written < 0 || written as usize != bytes.len() {
            return Err(InputError::Device);
        }
        Ok(())
    }

    fn destroy(&mut self) -> InputResult<()> {
        if self.created {
            // SAFETY: this owned fd created exactly this uinput device.
            check(unsafe { libc::ioctl(self.file.as_raw_fd(), UI_DEV_DESTROY) })?;
            self.created = false;
        }
        Ok(())
    }
}

impl Drop for UinputDevice {
    fn drop(&mut self) {
        let _ = self.destroy();
    }
}

fn peer_process(stream: &UnixStream) -> InputResult<ProcessIdentity> {
    // SAFETY: credentials is writable storage sized exactly for SO_PEERCRED.
    let mut credentials: libc::ucred = unsafe { std::mem::zeroed() };
    let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: getsockopt writes one ucred record to the live output buffer.
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut credentials as *mut libc::ucred).cast(),
            &mut length,
        )
    };
    if result != 0 || length as usize != std::mem::size_of::<libc::ucred>() {
        return Err(InputError::InvalidPeer);
    }
    let process = inspect_process(credentials.pid as u32)?;
    if process.principal != format!("uid:{}", credentials.uid) {
        return Err(InputError::InvalidPeer);
    }
    Ok(process)
}

fn inspect_process(process_id: u32) -> InputResult<ProcessIdentity> {
    let proc_root = Path::new("/proc").join(process_id.to_string());
    let status =
        std::fs::read_to_string(proc_root.join("status")).map_err(|_| InputError::InvalidPeer)?;
    let uid = status
        .lines()
        .find_map(|line| line.strip_prefix("Uid:")?.split_whitespace().nth(1))
        .and_then(|value| value.parse::<u32>().ok())
        .ok_or(InputError::InvalidPeer)?;
    let stat =
        std::fs::read_to_string(proc_root.join("stat")).map_err(|_| InputError::InvalidPeer)?;
    let fields = stat
        .rsplit_once(')')
        .ok_or(InputError::InvalidPeer)?
        .1
        .split_whitespace()
        .collect::<Vec<_>>();
    let start_time = fields
        .get(19)
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value != 0)
        .ok_or(InputError::InvalidPeer)?;
    // SAFETY: getsid only queries the kernel process session for this PID.
    let session = unsafe { libc::getsid(process_id as libc::pid_t) };
    if session < 0 {
        return Err(InputError::InvalidPeer);
    }
    ProcessIdentity::new(format!("uid:{uid}"), session as u32, process_id, start_time)
}

fn validate_socket_parent(path: &Path) -> InputResult<()> {
    let parent = path.parent().ok_or(InputError::InvalidPeer)?;
    let metadata = std::fs::metadata(parent).map_err(system_error)?;
    if !metadata.is_dir()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o022 != 0
    {
        return Err(InputError::PermissionDenied);
    }
    Ok(())
}

fn validate_uinput_access(path: &Path) -> InputResult<()> {
    let device = OpenOptions::new()
        .write(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(system_error)?;
    if !device
        .metadata()
        .map_err(system_error)?
        .file_type()
        .is_char_device()
    {
        return Err(InputError::Device);
    }
    Ok(())
}

fn read_frame(reader: &mut BufReader<UnixStream>) -> InputResult<Option<Vec<u8>>> {
    read_frame_timeout(reader, Duration::from_millis(50))
}

fn read_frame_timeout(
    reader: &mut BufReader<UnixStream>,
    timeout: Duration,
) -> InputResult<Option<Vec<u8>>> {
    reader
        .get_ref()
        .set_read_timeout(Some(timeout))
        .map_err(system_error)?;
    let mut frame = Vec::new();
    match reader.read_until(b'\n', &mut frame) {
        Ok(0) => Err(InputError::Closed),
        Ok(length) if length <= MAX_FRAME_BYTES && frame.last() == Some(&b'\n') => {
            frame.pop();
            Ok(Some(frame))
        }
        Ok(_) => Err(InputError::InvalidEvent),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
            ) && frame.is_empty() =>
        {
            Ok(None)
        }
        Err(error) => Err(system_error(error)),
    }
}

fn write_reply_to(stream: &UnixStream, reply: &Reply) -> InputResult<()> {
    let bytes = serde_json::to_vec(reply).map_err(|_| InputError::InvalidEvent)?;
    if bytes.len() > MAX_FRAME_BYTES {
        return Err(InputError::InvalidEvent);
    }
    let mut writer = stream;
    writer.write_all(&bytes).map_err(system_error)?;
    writer.write_all(b"\n").map_err(system_error)
}

fn set_bit(fd: RawFd, request: libc::Ioctl, bit: u32) -> InputResult<()> {
    // SAFETY: uinput bit ioctls read one integer value from this stack slot.
    check(unsafe { libc::ioctl(fd, request, bit as libc::c_int) })
}

fn configure_absolute_axis(fd: RawFd, code: u16) -> InputResult<()> {
    set_bit(fd, UI_SET_ABSBIT, u32::from(code))?;
    let setup = libc::uinput_abs_setup {
        code,
        absinfo: libc::input_absinfo {
            value: 0,
            minimum: 0,
            maximum: i32::from(u16::MAX),
            fuzz: 0,
            flat: 0,
            resolution: 0,
        },
    };
    // SAFETY: the initialized integer-only UAPI record stays valid for this ioctl.
    check(unsafe { libc::ioctl(fd, UI_ABS_SETUP, &setup) })
}

fn absolute_pointer_events(x: u16, y: u16) -> [(u16, u16, i32); 2] {
    [(EV_ABS, ABS_X, i32::from(x)), (EV_ABS, ABS_Y, i32::from(y))]
}

fn check(result: libc::c_int) -> InputResult<()> {
    if result < 0 {
        Err(InputError::Device)
    } else {
        Ok(())
    }
}

fn system_error(_: std::io::Error) -> InputError {
    InputError::Device
}

fn button_code(button: MouseButton) -> u16 {
    match button {
        MouseButton::Left => 0x110,
        MouseButton::Right => 0x111,
        MouseButton::Middle => 0x112,
        MouseButton::Back => 0x113,
        MouseButton::Forward => 0x114,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    fn socket_test_directory() -> PathBuf {
        static NEXT_ID: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "amaterasu-input-socket-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        path
    }

    #[test]
    fn listener_cleanup_failure_is_reported_on_join() {
        assert_eq!(listener_exit_diagnostic(Ok(Ok(()))), None);
        assert_eq!(
            listener_exit_diagnostic(Ok(Err(InputError::Device))).as_deref(),
            Some("input companion listener ended: Device")
        );
    }

    #[test]
    fn absolute_pointer_events_preserve_normalized_coordinate_bounds() {
        assert_eq!(
            absolute_pointer_events(0, u16::MAX),
            [(EV_ABS, ABS_X, 0), (EV_ABS, ABS_Y, i32::from(u16::MAX))]
        );
    }

    #[test]
    fn stale_owned_socket_is_removed_before_rebind() {
        let directory = socket_test_directory();
        let path = directory.join("input.sock");
        let listener = UnixListener::bind(&path).unwrap();
        drop(listener);

        let lock = SocketPathLock::acquire(&path).unwrap();
        remove_stale_socket(&path, &lock).unwrap();
        assert!(std::fs::symlink_metadata(&path).is_err());

        let replacement = UnixListener::bind(&path).unwrap();
        drop(replacement);
        drop(lock);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn live_owned_socket_is_never_unlinked() {
        let directory = socket_test_directory();
        let path = directory.join("input.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let identity = std::fs::symlink_metadata(&path).unwrap();
        let lock = SocketPathLock::acquire(&path).unwrap();

        assert_eq!(
            remove_stale_socket(&path, &lock),
            Err(InputError::ResourceExhausted)
        );
        let current = std::fs::symlink_metadata(&path).unwrap();
        assert_eq!(
            (current.dev(), current.ino()),
            (identity.dev(), identity.ino())
        );
        let connection = UnixStream::connect(&path).unwrap();

        drop(connection);
        drop(listener);
        drop(lock);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn stale_probe_does_not_unlink_a_replacement_socket() {
        let directory = socket_test_directory();
        let path = directory.join("input.sock");
        let stale_listener = UnixListener::bind(&path).unwrap();
        let stale_identity = std::fs::symlink_metadata(&path).unwrap();
        std::fs::remove_file(&path).unwrap();

        let live_listener = UnixListener::bind(&path).unwrap();
        let live_identity = std::fs::symlink_metadata(&path).unwrap();
        drop(stale_listener);
        let lock = SocketPathLock::acquire(&path).unwrap();
        assert!(!unlink_if_same_socket(&path, &stale_identity).unwrap());
        let current = std::fs::symlink_metadata(&path).unwrap();
        assert_eq!(
            (current.dev(), current.ino()),
            (live_identity.dev(), live_identity.ino())
        );
        let connection = UnixStream::connect(&path).unwrap();

        drop(connection);
        drop(live_listener);
        drop(lock);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn non_socket_path_is_preserved_during_recovery() {
        let directory = socket_test_directory();
        let path = directory.join("input.sock");
        std::fs::write(&path, b"owned regular file").unwrap();
        let lock = SocketPathLock::acquire(&path).unwrap();

        assert_eq!(
            remove_stale_socket(&path, &lock),
            Err(InputError::PermissionDenied)
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"owned regular file");

        drop(lock);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn symlink_to_live_socket_is_preserved_during_recovery() {
        let directory = socket_test_directory();
        let target = directory.join("foreign.sock");
        let path = directory.join("input.sock");
        let listener = UnixListener::bind(&target).unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();
        let lock = SocketPathLock::acquire(&path).unwrap();

        assert_eq!(
            remove_stale_socket(&path, &lock),
            Err(InputError::PermissionDenied)
        );
        assert_eq!(std::fs::read_link(&path).unwrap(), target);
        let connection = UnixStream::connect(&target).unwrap();

        drop(connection);
        drop(listener);
        drop(lock);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn post_bind_cleanup_removes_only_the_captured_socket_inode() {
        let directory = socket_test_directory();
        let path = directory.join("input.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let startup = (|| {
            let _cleanup = BoundSocketPath::capture(&path)?;
            Err::<(), _>(InputError::Device)
        })();
        assert_eq!(startup, Err(InputError::Device));
        assert!(std::fs::symlink_metadata(&path).is_err());
        drop(listener);

        let old_listener = UnixListener::bind(&path).unwrap();
        let old_cleanup = BoundSocketPath::capture(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        let replacement = UnixListener::bind(&path).unwrap();
        let replacement_identity = std::fs::symlink_metadata(&path).unwrap();
        drop(old_listener);
        drop(old_cleanup);
        let current = std::fs::symlink_metadata(&path).unwrap();
        assert_eq!(
            (current.dev(), current.ino()),
            (replacement_identity.dev(), replacement_identity.ino())
        );
        let connection = UnixStream::connect(&path).unwrap();

        drop(connection);
        drop(replacement);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn socket_lock_prevents_two_companion_startups_from_racing() {
        let directory = socket_test_directory();
        let path = directory.join("input.sock");
        let first_lock = SocketPathLock::acquire(&path).unwrap();
        assert!(matches!(
            SocketPathLock::acquire(&path),
            Err(InputError::ResourceExhausted)
        ));
        drop(first_lock);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn peer_identity_comes_from_socket_credentials_and_proc_starttime() {
        let (server, _client) = UnixStream::pair().unwrap();
        let peer = peer_process(&server).unwrap();

        assert!(peer.matches_instance(std::process::id(), peer.creation_time));
        assert_eq!(
            peer.principal,
            format!("uid:{}", unsafe { libc::geteuid() })
        );
    }

    #[test]
    fn closed_worker_socket_is_distinguished_from_a_read_timeout() {
        let (server, client) = UnixStream::pair().unwrap();
        drop(client);
        let mut reader = BufReader::new(server);

        assert_eq!(
            read_frame_timeout(&mut reader, Duration::from_millis(50)),
            Err(InputError::Closed)
        );
    }
}
