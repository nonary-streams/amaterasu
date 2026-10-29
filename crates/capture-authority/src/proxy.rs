//! Per-session forwarding between one authenticated worker and its trusted helper.
//!
//! The worker never receives the helper socket: this proxy remains on every
//! request and checks the protected approval again before exporting descriptors.
use crate::{ApprovalRecord, ApprovalStore};
use amaterasu_vkms_frame_helper::{
    HELPER_SOCKET_FD,
    channel::{Received, SeqpacketChannel},
    protocol::{self, Reply, Request, RequestRecord, ResponseRecord},
};
use std::{
    fs::File,
    io,
    os::{
        fd::{AsFd, AsRawFd, FromRawFd, OwnedFd},
        unix::process::CommandExt,
    },
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

const RECHECK_INTERVAL: Duration = Duration::from_millis(100);

/// Serves exactly one session. Closing its worker socket, losing its approval,
/// or seeing an invalid record closes both channels and reaps the helper.
///
/// `helper_executable` must already be verified against the signed request and
/// opened from protected storage. It is executed through its pinned descriptor.
pub fn serve_session(
    client: SeqpacketChannel,
    helper_executable: File,
    approval: ApprovalRecord,
    store: ApprovalStore,
    approval_id: String,
) -> Result<(), String> {
    require_current_approval(&store, &approval_id, &approval)?;
    let helper = HelperSession::spawn(helper_executable)?;
    proxy_session(
        &client,
        &helper.channel,
        &approval,
        ProtectedApproval {
            store: &store,
            identifier: &approval_id,
            approval: &approval,
        },
    )
}

trait SessionAuthorization {
    fn check(&self) -> Result<(), String>;
    fn execute(&self, operation: impl FnOnce() -> Result<(), String>) -> Result<(), String>;
}

struct ProtectedApproval<'a> {
    store: &'a ApprovalStore,
    identifier: &'a str,
    approval: &'a ApprovalRecord,
}

impl SessionAuthorization for ProtectedApproval<'_> {
    fn check(&self) -> Result<(), String> {
        require_current_approval(self.store, self.identifier, self.approval)
    }

    fn execute(&self, operation: impl FnOnce() -> Result<(), String>) -> Result<(), String> {
        self.store
            .with_current(self.identifier, self.approval, operation)
    }
}

// Test sessions supply an in-memory approval check without creating protected
// machine state. Production uses ProtectedApproval and its revocation lock.
#[cfg(test)]
impl<F: Fn() -> Result<(), String>> SessionAuthorization for F {
    fn check(&self) -> Result<(), String> {
        self()
    }

    fn execute(&self, operation: impl FnOnce() -> Result<(), String>) -> Result<(), String> {
        self()?;
        operation()
    }
}

fn require_current_approval(
    store: &ApprovalStore,
    identifier: &str,
    expected: &ApprovalRecord,
) -> Result<(), String> {
    if store.get(identifier)? != *expected {
        return Err("capture authority approval changed or was revoked".into());
    }
    Ok(())
}

/// Only fixed helper protocol operations are admitted. Successful card opening
/// gates frame work; a request cannot change its approved device afterward.
#[derive(Default)]
struct SessionPolicy {
    last_request_id: u32,
    card_open: bool,
}

impl SessionPolicy {
    fn admit(&mut self, bytes: &[u8], approved_card: u32) -> Result<RequestRecord, String> {
        let record = protocol::decode_request(bytes)
            .map_err(|_| "capture authority received a malformed request")?;
        if record.request_id != self.last_request_id.wrapping_add(1).max(1) {
            return Err("capture authority request was replayed or out of order".into());
        }
        match record.request {
            Request::Hello => {}
            Request::OpenCard { major, minor }
                if !self.card_open && major == 226 && minor == approved_card => {}
            Request::OpenCard { .. } => {
                return Err("capture authority denied an unapproved DRM card".into());
            }
            Request::Frame { .. } | Request::Wait { .. } if self.card_open => {}
            Request::Frame { .. } | Request::Wait { .. } => {
                return Err("capture authority requires an approved open card".into());
            }
        }
        self.last_request_id = record.request_id;
        Ok(record)
    }

    fn accept_reply(
        &mut self,
        request: &RequestRecord,
        response: &ResponseRecord,
        descriptor_count: usize,
    ) -> Result<(), String> {
        if response.request_id != request.request_id
            || response.opcode != request.request.opcode() as u16
        {
            return Err("capture authority helper response did not match its request".into());
        }
        let expected = match response.reply {
            Reply::FrameReady(frame) => frame.descriptor_count(),
            _ => 0,
        };
        if descriptor_count != expected {
            return Err("capture authority helper returned invalid descriptors".into());
        }
        match (&request.request, &response.reply) {
            (_, Reply::Failure { .. }) | (Request::Hello, Reply::Hello { .. }) => {}
            (Request::OpenCard { .. }, Reply::CardOpened) => self.card_open = true,
            (Request::Frame { crtc_id }, Reply::FrameReady(frame)) if *crtc_id == frame.crtc_id => {
            }
            (
                Request::Frame { crtc_id },
                Reply::FrameEmpty {
                    crtc_id: echoed, ..
                },
            )
            | (
                Request::Wait { crtc_id, .. },
                Reply::Presentation {
                    crtc_id: echoed, ..
                },
            ) if crtc_id == echoed => {}
            _ => return Err("capture authority helper returned an unexpected response".into()),
        }
        Ok(())
    }
}

fn proxy_session(
    client: &SeqpacketChannel,
    helper: &SeqpacketChannel,
    approval: &ApprovalRecord,
    authorization: impl SessionAuthorization,
) -> Result<(), String> {
    client
        .set_receive_timeout(Some(RECHECK_INTERVAL))
        .map_err(|error| error.to_string())?;
    helper
        .set_receive_timeout(Some(RECHECK_INTERVAL))
        .map_err(|error| error.to_string())?;
    set_send_timeout(client)?;
    set_send_timeout(helper)?;
    let mut policy = SessionPolicy::default();
    let mut request_buffer = [0; protocol::REQUEST_LEN + 1];
    let mut response_buffer = [0; protocol::RESPONSE_LEN + 1];
    loop {
        authorization.check()?;
        let received = match client.receive(&mut request_buffer) {
            Ok(received) => received,
            Err(error) if is_timeout(&error) => continue,
            Err(error) => return Err(error.to_string()),
        };
        if received.is_end_of_stream() {
            return Ok(());
        }
        validate_transport(&received, false)?;
        let request = policy.admit(
            &request_buffer[..received.len],
            approval.request.resource.drm_card,
        )?;
        authorization.execute(|| {
            helper
                .send(&request_buffer[..received.len], &[])
                .map_err(|error| error.to_string())
        })?;
        let deadline = Instant::now() + response_timeout(&request.request);
        let response = loop {
            authorization.check()?;
            match helper.receive(&mut response_buffer) {
                Ok(received) => break received,
                Err(error) if is_timeout(&error) && Instant::now() < deadline => {}
                Err(error) => return Err(format!("capture helper response failed: {error}")),
            }
        };
        validate_transport(&response, true)?;
        let record = protocol::decode_response(&response_buffer[..response.len])
            .map_err(|_| "capture authority helper returned a malformed response")?;
        policy.accept_reply(&request, &record, response.descriptors.len())?;
        // The approval is checked after the potentially blocking driver operation,
        // immediately before any new DMA-BUF reaches the user worker.
        let descriptors: Vec<_> = response.descriptors.iter().map(AsFd::as_fd).collect();
        authorization.execute(|| {
            client
                .send(&response_buffer[..response.len], &descriptors)
                .map_err(|error| error.to_string())
        })?;
        // SCM_RIGHTS duplicates accepted descriptors; these local copies close
        // when `response` leaves this iteration, including send failure paths.
    }
}

fn validate_transport(received: &Received, allow_descriptors: bool) -> Result<(), String> {
    if received.truncated
        || received.descriptors_truncated
        || (!allow_descriptors && !received.descriptors.is_empty())
    {
        return Err("capture authority received an invalid transport record".into());
    }
    Ok(())
}

fn is_timeout(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    )
}

fn set_send_timeout(channel: &SeqpacketChannel) -> Result<(), String> {
    let timeout = libc::timeval {
        tv_sec: 0,
        tv_usec: RECHECK_INTERVAL.as_micros() as libc::suseconds_t,
    };
    // SAFETY: the channel owns the socket and the option points to an initialized
    // timeval valid for its stated size. This bounds a peer that stops reading.
    let result = unsafe {
        libc::setsockopt(
            channel.as_fd().as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_SNDTIMEO,
            (&timeout as *const libc::timeval).cast(),
            std::mem::size_of::<libc::timeval>() as libc::socklen_t,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error().to_string());
    }
    Ok(())
}

fn response_timeout(request: &Request) -> Duration {
    match request {
        Request::Wait { timeout_ms, .. } => Duration::from_millis(u64::from(*timeout_ms) + 250),
        Request::Hello | Request::OpenCard { .. } => Duration::from_secs(1),
        Request::Frame { .. } => Duration::from_millis(250),
    }
}

struct HelperSession {
    channel: SeqpacketChannel,
    child: Child,
}

impl HelperSession {
    fn spawn(executable: File) -> Result<Self, String> {
        let (channel, peer) = SeqpacketChannel::pair().map_err(|error| error.to_string())?;
        let peer = duplicate_above_floor(peer.as_fd())?;
        let executable = duplicate_above_floor(executable.as_fd())?;
        let socket = peer.as_raw_fd();
        let parent = std::process::id() as libc::pid_t;
        let mut command = Command::new(format!("/proc/self/fd/{}", executable.as_raw_fd()));
        command
            .arg0("amaterasu-vkms-frame-helper")
            .arg("--approved-service")
            .env_clear()
            .current_dir("/")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // SAFETY: after fork this closure uses only integer-valued, async-signal-
        // safe Linux syscalls. The descriptor numbers are pinned by this scope.
        unsafe {
            command.pre_exec(move || {
                if libc::dup2(socket, HELPER_SOCKET_FD) < 0
                    || libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) < 0
                    || libc::getppid() != parent
                    || libc::syscall(
                        libc::SYS_close_range,
                        HELPER_SOCKET_FD as libc::c_uint + 1,
                        libc::c_uint::MAX,
                        libc::CLOSE_RANGE_CLOEXEC,
                    ) < 0
                {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = command.spawn().map_err(|error| error.to_string())?;
        Ok(Self { channel, child })
    }
}

impl Drop for HelperSession {
    fn drop(&mut self) {
        if let Ok(shutdown) = self.channel.shutdown_handle() {
            shutdown.shutdown();
        }
        // Never leave a privileged process alive after the session is revoked,
        // its worker exits, or either side violates the protocol.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn duplicate_above_floor(fd: std::os::fd::BorrowedFd<'_>) -> Result<OwnedFd, String> {
    // SAFETY: fcntl duplicates an open descriptor; the new fd has one owner.
    let duplicated = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 10) };
    if duplicated < 0 {
        return Err(io::Error::last_os_error().to_string());
    }
    // SAFETY: fcntl succeeded and transferred ownership of this new descriptor.
    Ok(unsafe { OwnedFd::from_raw_fd(duplicated) })
}

#[cfg(test)]
#[path = "proxy_tests.rs"]
mod tests;
