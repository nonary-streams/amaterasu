//! OS-derived pipe and process identity. No request body may assert a SID,
//! session, process creation time or service identity.
use crate::{InputError, InputResult, ProcessIdentity};
use std::ffi::c_void;
use std::ptr::{null, null_mut};

type Handle = *mut c_void;
const TOKEN_QUERY: u32 = 0x0008;
const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
const SC_MANAGER_CONNECT: u32 = 0x0001;
const SERVICE_QUERY_STATUS: u32 = 0x0004;
const TOKEN_USER: u32 = 1;
const TOKEN_SESSION_ID: u32 = 12;

#[repr(C)]
struct FileTime {
    low: u32,
    high: u32,
}

#[repr(C)]
struct ServiceStatusProcess {
    service_type: u32,
    current_state: u32,
    controls_accepted: u32,
    win32_exit_code: u32,
    service_specific_exit_code: u32,
    check_point: u32,
    wait_hint: u32,
    process_id: u32,
    service_flags: u32,
}

#[link(name = "advapi32")]
unsafe extern "system" {
    fn OpenProcessToken(process: Handle, access: u32, token: *mut Handle) -> i32;
    fn OpenThreadToken(thread: Handle, access: u32, open_as_self: i32, token: *mut Handle) -> i32;
    fn GetTokenInformation(
        token: Handle,
        class: u32,
        buffer: *mut c_void,
        length: u32,
        returned: *mut u32,
    ) -> i32;
    fn ConvertSidToStringSidW(sid: *const c_void, value: *mut *mut u16) -> i32;
    fn ImpersonateNamedPipeClient(pipe: Handle) -> i32;
    fn RevertToSelf() -> i32;
    fn OpenSCManagerW(machine: *const u16, database: *const u16, access: u32) -> Handle;
    fn OpenServiceW(manager: Handle, name: *const u16, access: u32) -> Handle;
    fn QueryServiceStatusEx(
        service: Handle,
        level: u32,
        status: *mut u8,
        size: u32,
        needed: *mut u32,
    ) -> i32;
    fn CloseServiceHandle(handle: Handle) -> i32;
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetCurrentThread() -> Handle;
    fn OpenProcess(access: u32, inherit: i32, process_id: u32) -> Handle;
    fn CloseHandle(handle: Handle) -> i32;
    fn GetProcessTimes(
        process: Handle,
        created: *mut FileTime,
        exited: *mut FileTime,
        kernel: *mut FileTime,
        user: *mut FileTime,
    ) -> i32;
    fn GetNamedPipeClientProcessId(pipe: Handle, process_id: *mut u32) -> i32;
    fn GetNamedPipeServerProcessId(pipe: Handle, process_id: *mut u32) -> i32;
    fn LocalFree(memory: Handle) -> Handle;
}

struct OwnedHandle(Handle);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: all handles in this wrapper came from OpenProcess or a token
        // opening API and are closed exactly once.
        unsafe { CloseHandle(self.0) };
    }
}

struct Impersonation;

impl Drop for Impersonation {
    fn drop(&mut self) {
        // SAFETY: this thread impersonated the pipe client successfully and
        // must revert even when token inspection fails.
        unsafe { RevertToSelf() };
    }
}

fn token_identity(token: Handle) -> InputResult<(String, u32)> {
    let mut storage = [0u64; 128];
    let mut returned = 0;
    // SAFETY: storage is aligned and its size is passed exactly. TOKEN_USER
    // begins with a pointer to a SID valid while the buffer remains alive.
    if unsafe {
        GetTokenInformation(
            token,
            TOKEN_USER,
            storage.as_mut_ptr().cast(),
            std::mem::size_of_val(&storage) as u32,
            &mut returned,
        )
    } == 0
        || returned < std::mem::size_of::<usize>() as u32
    {
        return Err(InputError::InvalidPeer);
    }
    let sid = storage[0] as usize as *const c_void;
    if sid.is_null() {
        return Err(InputError::InvalidPeer);
    }
    let mut text = null_mut();
    // SAFETY: SID points into the live TOKEN_USER buffer. Windows allocates
    // text, which is released with LocalFree after a bounded copy.
    if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 || text.is_null() {
        return Err(InputError::InvalidPeer);
    }
    let mut length = 0;
    // SAFETY: ConvertSidToStringSidW returned a NUL-terminated string. The
    // bounded scan rejects pathological output before copying it.
    while length < 256 && unsafe { *text.add(length) } != 0 {
        length += 1;
    }
    let principal = if length == 256 {
        None
    } else {
        Some(String::from_utf16_lossy(unsafe {
            std::slice::from_raw_parts(text, length)
        }))
    };
    unsafe { LocalFree(text.cast()) };
    let principal = principal.ok_or(InputError::InvalidPeer)?;

    let mut session = u32::MAX;
    returned = 0;
    // SAFETY: TokenSessionId writes exactly one u32 into live storage.
    if unsafe {
        GetTokenInformation(
            token,
            TOKEN_SESSION_ID,
            (&mut session as *mut u32).cast(),
            std::mem::size_of::<u32>() as u32,
            &mut returned,
        )
    } == 0
        || returned != 4
    {
        return Err(InputError::InvalidPeer);
    }
    Ok((principal, session))
}

pub(crate) fn inspect_process(process_id: u32) -> InputResult<ProcessIdentity> {
    // SAFETY: the PID came from the kernel pipe endpoint or trusted SCM host.
    // Only query rights are requested, and the process handle is scoped.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, process_id) };
    if process.is_null() {
        return Err(InputError::InvalidPeer);
    }
    let process = OwnedHandle(process);
    let mut token = null_mut();
    // SAFETY: token is writable, process handle remains live through the call.
    if unsafe { OpenProcessToken(process.0, TOKEN_QUERY, &mut token) } == 0 || token.is_null() {
        return Err(InputError::InvalidPeer);
    }
    let token = OwnedHandle(token);
    let (principal, session) = token_identity(token.0)?;
    let mut created = FileTime { low: 0, high: 0 };
    let mut exited = FileTime { low: 0, high: 0 };
    let mut kernel = FileTime { low: 0, high: 0 };
    let mut user = FileTime { low: 0, high: 0 };
    // SAFETY: all FILETIME outputs are writable, and process remains live.
    if unsafe { GetProcessTimes(process.0, &mut created, &mut exited, &mut kernel, &mut user) } == 0
    {
        return Err(InputError::InvalidPeer);
    }
    ProcessIdentity::new(
        principal,
        session,
        process_id,
        (u64::from(created.high) << 32) | u64::from(created.low),
    )
}

/// Call only after reading a request. Impersonation identifies the sender of
/// the last pipe message, then the kernel pipe PID and process token must agree.
pub(crate) fn pipe_peer(pipe: Handle) -> InputResult<ProcessIdentity> {
    let process = opening_process(pipe)?;
    // SAFETY: failure is checked before opening a thread token or performing
    // any privileged operation. The guard always reverts this thread.
    if unsafe { ImpersonateNamedPipeClient(pipe) } == 0 {
        return Err(InputError::InvalidPeer);
    }
    let _impersonation = Impersonation;
    let mut token = null_mut();
    if unsafe { OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &mut token) } == 0
        || token.is_null()
    {
        return Err(InputError::InvalidPeer);
    }
    let token = OwnedHandle(token);
    let observed = token_identity(token.0)?;
    drop(token);
    drop(_impersonation);
    if observed != (process.principal.clone(), process.session) {
        return Err(InputError::InvalidPeer);
    }
    Ok(process)
}

/// The opening process is available before a frame arrives, allowing the
/// worker listener to reject unpermitted clients without reserving a slot.
pub(crate) fn opening_process(pipe: Handle) -> InputResult<ProcessIdentity> {
    let mut process_id = 0;
    // SAFETY: pipe is a connected server handle; output is one writable u32.
    if unsafe { GetNamedPipeClientProcessId(pipe, &mut process_id) } == 0 {
        return Err(InputError::InvalidPeer);
    }
    inspect_process(process_id)
}

pub(crate) fn service_process_id(name: &str) -> InputResult<u32> {
    let name: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
    // SAFETY: null machine/database request local active SCM, with read-only
    // connection rights. Both handles are closed before returning.
    let manager = unsafe { OpenSCManagerW(null(), null(), SC_MANAGER_CONNECT) };
    if manager.is_null() {
        return Err(InputError::PermissionDenied);
    }
    let service = unsafe { OpenServiceW(manager, name.as_ptr(), SERVICE_QUERY_STATUS) };
    if service.is_null() {
        unsafe { CloseServiceHandle(manager) };
        return Err(InputError::PermissionDenied);
    }
    let mut status = ServiceStatusProcess {
        service_type: 0,
        current_state: 0,
        controls_accepted: 0,
        win32_exit_code: 0,
        service_specific_exit_code: 0,
        check_point: 0,
        wait_hint: 0,
        process_id: 0,
        service_flags: 0,
    };
    let mut needed = 0;
    let queried = unsafe {
        QueryServiceStatusEx(
            service,
            0,
            (&mut status as *mut ServiceStatusProcess).cast(),
            std::mem::size_of::<ServiceStatusProcess>() as u32,
            &mut needed,
        )
    };
    unsafe {
        CloseServiceHandle(service);
        CloseServiceHandle(manager);
    }
    // Amaterasu registers its first supervised worker while SCM still reports
    // START_PENDING; requiring RUNNING would deadlock service startup.
    if queried == 0 || status.process_id == 0 || !matches!(status.current_state, 2 | 4) {
        return Err(InputError::PermissionDenied);
    }
    Ok(status.process_id)
}

pub(crate) fn require_service_pipe_server(pipe: Handle) -> InputResult<()> {
    let mut process_id = 0;
    // SAFETY: pipe is an open client handle and process_id is writable.
    if unsafe { GetNamedPipeServerProcessId(pipe, &mut process_id) } == 0
        || process_id != service_process_id("AmaterasuInputCompanion")?
    {
        return Err(InputError::PermissionDenied);
    }
    Ok(())
}
