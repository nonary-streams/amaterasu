//! Local, bounded named-pipe frames. The OS process and impersonation checks
//! in `windows_identity` authorize each request after it has been read.
use crate::windows_pipe_state::{
    IoKind, IoResult, MAX_FRAME, OwnedIo, RetiredIo, remaining_wait_ms, take_frame,
};
use crate::{InputError, InputResult};
use std::cell::{RefCell, UnsafeCell};
use std::ffi::c_void;
use std::ptr::{null, null_mut};
use std::time::{Duration, Instant};
use windows_sys::Win32::System::IO::OVERLAPPED;

type Handle = *mut c_void;
const INVALID_HANDLE: Handle = -1isize as Handle;
const WORKER_PIPE: &str = r"\\.\pipe\AmaterasuInputCompanion-worker-v1";
const HOST_PIPE: &str = r"\\.\pipe\AmaterasuInputCompanion-host-v1";
const PIPE_ACCESS_DUPLEX: u32 = 3;
const FILE_FLAG_FIRST_PIPE_INSTANCE: u32 = 0x0008_0000;
const FILE_FLAG_OVERLAPPED: u32 = 0x4000_0000;
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
const ERROR_OPERATION_ABORTED: u32 = 995;
const ERROR_IO_INCOMPLETE: u32 = 996;
const ERROR_IO_PENDING: u32 = 997;
const WAIT_OBJECT_0: u32 = 0;
const WAIT_TIMEOUT: u32 = 258;

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
    fn CreateEventW(
        security: *const SecurityAttributes,
        manual_reset: i32,
        initial_state: i32,
        name: *const u16,
    ) -> Handle;
    fn WaitForSingleObject(handle: Handle, milliseconds: u32) -> u32;
    fn CancelIoEx(handle: Handle, overlapped: *mut OVERLAPPED) -> i32;
    fn GetOverlappedResult(
        handle: Handle,
        overlapped: *mut OVERLAPPED,
        transferred: *mut u32,
        wait: i32,
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
    io: RefCell<PipeIo>,
}

#[derive(Default)]
struct PipeIo {
    buffered: Vec<u8>,
    operation: Option<PendingIo>,
    failed: bool,
}

/// One exact kernel operation owns its stable allocation and completion event.
struct PendingIo {
    handle: Handle,
    event: Handle,
    // The kernel mutates OVERLAPPED's internal fields while pending. Only raw
    // pointers from UnsafeCell may access it until terminal completion; stable
    // allocation alone would not justify Rust references to those contents.
    overlapped: Box<UnsafeCell<OVERLAPPED>>,
    operation: Option<OwnedIo>,
}

impl PendingIo {
    fn begin(handle: Handle, kind: IoKind, bytes: Vec<u8>) -> InputResult<Self> {
        // SAFETY: a private manual-reset event has no inherited handle or callback.
        let event = unsafe { CreateEventW(null(), 1, 0, null()) };
        if event.is_null() {
            return Err(InputError::Device);
        }
        // SAFETY: zero is the documented initial OVERLAPPED state. Its offsets
        // are unused by named pipes; the private event belongs to this operation.
        let mut overlapped = unsafe { std::mem::zeroed::<OVERLAPPED>() };
        overlapped.hEvent = event;
        let mut pending = Self {
            handle,
            event,
            overlapped: Box::new(UnsafeCell::new(overlapped)),
            operation: Some(OwnedIo::new(kind, bytes)),
        };
        let operation = pending.operation.as_mut().expect("new operation");
        operation.start()?;
        let (buffer, length) = operation.buffer();
        // SAFETY: the boxed OVERLAPPED and owned buffer do not move or change
        // allocation until terminal completion, including after cancellation.
        // The Pipe keeps its handle live through this operation's retirement.
        let completed = unsafe {
            match kind {
                IoKind::Read => ReadFile(
                    handle,
                    buffer.cast(),
                    length as u32,
                    null_mut(),
                    pending.overlapped.get().cast(),
                ),
                IoKind::Write => WriteFile(
                    handle,
                    buffer.cast(),
                    length as u32,
                    null_mut(),
                    pending.overlapped.get().cast(),
                ),
            }
        };
        if completed != 0 {
            // Even an immediate asynchronous completion takes its actual byte
            // count from this operation, never a synchronous output parameter.
            pending.poll_completion();
        } else {
            // SAFETY: read immediately after the failed I/O call on this thread.
            let error = unsafe { GetLastError() };
            if error != ERROR_IO_PENDING {
                pending.complete(io_failure(error));
            }
        }
        Ok(pending)
    }

    fn complete(&mut self, result: IoResult) {
        if let Some(operation) = &mut self.operation {
            operation.complete(result);
        }
    }

    fn is_pending(&self) -> bool {
        self.operation.as_ref().is_some_and(OwnedIo::is_pending)
    }

    fn poll_completion(&mut self) -> bool {
        self.observe_completion(false)
    }

    fn observe_completion(&mut self, wait: bool) -> bool {
        if !self.is_pending() {
            return true;
        }
        let mut transferred = 0;
        // SAFETY: the exact operation, event, buffer and pipe handle remain
        // owned while GetOverlappedResult observes this operation's result.
        if unsafe {
            GetOverlappedResult(
                self.handle,
                self.overlapped.get(),
                &mut transferred,
                i32::from(wait),
            )
        } != 0
        {
            self.complete(IoResult::Transferred(transferred as usize));
            return true;
        }
        // SAFETY: this thread has made no intervening Win32 call.
        let error = unsafe { GetLastError() };
        if error == ERROR_IO_INCOMPLETE || error == ERROR_IO_PENDING {
            return false;
        }
        self.complete(io_failure(error));
        true
    }

    fn cancel(&mut self) {
        if self
            .operation
            .as_mut()
            .is_some_and(OwnedIo::request_cancellation)
        {
            // SAFETY: cancellation names this exact, still-owned operation.
            // Neither success nor ERROR_NOT_FOUND proves retirement; its final
            // result must still be observed before reclaiming the allocations.
            unsafe { CancelIoEx(self.handle, self.overlapped.get()) };
        }
    }

    fn wait_before(&mut self, deadline: Instant) -> InputResult<bool> {
        loop {
            let Some(milliseconds) = remaining_wait_ms(Instant::now(), deadline) else {
                self.cancel();
                self.poll_completion();
                return Ok(false);
            };
            if self.poll_completion() {
                return Ok(true);
            }
            // SAFETY: this operation exclusively owns the live completion event.
            match unsafe { WaitForSingleObject(self.event, milliseconds) } {
                WAIT_OBJECT_0 | WAIT_TIMEOUT => {}
                _ => {
                    self.cancel();
                    self.poll_completion();
                    return Err(InputError::Device);
                }
            }
        }
    }

    fn take_retired(&mut self) -> Option<RetiredIo> {
        let operation = self.operation.take()?;
        match operation.into_retired() {
            Ok(retired) => Some(retired),
            Err(operation) => {
                self.operation = Some(operation);
                None
            }
        }
    }
}

impl Drop for PendingIo {
    fn drop(&mut self) {
        self.cancel();
        // Forced destruction is a containment boundary, not another successful
        // cooperative wait. A stalled kernel cancellation may exceed the host
        // budget; the owning host worker must remain loaded or terminate as a
        // whole process. Returning while the kernel borrows this memory is unsafe.
        while !self.observe_completion(true) {}
        // SAFETY: terminal completion has retired all references to the event,
        // OVERLAPPED and buffer. The owning Pipe has not yet closed its handle.
        unsafe { CloseHandle(self.event) };
    }
}

fn io_failure(error: u32) -> IoResult {
    if error == ERROR_OPERATION_ABORTED {
        IoResult::Aborted
    } else {
        IoResult::Failed
    }
}

impl PipeIo {
    fn take_completed(&mut self) -> Option<RetiredIo> {
        let operation = self.operation.as_mut()?;
        if !operation.poll_completion() {
            return None;
        }
        let completed = operation.take_retired()?;
        self.operation = None;
        Some(completed)
    }

    fn retain_read(&mut self, completed: RetiredIo) -> InputResult<()> {
        match (completed.kind, completed.result) {
            (IoKind::Read, IoResult::Transferred(length)) if length != 0 => {
                self.buffered.extend_from_slice(&completed.bytes[..length]);
                Ok(())
            }
            (IoKind::Read, IoResult::Aborted) => Ok(()),
            _ => {
                self.failed = true;
                Err(InputError::Device)
            }
        }
    }
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
        // PendingIo's forced containment wait keeps the exact handle live until
        // the kernel has relinquished its buffer, OVERLAPPED and event borrows.
        drop(self.io.get_mut().operation.take());
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
            io: RefCell::new(PipeIo::default()),
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
                SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION | FILE_FLAG_OVERLAPPED,
                null_mut(),
            )
        };
        if handle == INVALID_HANDLE {
            return Err(InputError::Device);
        }
        let pipe = Self {
            handle,
            server: false,
            io: RefCell::new(PipeIo::default()),
        };
        crate::windows_identity::require_service_pipe_server(handle)?;
        Ok(pipe)
    }

    pub(crate) fn raw(&self) -> Handle {
        self.handle
    }

    pub(crate) fn write_line(&self, bytes: &[u8]) -> InputResult<()> {
        if !self.server {
            let deadline = Instant::now()
                .checked_add(crate::worker_client::REPLY_TIMEOUT)
                .ok_or(InputError::Device)?;
            return self.write_line_before(bytes, deadline);
        }
        self.write_server_line(bytes)
    }

    fn frame(bytes: &[u8]) -> InputResult<Vec<u8>> {
        if bytes.len() >= MAX_FRAME || bytes.contains(&b'\n') {
            return Err(InputError::InvalidEvent);
        }
        let mut frame = Vec::with_capacity(bytes.len() + 1);
        frame.extend_from_slice(bytes);
        frame.push(b'\n');
        Ok(frame)
    }

    pub(crate) fn write_line_before(&self, bytes: &[u8], deadline: Instant) -> InputResult<()> {
        let frame = Self::frame(bytes)?;
        if self.server || Instant::now() >= deadline {
            return Err(InputError::Device);
        }
        let mut io = self.io.borrow_mut();
        if let Some(completed) = io.take_completed() {
            io.retain_read(completed)?;
        }
        if io.failed || io.operation.is_some() {
            return Err(InputError::Device);
        }
        let length = frame.len();
        io.operation = Some(PendingIo::begin(self.handle, IoKind::Write, frame)?);
        let completed_before_deadline = io
            .operation
            .as_mut()
            .expect("owned write")
            .wait_before(deadline);
        let completed = io.take_completed();
        if completed_before_deadline == Ok(true)
            && completed
                .as_ref()
                .is_some_and(|completed| completed.result == IoResult::Transferred(length))
        {
            return Ok(());
        }
        // A cancelled or partial write may have delivered a prefix. Preserve any
        // outstanding kernel borrow and fence this byte stream against reuse.
        io.failed = true;
        Err(InputError::Device)
    }

    fn write_server_line(&self, bytes: &[u8]) -> InputResult<()> {
        let frame = Self::frame(bytes)?;
        let mut written = 0;
        // SAFETY: only server handles use synchronous I/O; frame remains live
        // until WriteFile returns. Every overlapped client path uses PendingIo.
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
        if self.server {
            return self.read_server_line(deadline);
        }
        let mut io = self.io.borrow_mut();
        loop {
            if let Some(completed) = io.take_completed() {
                io.retain_read(completed)?;
            }
            if io.failed {
                return Err(InputError::Device);
            }
            if let Some(frame) = take_frame(&mut io.buffered)? {
                return Ok(Some(frame));
            }
            if Instant::now() >= deadline {
                if let Some(operation) = &mut io.operation {
                    operation.cancel();
                }
                return Ok(None);
            }
            if io.operation.is_none() {
                let length = 512.min(MAX_FRAME - io.buffered.len());
                io.operation = Some(PendingIo::begin(
                    self.handle,
                    IoKind::Read,
                    vec![0; length],
                )?);
            }
            let completed_before_deadline = io
                .operation
                .as_mut()
                .expect("owned read")
                .wait_before(deadline)?;
            if let Some(completed) = io.take_completed() {
                io.retain_read(completed)?;
            }
            if !completed_before_deadline {
                // Bytes from a completion/cancellation race remain buffered for
                // the next observation of this same outstanding wire request.
                return Ok(None);
            }
        }
    }

    fn read_server_line(&mut self, deadline: Instant) -> InputResult<Option<Vec<u8>>> {
        loop {
            if let Some(frame) = take_frame(&mut self.io.get_mut().buffered)? {
                return Ok(Some(frame));
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
                std::thread::sleep(
                    Duration::from_millis(10)
                        .min(deadline.saturating_duration_since(Instant::now())),
                );
                continue;
            }
            let mut buffer = [0u8; 512];
            let mut read = 0;
            let amount = available
                .min(buffer.len() as u32)
                .min((MAX_FRAME - self.io.get_mut().buffered.len()) as u32);
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
            self.io
                .get_mut()
                .buffered
                .extend_from_slice(&buffer[..read as usize]);
        }
    }
}
