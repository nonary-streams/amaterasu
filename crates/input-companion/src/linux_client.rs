//! User-session Unix client with ordered acknowledgement and bounded native IO.
use crate::worker_client::{REPLY_TIMEOUT as CONNECT_TIMEOUT, WorkerClient, WorkerTransport};
use crate::{GamepadFeedback, InputError, InputEvent, InputResult};
use std::io::{BufRead, BufReader, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

const MAX_FRAME_BYTES: usize = 64 * 1024;
// Preserve the v1 Unix client observation window while retaining pending replies.
const REPLY_TIMEOUT: Duration = Duration::from_millis(50);

fn system_error(_: std::io::Error) -> InputError {
    InputError::Device
}

/// Worker-side client. Socket closure requests server-owned revocation; only
/// an acknowledged Close proves that the server completed native cleanup.
pub struct LinuxInputClient {
    connection: WorkerClient<UnixWorkerTransport>,
}

impl LinuxInputClient {
    pub fn connect(socket_path: &Path) -> InputResult<Self> {
        Self::connect_bounded(socket_path, None)
    }

    pub fn connect_with_deadline(socket_path: &Path, deadline: Instant) -> InputResult<Self> {
        Self::connect_bounded(socket_path, Some(deadline))
    }

    fn connect_bounded(
        socket_path: &Path,
        shutdown_deadline: Option<Instant>,
    ) -> InputResult<Self> {
        let local = Instant::now()
            .checked_add(CONNECT_TIMEOUT)
            .ok_or(InputError::Device)?;
        let deadline = shutdown_deadline.map_or(local, |shutdown| local.min(shutdown));
        let stream = connect_worker_stream(socket_path, deadline)?;
        let transport = UnixWorkerTransport {
            stream: BufReader::new(stream),
            pending: Vec::new(),
        };
        Ok(Self {
            connection: match shutdown_deadline {
                Some(deadline) => {
                    WorkerClient::connect_with_deadline(transport, REPLY_TIMEOUT, Some(deadline))?
                }
                None => WorkerClient::connect(transport, REPLY_TIMEOUT)?,
            },
        })
    }

    /// Stores the earliest host shutdown deadline without performing cleanup.
    pub fn restrict_deadline(&mut self, deadline: Instant) {
        self.connection.restrict_deadline(deadline);
    }

    /// Reports the separate gamepad bit returned for this authenticated lease.
    pub fn gamepad_enabled(&self) -> bool {
        self.connection.gamepad_enabled()
    }

    pub fn send(&mut self, event: InputEvent) -> InputResult<()> {
        self.connection.send(event)
    }

    /// Polls one local rumble event from a session-owned gamepad. This stays
    /// separate from the Moonlight protocol until a host feedback route exists.
    pub fn poll_gamepad_feedback(&mut self) -> InputResult<Option<GamepadFeedback>> {
        self.connection.poll_gamepad_feedback()
    }

    pub fn close(&mut self) -> InputResult<()> {
        self.connection.close()
    }
}

struct UnixWorkerTransport {
    stream: BufReader<UnixStream>,
    pending: Vec<u8>,
}

impl WorkerTransport for UnixWorkerTransport {
    fn write_frame(&mut self, frame: &[u8], deadline: Instant) -> InputResult<()> {
        if frame.len() > MAX_FRAME_BYTES || frame.contains(&b'\n') {
            return Err(InputError::InvalidEvent);
        }
        let mut framed = Vec::with_capacity(frame.len() + 1);
        framed.extend_from_slice(frame);
        framed.push(b'\n');
        let mut remaining = framed.as_slice();
        while !remaining.is_empty() {
            if Instant::now() >= deadline {
                return Err(InputError::Device);
            }
            match self.stream.get_mut().write(remaining) {
                Ok(0) => return Err(InputError::Closed),
                Ok(written) => remaining = &remaining[written..],
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if !wait_worker_socket(
                        self.stream.get_ref().as_raw_fd(),
                        libc::POLLOUT,
                        deadline,
                    )? {
                        return Err(InputError::Device);
                    }
                }
                Err(error) => return Err(system_error(error)),
            }
        }
        Ok(())
    }

    fn read_frame(&mut self, deadline: Instant) -> InputResult<Option<Vec<u8>>> {
        loop {
            if Instant::now() >= deadline {
                return Ok(None);
            }
            let available = match self.stream.fill_buf() {
                Ok([]) => return Err(InputError::Closed),
                Ok(available) => available,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if !wait_worker_socket(
                        self.stream.get_ref().as_raw_fd(),
                        libc::POLLIN,
                        deadline,
                    )? {
                        return Ok(None);
                    }
                    continue;
                }
                Err(error) => return Err(system_error(error)),
            };
            let end = available.iter().position(|byte| *byte == b'\n');
            let length = end.map_or(available.len(), |end| end + 1);
            if self.pending.len().saturating_add(length) > MAX_FRAME_BYTES {
                return Err(InputError::InvalidEvent);
            }
            self.pending.extend_from_slice(&available[..length]);
            self.stream.consume(length);
            if end.is_some() {
                self.pending.pop();
                return Ok(Some(std::mem::take(&mut self.pending)));
            }
        }
    }
}

fn connect_worker_stream(socket_path: &Path, deadline: Instant) -> InputResult<UnixStream> {
    if Instant::now() >= deadline {
        return Err(InputError::Device);
    }
    // SAFETY: sockaddr_un contains only integer fields and a fixed byte array;
    // zero initialization supplies the unused path bytes and terminating NUL.
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    let path = socket_path.as_os_str().as_bytes();
    if path.is_empty() || path.contains(&0) || path.len() >= address.sun_path.len() {
        return Err(InputError::InvalidEvent);
    }
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (destination, source) in address.sun_path.iter_mut().zip(path) {
        *destination = *source as libc::c_char;
    }
    // SAFETY: socket returns one owned descriptor; OwnedFd closes it on every
    // early error. NONBLOCK bounds connect and all subsequent client IO.
    let descriptor = unsafe {
        libc::socket(
            libc::AF_UNIX,
            libc::SOCK_STREAM | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
            0,
        )
    };
    if descriptor < 0 {
        return Err(system_error(std::io::Error::last_os_error()));
    }
    // SAFETY: descriptor is the newly created, uniquely owned socket above.
    let descriptor = unsafe { OwnedFd::from_raw_fd(descriptor) };
    let length = std::mem::offset_of!(libc::sockaddr_un, sun_path) + path.len() + 1;
    // SAFETY: address remains live and length includes only its initialized
    // family/path bytes. The socket is nonblocking and independently owned.
    let connected = unsafe {
        libc::connect(
            descriptor.as_raw_fd(),
            (&address as *const libc::sockaddr_un).cast(),
            length as libc::socklen_t,
        )
    };
    if connected < 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::EINPROGRESS) {
            return Err(system_error(error));
        }
        if !wait_worker_socket(descriptor.as_raw_fd(), libc::POLLOUT, deadline)? {
            return Err(InputError::Device);
        }
        let mut error = 0i32;
        let mut length = std::mem::size_of_val(&error) as libc::socklen_t;
        // SAFETY: SO_ERROR writes one integer to the live stack slot. The
        // descriptor remains owned by this function throughout the query.
        if unsafe {
            libc::getsockopt(
                descriptor.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_ERROR,
                (&mut error as *mut i32).cast(),
                &mut length,
            )
        } < 0
            || error != 0
        {
            return Err(InputError::Device);
        }
    }
    Ok(UnixStream::from(descriptor))
}

fn wait_worker_socket(
    descriptor: RawFd,
    events: libc::c_short,
    deadline: Instant,
) -> InputResult<bool> {
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(false);
        }
        let timeout = remaining
            .as_millis()
            .saturating_add(1)
            .min(i32::MAX as u128) as i32;
        let mut poll = libc::pollfd {
            fd: descriptor,
            events,
            revents: 0,
        };
        // SAFETY: poll receives one live pollfd. This function borrows rather
        // than owns the descriptor, which the calling transport retains.
        let ready = unsafe { libc::poll(&mut poll, 1, timeout) };
        if ready < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(system_error(error));
        }
        if ready > 0 {
            if poll.revents & libc::POLLNVAL != 0 {
                return Err(InputError::Closed);
            }
            return Ok(true);
        }
    }
}

#[cfg(test)]
mod tests;
