//! Local, bounded named-pipe frames. The OS process and impersonation checks
//! in `windows_identity` authorize each request after it has been read.
use crate::{InputError, InputResult};
use std::ffi::c_void;
use std::ptr::{null, null_mut};
use std::time::{Duration, Instant};

type Handle = *mut c_void;
const INVALID_HANDLE: Handle = -1isize as Handle;
const WORKER_PIPE: &str = r"\\.\pipe\AmaterasuInputCompanion-worker-v1";
const HOST_PIPE: &str = r"\\.\pipe\AmaterasuInputCompanion-host-v1";
const MAX_FRAME: usize = 4096;
const PIPE_ACCESS_DUPLEX: u32 = 3;
const FILE_FLAG_FIRST_PIPE_INSTANCE: u32 = 0x0008_0000;
const PIPE_TYPE_BYTE: u32 = 0;
const PIPE_READMODE_BYTE: u32 = 0;
const PIPE_WAIT: u32 = 0;
const PIPE_REJECT_REMOTE_CLIENTS: u32 = 0x0000_0008;
const ERROR_PIPE_CONNECTED: u32 = 535;
const ERROR_NO_DATA: u32 = 232;
// Generic write includes FILE_CREATE_PIPE_INSTANCE. Clients need only data IO.
const CLIENT_DATA_IO: u32 = 0x0010_0003;
const OPEN_EXISTING: u32 = 3;
const SECURITY_SQOS_PRESENT: u32 = 0x0010_0000;
const SECURITY_IDENTIFICATION: u32 = 0x0001_0000;

#[repr(C)]
struct SecurityAttributes {
    length: u32,
    descriptor: *mut c_void,
    inherit: i32,
}

#[link(name = "advapi32")]
unsafe extern "system" {
    fn ConvertStringSecurityDescriptorToSecurityDescriptorW(
        source: *const u16,
        revision: u32,
        descriptor: *mut Handle,
        length: *mut u32,
    ) -> i32;
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateNamedPipeW(
        name: *const u16,
        open_mode: u32,
        pipe_mode: u32,
        max_instances: u32,
        output_size: u32,
        input_size: u32,
        timeout: u32,
        security: *const SecurityAttributes,
    ) -> Handle;
    fn ConnectNamedPipe(pipe: Handle, overlapped: *mut c_void) -> i32;
    fn DisconnectNamedPipe(pipe: Handle) -> i32;
    fn CreateFileW(
        name: *const u16,
        access: u32,
        share_mode: u32,
        security: *const SecurityAttributes,
        creation: u32,
        flags: u32,
        template: Handle,
    ) -> Handle;
    fn ReadFile(
        handle: Handle,
        buffer: *mut c_void,
        length: u32,
        read: *mut u32,
        overlapped: *mut c_void,
    ) -> i32;
    fn WriteFile(
        handle: Handle,
        buffer: *const c_void,
        length: u32,
        written: *mut u32,
        overlapped: *mut c_void,
    ) -> i32;
    fn PeekNamedPipe(
        pipe: Handle,
        buffer: *mut c_void,
        length: u32,
        read: *mut u32,
        available: *mut u32,
        remaining: *mut u32,
    ) -> i32;
    fn CloseHandle(handle: Handle) -> i32;
    fn LocalFree(memory: Handle) -> Handle;
    fn GetLastError() -> u32;
}

pub(crate) struct Pipe {
    handle: Handle,
    server: bool,
    pending: Vec<u8>,
}

#[derive(Clone, Copy)]
pub(crate) enum PipeKind {
    Host,
    Worker,
}

impl PipeKind {
    fn name(self) -> &'static str {
        match self {
            Self::Host => HOST_PIPE,
            Self::Worker => WORKER_PIPE,
        }
    }
}

// Windows HANDLEs are process-global references. Each Pipe is moved to one
// connection thread and is never concurrently used by two threads.
unsafe impl Send for Pipe {}

impl Drop for Pipe {
    fn drop(&mut self) {
        // SAFETY: a successful create/open returned this owned handle. The
        // server end is disconnected before its final close.
        unsafe {
            if self.server {
                DisconnectNamedPipe(self.handle);
            }
            CloseHandle(self.handle);
        }
    }
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}

impl Pipe {
    pub(crate) fn listen(kind: PipeKind, first: bool) -> InputResult<Self> {
        // Explicit DACL: SYSTEM/Administrators may own the server; only
        // interactive local users may open a client connection. The pipe is
        // still subject to per-request OS identity and host-grant checks.
        let sddl = wide(match kind {
            PipeKind::Host => "D:P(A;;GA;;;SY)(A;;GA;;;BA)",
            PipeKind::Worker => "D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;0x00100003;;;IU)",
        });
        let mut descriptor = null_mut();
        let mut length = 0;
        // SAFETY: all buffers are live NUL-terminated UTF-16 and descriptor
        // is an out pointer released with LocalFree after pipe creation.
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                1,
                &mut descriptor,
                &mut length,
            )
        } == 0
            || descriptor.is_null()
        {
            return Err(InputError::PermissionDenied);
        }
        let security = SecurityAttributes {
            length: std::mem::size_of::<SecurityAttributes>() as u32,
            descriptor,
            inherit: 0,
        };
        let flags = PIPE_ACCESS_DUPLEX
            | if first {
                FILE_FLAG_FIRST_PIPE_INSTANCE
            } else {
                0
            };
        let handle = unsafe {
            CreateNamedPipeW(
                wide(kind.name()).as_ptr(),
                flags,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                16,
                MAX_FRAME as u32,
                MAX_FRAME as u32,
                0,
                &security,
            )
        };
        unsafe { LocalFree(descriptor) };
        if handle == INVALID_HANDLE {
            return Err(InputError::Device);
        }
        Ok(Self {
            handle,
            server: true,
            pending: Vec::new(),
        })
    }

    pub(crate) fn accept(&self) -> InputResult<bool> {
        // SAFETY: the handle is a live server pipe instance. The only benign
        // failures are an already-connected client or one that disconnected
        // before accept. The latter must not terminate the service.
        if unsafe { ConnectNamedPipe(self.handle, null_mut()) } == 0 {
            return match unsafe { GetLastError() } {
                ERROR_PIPE_CONNECTED => Ok(true),
                ERROR_NO_DATA => Ok(false),
                _ => Err(InputError::Device),
            };
        }
        Ok(true)
    }

    pub(crate) fn connect(kind: PipeKind) -> InputResult<Self> {
        let handle = unsafe {
            CreateFileW(
                wide(kind.name()).as_ptr(),
                CLIENT_DATA_IO,
                0,
                null(),
                OPEN_EXISTING,
                SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
                null_mut(),
            )
        };
        if handle == INVALID_HANDLE {
            return Err(InputError::Device);
        }
        let pipe = Self {
            handle,
            server: false,
            pending: Vec::new(),
        };
        crate::windows_identity::require_service_pipe_server(handle)?;
        Ok(pipe)
    }

    pub(crate) fn raw(&self) -> Handle {
        self.handle
    }

    pub(crate) fn write_line(&self, bytes: &[u8]) -> InputResult<()> {
        if bytes.len() >= MAX_FRAME || bytes.contains(&b'\n') {
            return Err(InputError::InvalidEvent);
        }
        let mut frame = Vec::with_capacity(bytes.len() + 1);
        frame.extend_from_slice(bytes);
        frame.push(b'\n');
        let mut written = 0;
        // SAFETY: frame is live until synchronous WriteFile returns.
        if unsafe {
            WriteFile(
                self.handle,
                frame.as_ptr().cast(),
                frame.len() as u32,
                &mut written,
                null_mut(),
            )
        } == 0
            || written as usize != frame.len()
        {
            return Err(InputError::Device);
        }
        Ok(())
    }

    pub(crate) fn read_line(&mut self, deadline: Instant) -> InputResult<Option<Vec<u8>>> {
        loop {
            if let Some(end) = self.pending.iter().position(|byte| *byte == b'\n') {
                let line = self.pending.drain(..=end).collect::<Vec<_>>();
                return Ok(Some(line[..line.len() - 1].to_vec()));
            }
            if self.pending.len() >= MAX_FRAME {
                return Err(InputError::InvalidEvent);
            }
            let mut available = 0;
            // SAFETY: PeekNamedPipe uses no data buffer and writes one u32.
            if unsafe {
                PeekNamedPipe(
                    self.handle,
                    null_mut(),
                    0,
                    null_mut(),
                    &mut available,
                    null_mut(),
                )
            } == 0
            {
                return Err(InputError::Device);
            }
            if available == 0 {
                if Instant::now() >= deadline {
                    return Ok(None);
                }
                std::thread::sleep(Duration::from_millis(10));
                continue;
            }
            let mut buffer = [0u8; 512];
            let mut read = 0;
            let amount = available.min(buffer.len() as u32);
            // SAFETY: available bytes were observed immediately before the
            // synchronous read; the buffer remains live and is bounded.
            if unsafe {
                ReadFile(
                    self.handle,
                    buffer.as_mut_ptr().cast(),
                    amount,
                    &mut read,
                    null_mut(),
                )
            } == 0
                || read == 0
            {
                return Err(InputError::Device);
            }
            self.pending.extend_from_slice(&buffer[..read as usize]);
        }
    }
}
