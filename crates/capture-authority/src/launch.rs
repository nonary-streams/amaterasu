//! Root-only launch and lifetime ownership for an approved worker.
use crate::{AUTHORITY_CHANNEL_FD, ApprovalRecord, ApprovalStore, AuthorityResult, files};
use amaterasu_package::{BootstrapConfig, VerificationPolicy, package_directory, verify_package};
use amaterasu_vkms_frame_helper::channel::{ChannelShutdown, SeqpacketChannel};
use std::{
    fs::File,
    io,
    os::{
        fd::{AsFd, AsRawFd, FromRawFd, OwnedFd},
        unix::{fs::MetadataExt, process::CommandExt},
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread::{self, JoinHandle},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const OPEN_SESSION: &[u8] = b"AMACA001";
const PRECHECK: &[u8] = b"AMACAP01";
const PRECHECK_READY: &[u8] = b"AMACAPOK";
const SESSION_READY: &[u8] = b"AMACAOK1";
const SESSION_DENIED: &[u8] = b"AMACANO1";
const MAX_SESSIONS: usize = 8;
const POLL_INTERVAL: Duration = Duration::from_millis(100);

pub struct LaunchOptions {
    pub approval_store: PathBuf,
    pub approval_id: String,
    pub package_root: PathBuf,
    pub worker: PathBuf,
    pub state_root: PathBuf,
}

/// Runs only an installed, signed package selected by an administrator. Root
/// retains the supervisor and helper; only the verified worker drops to uid/gid.
pub fn run(options: LaunchOptions) -> AuthorityResult<()> {
    crate::store::require_root()?;
    require_root_identity()?;
    let mut store = ApprovalStore::open(&options.approval_store)?;
    let approval = store.get(&options.approval_id)?;
    store.pin(&options.approval_id, &approval)?;
    verify_ptrace_boundary(approval.uid)?;
    let helper_path = verify_installation(&options, &approval)?;
    let executable = files::verify_file(&options.worker, &approval.worker_sha256, true, false)?;
    let (control, worker_control) = SeqpacketChannel::pair().map_err(|e| e.to_string())?;
    control
        .set_receive_timeout(Some(POLL_INTERVAL))
        .map_err(|e| e.to_string())?;
    set_send_timeout(&control)?;
    let mut worker = Worker(spawn_worker(
        executable,
        worker_control,
        &options,
        &approval,
    )?);
    let mut sessions: Vec<(ChannelShutdown, JoinHandle<AuthorityResult<()>>)> = Vec::new();
    let result = serve_control(
        &control,
        &store,
        &options,
        &approval,
        &helper_path,
        &mut worker.0,
        &mut sessions,
    );
    for (shutdown, _) in &sessions {
        shutdown.shutdown();
    }
    for (_, thread) in sessions {
        let _ = thread.join();
    }
    result
}

fn verify_installation(
    options: &LaunchOptions,
    approval: &ApprovalRecord,
) -> AuthorityResult<PathBuf> {
    let bootstrap_file = files::open_protected_file(&options.package_root.join("bootstrap.json"))?;
    let bootstrap: BootstrapConfig =
        serde_json::from_reader(bootstrap_file).map_err(|e| e.to_string())?;
    if bootstrap.development || bootstrap.allow_unsigned {
        return Err("privileged capture requires a signed production package".into());
    }
    files::open_protected_file(&options.package_root.join("selection.json"))?;
    let directory = package_directory(&options.package_root, &approval.request.manifest_digest)?;
    files::open_protected_file(&directory.join("manifest.json"))?;
    files::open_protected_file(&directory.join("manifest.sig"))?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_secs();
    let package = verify_package(
        &directory,
        &bootstrap,
        VerificationPolicy {
            minimum_sequence: 0,
            allow_historical: false,
            now,
        },
    )?;
    if package.digest != approval.request.manifest_digest
        || !package.privileged_requests.contains(&approval.request)
    {
        return Err("installed package no longer matches approved authority".into());
    }
    // Every library is immutable to the target user before its later dlopen.
    for component in &package.manifest.components {
        files::verify_file(
            &directory.join(&component.file),
            &component.sha256,
            false,
            false,
        )?;
    }
    let helper = directory.join(&approval.request.helper.file);
    files::verify_file(&helper, &approval.request.helper.sha256, true, true)?;
    verify_state_root(&options.state_root, approval.uid)?;
    Ok(helper)
}

fn verify_state_root(path: &Path, uid: u32) -> AuthorityResult<()> {
    if !path.is_absolute()
        || path.components().any(|c| {
            matches!(
                c,
                std::path::Component::ParentDir | std::path::Component::CurDir
            )
        })
    {
        return Err("worker state root must be an absolute normalized path".into());
    }
    let metadata = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !metadata.is_dir() || metadata.uid() != uid || metadata.mode() & 0o077 != 0 {
        return Err(
            "worker state root must be a private directory owned by the approved uid".into(),
        );
    }
    Ok(())
}

fn serve_control(
    control: &SeqpacketChannel,
    store: &ApprovalStore,
    options: &LaunchOptions,
    approval: &ApprovalRecord,
    helper_path: &Path,
    worker: &mut Child,
    sessions: &mut Vec<(ChannelShutdown, JoinHandle<AuthorityResult<()>>)>,
) -> AuthorityResult<()> {
    let mut buffer = [0; 9];
    loop {
        if store.get(&options.approval_id).as_ref() != Ok(approval) {
            return Err("capture authority was revoked or changed".into());
        }
        if let Some(status) = worker.try_wait().map_err(|e| e.to_string())? {
            return if status.success() {
                Ok(())
            } else {
                Err(format!("approved worker exited: {status}"))
            };
        }
        // Reap completed sessions so repeated probes do not exhaust the bound.
        let mut index = 0;
        while index < sessions.len() {
            if sessions[index].1.is_finished() {
                let (_, finished) = sessions.swap_remove(index);
                let _ = finished.join();
            } else {
                index += 1;
            }
        }
        let received = match control.receive(&mut buffer) {
            Ok(received) => received,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                continue;
            }
            Err(error) => return Err(error.to_string()),
        };
        if received.is_end_of_stream() {
            return Ok(());
        }
        if received.truncated || received.descriptors_truncated || !received.descriptors.is_empty()
        {
            return Err("malformed authority control request".into());
        }
        let Some(executable) =
            prepare_control_request(control, &buffer[..received.len], sessions.len(), || {
                // Recheck publisher, exact package, protected helper hash and
                // capabilities for both operations. This only reads installation
                // state; PRECHECK never enters the session allocation branch.
                verify_installation(options, approval)?;
                files::verify_file(helper_path, &approval.request.helper.sha256, true, true)
            })?
        else {
            continue;
        };
        let (server, client) = SeqpacketChannel::pair().map_err(|e| e.to_string())?;
        let shutdown = server.shutdown_handle().map_err(|e| e.to_string())?;
        let session_store = store.try_clone()?;
        let session_approval = approval.clone();
        let session_id = options.approval_id.clone();
        let handle = thread::spawn(move || {
            crate::proxy::serve_session(
                server,
                executable,
                session_approval,
                session_store,
                session_id,
            )
        });
        if let Err(error) = control.send(SESSION_READY, &[client.as_fd()]) {
            shutdown.shutdown();
            let _ = handle.join();
            return Err(error.to_string());
        }
        sessions.push((shutdown, handle));
    }
}

/// Return an executable only for an authorized OPEN. PRECHECK shares all
/// protected verification and returns an empty acknowledgement: no session
/// socketpair, SCM_RIGHTS descriptor, helper process or worker is allocated.
fn prepare_control_request(
    control: &SeqpacketChannel,
    bytes: &[u8],
    active_sessions: usize,
    verify: impl FnOnce() -> AuthorityResult<File>,
) -> AuthorityResult<Option<File>> {
    if bytes == PRECHECK {
        drop(verify()?);
        control
            .send(PRECHECK_READY, &[])
            .map_err(|error| error.to_string())?;
        return Ok(None);
    }
    if bytes != OPEN_SESSION {
        return Err("malformed authority control request".into());
    }
    if active_sessions >= MAX_SESSIONS {
        control
            .send(SESSION_DENIED, &[])
            .map_err(|error| error.to_string())?;
        return Ok(None);
    }
    verify().map(Some)
}

#[cfg(test)]
mod precheck_tests {
    use super::*;
    #[test]
    fn precheck_transcript_rechecks_authority_without_allocating_a_session() {
        let (client, server) = SeqpacketChannel::pair().unwrap();
        client.send(PRECHECK, &[]).unwrap();
        let mut bytes = [0; 9];
        let packet = server.receive(&mut bytes).unwrap();
        assert!(!packet.truncated && packet.descriptors.is_empty());
        let verified = std::cell::Cell::new(0);
        let opened = prepare_control_request(&server, &bytes[..packet.len], MAX_SESSIONS, || {
            verified.set(verified.get() + 1);
            File::open("/dev/null").map_err(|error| error.to_string())
        })
        .unwrap();
        assert!(
            opened.is_none(),
            "no execution/session continuation is returned"
        );
        assert_eq!(
            verified.get(),
            1,
            "precheck does not reuse an admission result"
        );
        let reply = client.receive(&mut bytes).unwrap();
        assert_eq!(&bytes[..reply.len], PRECHECK_READY);
        assert!(!reply.truncated && !reply.descriptors_truncated && reply.descriptors.is_empty());
    }
    #[test]
    fn revoked_precheck_and_unknown_requests_cannot_reach_session_creation() {
        let (client, server) = SeqpacketChannel::pair().unwrap();
        client
            .set_receive_timeout(Some(Duration::from_millis(1)))
            .unwrap();
        let error =
            prepare_control_request(&server, PRECHECK, 0, || Err("approval revoked".into()))
                .err()
                .unwrap();
        assert_eq!(error, "approval revoked");
        assert!(
            client.receive(&mut [0; 9]).is_err(),
            "no readiness acknowledgement follows failed verification"
        );
        assert!(
            prepare_control_request(&server, b"AMACAP00", 0, || panic!(
                "unknown opcode must not verify or open"
            ))
            .is_err()
        );
    }
}

struct Worker(Child);
impl Drop for Worker {
    fn drop(&mut self) {
        if matches!(self.0.try_wait(), Ok(None)) {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}

fn spawn_worker(
    executable: File,
    channel: SeqpacketChannel,
    options: &LaunchOptions,
    approval: &ApprovalRecord,
) -> AuthorityResult<Child> {
    let executable = duplicate_high(executable.into())?;
    let channel = duplicate_high(channel.into_owned())?;
    let mut command = Command::new(format!("/proc/self/fd/{}", executable.as_raw_fd()));
    command
        .arg0("amaterasu")
        .arg("worker-authorized-serve-v2")
        .arg(&options.package_root)
        .arg(&approval.request.manifest_digest)
        .arg(&options.state_root)
        .arg(&approval.request.component)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("LANG", "C.UTF-8")
        .current_dir("/")
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    // Desktop transport addresses carry no loader configuration. The UID's
    // runtime directory is derived; arbitrary LD_*, PYTHON*, etc never cross.
    command.env("XDG_RUNTIME_DIR", format!("/run/user/{}", approval.uid));
    command.env(
        "DBUS_SESSION_BUS_ADDRESS",
        format!("unix:path=/run/user/{}/bus", approval.uid),
    );
    if let Some(home) = user_home(approval.uid)? {
        command.env("HOME", home);
    }
    if let Some(display) = validated_wayland_display(approval.uid)? {
        command.env("WAYLAND_DISPLAY", display);
    }
    let uid = approval.uid;
    let gid = approval.gid;
    let socket = channel.as_raw_fd();
    let parent = std::process::id() as libc::pid_t;
    // SAFETY: only async-signal-safe syscalls execute in the forked child. All
    // values are copied; executable and channel descriptors remain open in parent.
    unsafe {
        command.pre_exec(move || {
            if libc::dup2(socket, AUTHORITY_CHANNEL_FD) < 0 {
                return Err(io::Error::last_os_error());
            }
            if libc::syscall(
                libc::SYS_close_range,
                3u32,
                197u32,
                libc::CLOSE_RANGE_CLOEXEC,
            ) != 0
                || libc::syscall(
                    libc::SYS_close_range,
                    199u32,
                    u32::MAX,
                    libc::CLOSE_RANGE_CLOEXEC,
                ) != 0
                || libc::setgroups(0, std::ptr::null()) != 0
                || libc::setresgid(gid, gid, gid) != 0
                || libc::setresuid(uid, uid, uid) != 0
                || libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0
                || libc::getppid() != parent
                || libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0
                || libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) != 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    command.spawn().map_err(|e| e.to_string())
}

fn duplicate_high(fd: OwnedFd) -> AuthorityResult<OwnedFd> {
    // SAFETY: fcntl duplicates a live descriptor with close-on-exec.
    let result = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 200) };
    if result < 0 {
        return Err(io::Error::last_os_error().to_string());
    }
    // SAFETY: the new descriptor has no other owner.
    Ok(unsafe { OwnedFd::from_raw_fd(result) })
}

fn user_home(uid: u32) -> AuthorityResult<Option<PathBuf>> {
    // A bounded reentrant lookup avoids retaining libc's global passwd memory.
    let mut buffer = vec![0u8; 65_536];
    let mut entry = std::mem::MaybeUninit::<libc::passwd>::uninit();
    let mut result = std::ptr::null_mut();
    // SAFETY: output buffers have stated sizes and live through the lookup/copy.
    let status = unsafe {
        libc::getpwuid_r(
            uid,
            entry.as_mut_ptr(),
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            &mut result,
        )
    };
    if status != 0 {
        return Err(format!("cannot resolve approved worker uid: {status}"));
    }
    if result.is_null() {
        return Err("approved worker uid does not exist".into());
    }
    // SAFETY: successful lookup initialized entry; its string lives in buffer.
    let home = unsafe { std::ffi::CStr::from_ptr((*result).pw_dir) }
        .to_str()
        .map_err(|e| e.to_string())?;
    Ok(Some(PathBuf::from(home)))
}

fn set_send_timeout(channel: &SeqpacketChannel) -> AuthorityResult<()> {
    let value = libc::timeval {
        tv_sec: 0,
        tv_usec: 100_000,
    };
    // SAFETY: channel is an open socket and value points to initialized timeval.
    let result = unsafe {
        libc::setsockopt(
            channel.as_fd().as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_SNDTIMEO,
            (&value as *const libc::timeval).cast(),
            std::mem::size_of::<libc::timeval>() as libc::socklen_t,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error().to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn control_backpressure_has_bounded_send_time() {
        let (server, _client) = SeqpacketChannel::pair().unwrap();
        set_send_timeout(&server).unwrap();
        let started = std::time::Instant::now();
        loop {
            match server.send(&[0; 4096], &[]) {
                Ok(()) => {}
                Err(error) => {
                    assert!(matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ));
                    break;
                }
            }
        }
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}

/// Ordinary exec temporarily resets dumpability before the host can reassert
/// it. Linux Yama prevents an unrelated same-UID process attaching in that gap.
/// For scope 1 the broker must itself have no approved-UID ancestor (a root
/// service launch, rather than a user shell invoking sudo).
fn verify_ptrace_boundary(uid: u32) -> AuthorityResult<()> {
    let value = std::fs::read_to_string("/proc/sys/kernel/yama/ptrace_scope")
        .map_err(|_| "approved capture requires Linux Yama ptrace_scope >= 1")?;
    let scope: u32 = value
        .trim()
        .parse()
        .map_err(|_| "invalid Linux Yama ptrace scope")?;
    if scope == 0 || scope > 3 {
        return Err("approved capture requires Linux Yama ptrace_scope >= 1".into());
    }
    if scope == 1 {
        verify_ancestors(uid, std::process::id())?;
    }
    Ok(())
}

fn verify_ancestors(uid: u32, mut pid: u32) -> AuthorityResult<()> {
    for _ in 0..256 {
        if pid <= 1 {
            return Ok(());
        }
        let status =
            std::fs::read_to_string(format!("/proc/{pid}/status")).map_err(|e| e.to_string())?;
        let (parent, real_uid) = parse_process_identity(&status)?;
        if real_uid == uid {
            return Err("Yama scope 1 requires the broker to be launched by a system service, outside the approved UID's ancestry".into());
        }
        if parent == pid {
            return Err("invalid process ancestry".into());
        }
        pid = parent;
    }
    Err("process ancestry exceeds capture authority bound".into())
}

fn parse_process_identity(status: &str) -> AuthorityResult<(u32, u32)> {
    let mut parent = None;
    let mut uid = None;
    for line in status.lines() {
        if let Some(value) = line.strip_prefix("PPid:") {
            parent = value.trim().parse::<u32>().ok();
        }
        if let Some(value) = line.strip_prefix("Uid:") {
            uid = value
                .split_whitespace()
                .next()
                .and_then(|s| s.parse::<u32>().ok());
        }
    }
    Ok((
        parent.ok_or("process parent identity unavailable")?,
        uid.ok_or("process user identity unavailable")?,
    ))
}

#[cfg(test)]
mod ptrace_tests {
    #[test]
    fn process_identity_uses_real_uid_and_parent() {
        assert_eq!(
            super::parse_process_identity(
                "Name:\tworker\nPPid:\t42\nUid:\t1000\t1000\t1000\t1000\n"
            )
            .unwrap(),
            (42, 1000)
        );
        assert!(super::parse_process_identity("Uid:\t0\n").is_err());
    }
}

fn require_root_identity() -> AuthorityResult<()> {
    let (mut real, mut effective, mut saved) = (u32::MAX, u32::MAX, u32::MAX);
    let (mut real_group, mut effective_group, mut saved_group) = (u32::MAX, u32::MAX, u32::MAX);
    // SAFETY: outputs point to three writable ids for each libc query.
    let valid = unsafe {
        libc::getresuid(&mut real, &mut effective, &mut saved) == 0
            && libc::getresgid(&mut real_group, &mut effective_group, &mut saved_group) == 0
    };
    if !valid
        || [
            real,
            effective,
            saved,
            real_group,
            effective_group,
            saved_group,
        ]
        .iter()
        .any(|id| *id != 0)
    {
        return Err("capture supervisor requires root real, effective and saved uid/gid".into());
    }
    Ok(())
}

/// Only an explicitly configured basename from the root service environment is
/// forwarded. No loader variables or arbitrary socket path is inherited.
fn validated_wayland_display(uid: u32) -> AuthorityResult<Option<String>> {
    let Some(value) = std::env::var_os("WAYLAND_DISPLAY") else {
        return Ok(None);
    };
    let name = value.to_str().ok_or("Wayland display name is not UTF-8")?;
    if !name.starts_with("wayland-")
        || name.len() > 100
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    {
        return Err("Wayland display must be a simple wayland-* socket basename".into());
    }
    let path = PathBuf::from(format!("/run/user/{uid}")).join(name);
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|e| format!("configured Wayland socket unavailable: {e}"))?;
    use std::os::unix::fs::FileTypeExt;
    if !metadata.file_type().is_socket() || metadata.uid() != uid {
        return Err("configured Wayland socket is not owned by the approved uid".into());
    }
    Ok(Some(name.to_owned()))
}
