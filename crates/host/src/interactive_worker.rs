//! Interactive-user worker launcher for the Windows SCM service.
//!
//! The service itself remains in Session 0 to retain SCM ownership. Loaded
//! components run only in an active WTS user session, using a primary token
//! duplicated with the rights needed for process creation. No token, user
//! environment, or service handle is passed to the worker after launch.

use amaterasu_package::PackageResult;
use amaterasu_package::playnite_extension::{
    PlayniteExtensionInstall, PlayniteExtensionReceipt, finalize_playnite_extension,
    install_playnite_extension, pending_playnite_extension_receipt, rollback_playnite_extension,
};
use std::ffi::{OsStr, OsString, c_void};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf, Prefix};

const WTS_ACTIVE: i32 = 0;
const INVALID_SESSION_ID: u32 = u32::MAX;
const WAIT_OBJECT_0: u32 = 0;
const WAIT_TIMEOUT: u32 = 258;
const TOKEN_ASSIGN_PRIMARY: u32 = 0x0001;
const TOKEN_DUPLICATE: u32 = 0x0002;
const TOKEN_QUERY: u32 = 0x0008;
const TOKEN_USER: u32 = 1;
const OWNER_SECURITY_INFORMATION: u32 = 0x0000_0001;
const SE_FILE_OBJECT: u32 = 1;
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
const INVALID_FILE_ATTRIBUTES: u32 = u32::MAX;
const TOKEN_ADJUST_DEFAULT: u32 = 0x0080;
const TOKEN_ADJUST_SESSIONID: u32 = 0x0100;
const SECURITY_IMPERSONATION: u32 = 2;
const TOKEN_PRIMARY: u32 = 1;
const CREATE_UNICODE_ENVIRONMENT: u32 = 0x0000_0400;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const EXTENDED_STARTUPINFO_PRESENT: u32 = 0x0008_0000;
const STARTF_USESTDHANDLES: u32 = 0x0000_0100;
const GENERIC_READ: u32 = 0x8000_0000;
const GENERIC_WRITE: u32 = 0x4000_0000;
const OPEN_EXISTING: u32 = 3;
const FILE_ATTRIBUTE_NORMAL: u32 = 0x80;
const MAX_START_FAILURES: usize = 3;
const HANDLE_FLAG_INHERIT: u32 = 0x0000_0001;
const PROC_THREAD_ATTRIBUTE_HANDLE_LIST: usize = 0x0002_0002;
const STARTUP_READY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

#[repr(C)]
struct SecurityAttributes {
    length: u32,
    security_descriptor: *mut c_void,
    inherit_handle: i32,
}

#[repr(C)]
struct StartupInfoW {
    cb: u32,
    reserved: *mut u16,
    desktop: *mut u16,
    title: *mut u16,
    x: u32,
    y: u32,
    x_size: u32,
    y_size: u32,
    x_count_chars: u32,
    y_count_chars: u32,
    fill_attribute: u32,
    flags: u32,
    show_window: u16,
    reserved2_count: u16,
    reserved2: *mut u8,
    standard_input: *mut c_void,
    standard_output: *mut c_void,
    standard_error: *mut c_void,
}

#[repr(C)]
struct StartupInfoExW {
    startup: StartupInfoW,
    attribute_list: *mut c_void,
}

#[repr(C)]
struct ProcessInformation {
    process: *mut c_void,
    thread: *mut c_void,
    process_id: u32,
    thread_id: u32,
}

#[repr(C)]
struct WtsSessionInfoW {
    session_id: u32,
    station_name: *mut u16,
    state: i32,
}

#[repr(C)]
struct SidAndAttributes {
    sid: *mut c_void,
    attributes: u32,
}

#[link(name = "advapi32")]
unsafe extern "system" {
    fn GetTokenInformation(
        token: *mut c_void,
        class: u32,
        buffer: *mut c_void,
        length: u32,
        returned: *mut u32,
    ) -> i32;
    fn GetNamedSecurityInfoW(
        object: *const u16,
        object_type: u32,
        information: u32,
        owner: *mut *mut c_void,
        group: *mut *mut c_void,
        dacl: *mut *mut c_void,
        sacl: *mut *mut c_void,
        descriptor: *mut *mut c_void,
    ) -> u32;
    fn EqualSid(left: *const c_void, right: *const c_void) -> i32;
    fn ConvertSidToStringSidW(sid: *mut c_void, text: *mut *mut u16) -> i32;
    fn DuplicateTokenEx(
        existing: *mut c_void,
        desired_access: u32,
        attributes: *const SecurityAttributes,
        impersonation: u32,
        token_type: u32,
        duplicated: *mut *mut c_void,
    ) -> i32;
    fn CreateProcessAsUserW(
        token: *mut c_void,
        application_name: *const u16,
        command_line: *mut u16,
        process_attributes: *const SecurityAttributes,
        thread_attributes: *const SecurityAttributes,
        inherit_handles: i32,
        creation_flags: u32,
        environment: *const c_void,
        current_directory: *const u16,
        startup: *const StartupInfoW,
        process: *mut ProcessInformation,
    ) -> i32;
}

#[link(name = "wtsapi32")]
unsafe extern "system" {
    fn WTSEnumerateSessionsW(
        server: *mut c_void,
        reserved: u32,
        version: u32,
        sessions: *mut *mut WtsSessionInfoW,
        count: *mut u32,
    ) -> i32;
    fn WTSFreeMemory(memory: *mut c_void);
    fn WTSGetActiveConsoleSessionId() -> u32;
    fn WTSQueryUserToken(session_id: u32, token: *mut *mut c_void) -> i32;
}

#[link(name = "userenv")]
unsafe extern "system" {
    fn GetUserProfileDirectoryW(token: *mut c_void, directory: *mut u16, length: *mut u32) -> i32;
    fn CreateEnvironmentBlock(
        environment: *mut *mut c_void,
        token: *mut c_void,
        inherit: i32,
    ) -> i32;
    fn DestroyEnvironmentBlock(environment: *mut c_void) -> i32;
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn LocalFree(memory: *mut c_void) -> *mut c_void;
    fn GetFileAttributesW(path: *const u16) -> u32;
    fn CreateFileW(
        path: *const u16,
        desired_access: u32,
        share_mode: u32,
        attributes: *mut SecurityAttributes,
        creation: u32,
        flags: u32,
        template: *mut c_void,
    ) -> *mut c_void;
    fn CloseHandle(handle: *mut c_void) -> i32;
    fn WaitForSingleObject(handle: *mut c_void, milliseconds: u32) -> u32;
    fn TerminateProcess(process: *mut c_void, exit_code: u32) -> i32;
    fn CreatePipe(
        read_pipe: *mut *mut c_void,
        write_pipe: *mut *mut c_void,
        attributes: *mut SecurityAttributes,
        size: u32,
    ) -> i32;
    fn SetHandleInformation(handle: *mut c_void, mask: u32, flags: u32) -> i32;
    fn PeekNamedPipe(
        pipe: *mut c_void,
        buffer: *mut c_void,
        bytes_to_read: u32,
        bytes_read: *mut u32,
        total_bytes_available: *mut u32,
        bytes_left_this_message: *mut u32,
    ) -> i32;
    fn ReadFile(
        handle: *mut c_void,
        buffer: *mut c_void,
        bytes_to_read: u32,
        bytes_read: *mut u32,
        overlapped: *mut c_void,
    ) -> i32;
    fn InitializeProcThreadAttributeList(
        list: *mut c_void,
        attribute_count: u32,
        flags: u32,
        size: *mut usize,
    ) -> i32;
    fn UpdateProcThreadAttribute(
        list: *mut c_void,
        flags: u32,
        attribute: usize,
        value: *const c_void,
        size: usize,
        previous: *mut c_void,
        return_size: *mut usize,
    ) -> i32;
    fn DeleteProcThreadAttributeList(list: *mut c_void);
}

/// Supervises one worker tied to the currently active interactive session.
pub(crate) struct Supervisor<'a> {
    root: &'a Path,
    digest: &'a str,
    worker: Option<Worker>,
    failed_starts: usize,
    observed_session: Option<u32>,
}

impl<'a> Supervisor<'a> {
    pub(crate) fn new(root: &'a Path, digest: &'a str) -> PackageResult<Self> {
        let mut supervisor = Self {
            root,
            digest,
            worker: None,
            failed_starts: 0,
            observed_session: None,
        };
        supervisor.refresh(true)?;
        Ok(supervisor)
    }

    /// Stops a worker as soon as its user session is no longer active. Polling
    /// also closes the race where SCM does not deliver a session-change event.
    pub(crate) fn refresh(&mut self, session_changed: bool) -> PackageResult<()> {
        let active = active_user_session()?;
        let session_changed_by_polling = active != self.observed_session;
        self.observed_session = active;
        if self
            .worker
            .as_ref()
            .is_some_and(|worker| active != Some(worker.session_id))
        {
            self.worker = None;
            self.failed_starts = 0;
        }
        if let Some(worker) = &self.worker
            && worker.has_exited()?
        {
            self.worker = None;
            self.failed_starts += 1;
        }
        if self.worker.is_none()
            && let Some(session_id) = active
            && (session_changed || session_changed_by_polling || self.failed_starts > 0)
        {
            match Worker::start(self.root, self.digest, session_id) {
                Ok(worker) => {
                    self.worker = Some(worker);
                    self.failed_starts = 0;
                }
                Err(_) => self.failed_starts += 1,
            }
        }
        if self.failed_starts >= MAX_START_FAILURES {
            return Err("interactive worker restart budget exhausted".into());
        }
        Ok(())
    }

    /// A readiness marker may be published only after a selected interactive
    /// session owns a live worker that completed its component startup check.
    pub(crate) fn has_ready_worker(&self) -> PackageResult<bool> {
        self.worker
            .as_ref()
            .map(|worker| {
                worker
                    .is_alive()
                    .map(|alive| alive && self.observed_session == Some(worker.session_id))
            })
            .transpose()
            .map(|ready| ready.unwrap_or(false))
    }

    pub(crate) fn shutdown(mut self) -> PackageResult<()> {
        self.worker = None;
        Ok(())
    }
}

/// A process handle and the WTS session that owns its desktop.
struct Worker {
    process: *mut c_void,
    session_id: u32,
}

impl Worker {
    fn start(root: &Path, digest: &str, session_id: u32) -> PackageResult<Self> {
        let user_token = UserToken::for_session(session_id)?;
        let prepared = crate::state_root::prepare(root, digest, user_token.handle)?;
        let primary_token = user_token.duplicate_primary()?;
        let environment = UserEnvironment::for_token(primary_token.handle)?;
        let executable = std::env::current_exe().map_err(|error| error.to_string())?;
        let extension_install = match prepare_playnite_extension(user_token.handle, &executable) {
            Ok(install) => install,
            Err(error) => {
                eprintln!("Playnite extension installation skipped: {error}");
                None
            }
        };
        let null_device = InheritableNull::open()?;
        let mut readiness = ReadinessPipe::open()?;
        let application_name = wide_os(executable.as_os_str())?;
        let mut arguments = vec![
            executable.as_os_str(),
            OsStr::new("worker-user-serve"),
            root.as_os_str(),
            OsStr::new(digest),
            prepared.root.as_os_str(),
        ];
        if let Some(seed) = &prepared.seed {
            arguments.push(seed.as_os_str());
        }
        let mut command_line = command_line(&arguments)?;
        let mut desktop: Vec<u16> = "winsta0\\default".encode_utf16().chain(Some(0)).collect();
        let allowed_handles = [null_device.handle, readiness.write_handle()];
        let attributes = StartupAttributeList::only_handles(&allowed_handles)?;
        let startup = StartupInfoExW {
            startup: StartupInfoW {
                cb: std::mem::size_of::<StartupInfoExW>() as u32,
                reserved: std::ptr::null_mut(),
                desktop: desktop.as_mut_ptr(),
                title: std::ptr::null_mut(),
                x: 0,
                y: 0,
                x_size: 0,
                y_size: 0,
                x_count_chars: 0,
                y_count_chars: 0,
                fill_attribute: 0,
                flags: STARTF_USESTDHANDLES,
                show_window: 0,
                reserved2_count: 0,
                reserved2: std::ptr::null_mut(),
                standard_input: null_device.handle,
                standard_output: readiness.write_handle(),
                standard_error: null_device.handle,
            },
            attribute_list: attributes.pointer(),
        };
        let mut process = ProcessInformation {
            process: std::ptr::null_mut(),
            thread: std::ptr::null_mut(),
            process_id: 0,
            thread_id: 0,
        };
        // SAFETY: application_name and command_line are NUL-terminated UTF-16;
        // command_line is mutable as required by CreateProcessAsUserW. Scoped
        // token, environment, desktop, and standard-handle storage remain live
        // for the call. The explicit attribute list restricts inheritance to
        // the NUL device and one write-only readiness pipe; no other SYSTEM
        // handle can reach the interactive user process.
        let created = unsafe {
            CreateProcessAsUserW(
                primary_token.handle,
                application_name.as_ptr(),
                command_line.as_mut_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                1,
                CREATE_UNICODE_ENVIRONMENT | CREATE_NO_WINDOW | EXTENDED_STARTUPINFO_PRESENT,
                environment.pointer,
                std::ptr::null(),
                &startup.startup,
                &mut process,
            )
        };
        if created == 0 || process.process.is_null() || process.thread.is_null() {
            return Err("interactive worker process could not be created".into());
        }
        // SAFETY: CreateProcessAsUserW returned an independently owned thread
        // handle. The retained process handle has the worker's full lifetime.
        unsafe { CloseHandle(process.thread) };
        readiness.close_write();
        let worker = Self {
            process: process.process,
            session_id,
        };
        if let Err(error) = readiness.wait_for_ready(&worker) {
            drop(worker);
            return Err(error);
        }
        if let Some(install) = extension_install
            && let Err(error) = install.finalize()
        {
            eprintln!("Playnite extension installation could not finalize: {error}");
        }
        Ok(worker)
    }

    fn has_exited(&self) -> PackageResult<bool> {
        // SAFETY: this wrapper retains the process handle until Drop closes it.
        match unsafe { WaitForSingleObject(self.process, 0) } {
            WAIT_TIMEOUT => Ok(false),
            WAIT_OBJECT_0 => Ok(true),
            _ => Err("interactive worker status is unavailable".into()),
        }
    }

    fn is_alive(&self) -> PackageResult<bool> {
        self.has_exited().map(|exited| !exited)
    }
}

/// One inherited write handle carries a bounded, one-way startup proof from
/// the unprivileged worker to its Session 0 parent. The parent end is marked
/// non-inheritable before CreateProcessAsUserW, so the user process cannot
/// receive or retain any service-owned handles besides its own stdout pipe.
struct ReadinessPipe {
    read: *mut c_void,
    write: *mut c_void,
}

impl ReadinessPipe {
    fn open() -> PackageResult<Self> {
        let mut attributes = SecurityAttributes {
            length: std::mem::size_of::<SecurityAttributes>() as u32,
            security_descriptor: std::ptr::null_mut(),
            inherit_handle: 1,
        };
        let mut read = std::ptr::null_mut();
        let mut write = std::ptr::null_mut();
        // SAFETY: output pointers and attributes stay valid through the call.
        if unsafe { CreatePipe(&mut read, &mut write, &mut attributes, 0) } == 0
            || read.is_null()
            || write.is_null()
        {
            return Err("interactive worker readiness pipe could not be created".into());
        }
        // SAFETY: `read` is our parent endpoint; clearing inheritance prevents
        // CreateProcessAsUserW from leaking it into the interactive process.
        if unsafe { SetHandleInformation(read, HANDLE_FLAG_INHERIT, 0) } == 0 {
            unsafe {
                let _ = CloseHandle(read);
                let _ = CloseHandle(write);
            }
            return Err("interactive worker readiness pipe could not be secured".into());
        }
        Ok(Self { read, write })
    }

    fn write_handle(&self) -> *mut c_void {
        self.write
    }

    fn close_write(&mut self) {
        if !self.write.is_null() {
            // SAFETY: the parent owns this duplicate; the child inherited its
            // own write endpoint during process creation.
            unsafe {
                let _ = CloseHandle(self.write);
            }
            self.write = std::ptr::null_mut();
        }
    }

    fn wait_for_ready(&mut self, worker: &Worker) -> PackageResult<()> {
        let deadline = std::time::Instant::now() + STARTUP_READY_TIMEOUT;
        let mut message = Vec::new();
        while std::time::Instant::now() < deadline {
            if worker.has_exited()? {
                return Err("interactive worker exited before readiness".into());
            }
            let mut available = 0;
            // SAFETY: `read` remains valid and the null buffer asks only for
            // the available-byte count; no untrusted pointer is dereferenced.
            if unsafe {
                PeekNamedPipe(
                    self.read,
                    std::ptr::null_mut(),
                    0,
                    std::ptr::null_mut(),
                    &mut available,
                    std::ptr::null_mut(),
                )
            } == 0
            {
                return Err("interactive worker readiness pipe failed".into());
            }
            if available != 0 {
                let size = available.min(64) as usize;
                let mut bytes = vec![0; size];
                let mut read = 0;
                // SAFETY: `bytes` has exactly `size` writable bytes and this
                // synchronous pipe endpoint is retained by this wrapper.
                if unsafe {
                    ReadFile(
                        self.read,
                        bytes.as_mut_ptr().cast(),
                        size as u32,
                        &mut read,
                        std::ptr::null_mut(),
                    )
                } == 0
                {
                    return Err("interactive worker readiness pipe failed".into());
                }
                message.extend_from_slice(&bytes[..read as usize]);
                if message == b"READY\n" {
                    return Ok(());
                }
                if message.len() >= 64 || !b"READY\n".starts_with(&message) {
                    return Err("interactive worker sent an invalid readiness proof".into());
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        Err("interactive worker did not become ready".into())
    }
}

impl Drop for ReadinessPipe {
    fn drop(&mut self) {
        for handle in [&mut self.read, &mut self.write] {
            if !(*handle).is_null() {
                // SAFETY: this wrapper owns both parent pipe endpoints.
                unsafe {
                    let _ = CloseHandle(*handle);
                }
                *handle = std::ptr::null_mut();
            }
        }
    }
}

/// Heap storage required by the Windows extended-startup attribute API.
struct StartupAttributeList {
    bytes: Vec<u8>,
}

impl StartupAttributeList {
    fn only_handles(handles: &[*mut c_void]) -> PackageResult<Self> {
        let mut size = 0;
        // SAFETY: the first sizing call intentionally supplies a null list.
        unsafe {
            let _ = InitializeProcThreadAttributeList(std::ptr::null_mut(), 1, 0, &mut size);
        }
        if size == 0 {
            return Err("interactive worker handle policy could not be initialized".into());
        }
        let mut bytes = vec![0u8; size];
        let pointer = bytes.as_mut_ptr().cast();
        // SAFETY: `bytes` owns the exact buffer size requested by Windows.
        if unsafe { InitializeProcThreadAttributeList(pointer, 1, 0, &mut size) } == 0 {
            return Err("interactive worker handle policy could not be initialized".into());
        }
        // SAFETY: the two valid, inheritable handles stay live for process
        // creation. Windows copies only this fixed handle array.
        if unsafe {
            UpdateProcThreadAttribute(
                pointer,
                0,
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
                handles.as_ptr().cast(),
                std::mem::size_of_val(handles),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        } == 0
        {
            unsafe {
                DeleteProcThreadAttributeList(pointer);
            }
            return Err("interactive worker handle policy could not be applied".into());
        }
        Ok(Self { bytes })
    }

    fn pointer(&self) -> *mut c_void {
        self.bytes.as_ptr().cast_mut().cast()
    }
}

impl Drop for StartupAttributeList {
    fn drop(&mut self) {
        if !self.bytes.is_empty() {
            // SAFETY: InitializeProcThreadAttributeList initialized this exact
            // buffer before it was retained by the wrapper.
            unsafe {
                DeleteProcThreadAttributeList(self.bytes.as_mut_ptr().cast());
            }
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        if !self.process.is_null() {
            // SAFETY: the supervisor owns this worker handle. Termination occurs
            // only on service stop, session loss, or a user-session switch.
            unsafe {
                let _ = TerminateProcess(self.process, 0);
                let _ = WaitForSingleObject(self.process, 5_000);
                let _ = CloseHandle(self.process);
            }
        }
    }
}

struct UserToken {
    handle: *mut c_void,
}

/// A real local profile directory whose owner is the active WTS token's user.
/// The service must never derive this path from its own environment.
struct BoundUserProfile {
    path: PathBuf,
    sid: String,
}

/// Keeps the receipt live until the user worker reports ready. Every early
/// startup return rolls the extension back; a crash leaves the durable receipt
/// for the next startup to recover before another install.
struct PendingExtensionInstall {
    receipt: Option<PlayniteExtensionReceipt>,
    profile: BoundUserProfile,
}

impl PendingExtensionInstall {
    fn finalize(mut self) -> PackageResult<()> {
        let receipt = self.receipt.as_ref().expect("pending receipt");
        finalize_playnite_extension(receipt, &self.profile.path, &self.profile.sid)?;
        self.receipt = None;
        Ok(())
    }
}

impl Drop for PendingExtensionInstall {
    fn drop(&mut self) {
        if let Some(receipt) = self.receipt.take()
            && let Err(error) =
                rollback_playnite_extension(&receipt, &self.profile.path, &self.profile.sid)
        {
            eprintln!("Playnite extension rollback requires recovery: {error}");
        }
    }
}

fn prepare_playnite_extension(
    token: *mut c_void,
    executable: &Path,
) -> PackageResult<Option<PendingExtensionInstall>> {
    let profile = BoundUserProfile::for_token(token)?;
    // This is the default per-user Playnite location only. A configured
    // playnite_extensions_dir needs an authenticated per-user setting flow.
    let mut extensions = profile.path.clone();
    for component in ["AppData", "Roaming", "Playnite", "Extensions"] {
        extensions.push(component);
        match std::fs::symlink_metadata(&extensions) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err("Playnite extension target is unavailable".into()),
            Ok(metadata) if metadata.is_dir() && !is_reparse_point(&metadata) => {}
            Ok(_) => return Err("Playnite extension target contains an unsafe path".into()),
        }
    }
    if let Some(receipt) =
        pending_playnite_extension_receipt(&extensions, &profile.path, &profile.sid)?
    {
        rollback_playnite_extension(&receipt, &profile.path, &profile.sid)?;
    }
    let source = bundled_extension_source(executable)?;
    match install_playnite_extension(&source, &extensions, &profile.path, &profile.sid)? {
        PlayniteExtensionInstall::AlreadyCurrent => Ok(None),
        PlayniteExtensionInstall::Installed(receipt) => Ok(Some(PendingExtensionInstall {
            receipt: Some(receipt),
            profile,
        })),
    }
}

fn is_reparse_point(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

impl BoundUserProfile {
    fn for_token(token: *mut c_void) -> PackageResult<Self> {
        let mut length = 0u32;
        // SAFETY: the sizing call supplies no output buffer; token remains owned
        // by UserToken throughout this lookup.
        unsafe { GetUserProfileDirectoryW(token, std::ptr::null_mut(), &mut length) };
        if !(2..=32_768).contains(&length) {
            return Err("active user profile is unavailable".into());
        }
        let mut buffer = vec![0u16; length as usize];
        // SAFETY: buffer has the exact capacity requested by userenv and token
        // remains live for the synchronous call.
        if unsafe { GetUserProfileDirectoryW(token, buffer.as_mut_ptr(), &mut length) } == 0
            || length < 2
            || length as usize > buffer.len()
            || buffer[length as usize - 1] != 0
        {
            return Err("active user profile is unavailable".into());
        }
        let path = PathBuf::from(OsString::from_wide(&buffer[..length as usize - 1]));
        if !path.is_absolute()
            || !matches!(
                path.components().next(),
                Some(std::path::Component::Prefix(prefix))
                    if matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_))
            )
        {
            return Err("active user profile is not a local absolute path".into());
        }
        require_real_path_ancestors(&path)?;
        let canonical = path
            .canonicalize()
            .map_err(|_| "active user profile is unavailable")?;
        if !canonical.is_dir() {
            return Err("active user profile is unavailable".into());
        }
        let sid = verify_profile_owner(token, &canonical)?;
        Ok(Self {
            path: canonical,
            sid,
        })
    }
}

fn bundled_extension_source(executable: &Path) -> PackageResult<PathBuf> {
    require_real_path_ancestors(executable)?;
    if !executable.is_file() {
        return Err("service executable is unavailable".into());
    }
    let parent = executable
        .parent()
        .ok_or("service executable has no install directory")?;
    let source = parent.join("playnite-extension");
    require_real_path_ancestors(&source)?;
    if !source.is_dir() {
        return Err("bundled Playnite extension is unavailable".into());
    }
    source
        .canonicalize()
        .map_err(|_| "bundled Playnite extension is unavailable".into())
}

fn require_real_path_ancestors(path: &Path) -> PackageResult<()> {
    for ancestor in path.ancestors() {
        let wide = wide_os(ancestor.as_os_str())?;
        // SAFETY: wide_os NUL-terminates every component path. Reparse points
        // could redirect an elevated installer outside its approved location.
        let attributes = unsafe { GetFileAttributesW(wide.as_ptr()) };
        if attributes == INVALID_FILE_ATTRIBUTES || attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err("installer path contains a reparse point or missing ancestor".into());
        }
    }
    Ok(())
}

fn verify_profile_owner(token: *mut c_void, profile: &Path) -> PackageResult<String> {
    let mut length = 0u32;
    // SAFETY: the sizing call intentionally passes no output buffer.
    unsafe { GetTokenInformation(token, TOKEN_USER, std::ptr::null_mut(), 0, &mut length) };
    if length < std::mem::size_of::<SidAndAttributes>() as u32 || length > 16 * 1024 {
        return Err("active user token has no valid SID".into());
    }
    let mut token_user = vec![0u8; length as usize];
    // SAFETY: token_user is large enough for the TOKEN_USER result and remains
    // allocated until after EqualSid has consumed its SID pointer.
    if unsafe {
        GetTokenInformation(
            token,
            TOKEN_USER,
            token_user.as_mut_ptr().cast(),
            length,
            &mut length,
        )
    } == 0
    {
        return Err("active user token SID is unavailable".into());
    }
    // SAFETY: GetTokenInformation wrote the leading SID_AND_ATTRIBUTES. Use an
    // unaligned read because a byte buffer does not promise pointer alignment.
    let user = unsafe { std::ptr::read_unaligned(token_user.as_ptr().cast::<SidAndAttributes>()) };
    if user.sid.is_null() {
        return Err("active user token SID is unavailable".into());
    }
    let path = wide_os(profile.as_os_str())?;
    let mut owner = std::ptr::null_mut();
    let mut descriptor = std::ptr::null_mut();
    // SAFETY: path is NUL-terminated; GetNamedSecurityInfoW returns the owner
    // pointer inside its separately owned descriptor allocation.
    let status = unsafe {
        GetNamedSecurityInfoW(
            path.as_ptr(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION,
            &mut owner,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut descriptor,
        )
    };
    if status != 0 || descriptor.is_null() || owner.is_null() {
        if !descriptor.is_null() {
            // SAFETY: GetNamedSecurityInfoW owns this descriptor allocation.
            unsafe { LocalFree(descriptor) };
        }
        return Err("active user profile owner is unavailable".into());
    }
    // SAFETY: both SID pointers remain valid until the descriptor and token
    // information buffer are released after this comparison.
    let matches = unsafe { EqualSid(user.sid, owner) } != 0;
    // SAFETY: owner points inside this descriptor and must not be freed alone.
    unsafe { LocalFree(descriptor) };
    if !matches {
        return Err("active user profile is not owned by its token SID".into());
    }
    let mut text = std::ptr::null_mut();
    // SAFETY: the token SID is valid while token_user remains allocated;
    // Windows returns a separate LocalFree-owned UTF-16 string.
    if unsafe { ConvertSidToStringSidW(user.sid, &mut text) } == 0 || text.is_null() {
        return Err("active user token SID is unavailable".into());
    }
    let mut length = 0usize;
    // SAFETY: Windows returns a terminated SID string. A SID string longer
    // than this bound is rejected before exposing it to the installer.
    while length < 184 && unsafe { *text.add(length) } != 0 {
        length += 1;
    }
    let sid = if length == 184 {
        Err("active user token SID is invalid".into())
    } else {
        // SAFETY: the slice excludes the terminator and remains owned locally.
        String::from_utf16(unsafe { std::slice::from_raw_parts(text, length) })
            .map_err(|_| "active user token SID is invalid".to_string())
    };
    // SAFETY: ConvertSidToStringSidW allocated this exact buffer.
    unsafe { LocalFree(text.cast()) };
    let sid = sid?;
    if !sid.starts_with("S-1-")
        || !sid
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b'-' || byte == b'S')
    {
        return Err("active user token SID is invalid".into());
    }
    Ok(sid)
}

impl UserToken {
    fn for_session(session_id: u32) -> PackageResult<Self> {
        let mut token = std::ptr::null_mut();
        // SAFETY: WTS writes one token handle on success for the active session.
        if unsafe { WTSQueryUserToken(session_id, &mut token) } == 0 || token.is_null() {
            return Err("active user token is unavailable".into());
        }
        Ok(Self { handle: token })
    }

    fn duplicate_primary(&self) -> PackageResult<PrimaryToken> {
        let mut duplicated = std::ptr::null_mut();
        let access = TOKEN_ASSIGN_PRIMARY
            | TOKEN_DUPLICATE
            | TOKEN_QUERY
            | TOKEN_ADJUST_DEFAULT
            | TOKEN_ADJUST_SESSIONID;
        // SAFETY: the WTS token remains owned by this wrapper. Requested access
        // is limited to process creation and user-session environment setup.
        if unsafe {
            DuplicateTokenEx(
                self.handle,
                access,
                std::ptr::null(),
                SECURITY_IMPERSONATION,
                TOKEN_PRIMARY,
                &mut duplicated,
            )
        } == 0
            || duplicated.is_null()
        {
            return Err("active user primary token is unavailable".into());
        }
        Ok(PrimaryToken { handle: duplicated })
    }
}

impl Drop for UserToken {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            // SAFETY: this wrapper owns the WTS-issued token handle.
            unsafe { CloseHandle(self.handle) };
        }
    }
}

struct PrimaryToken {
    handle: *mut c_void,
}

impl Drop for PrimaryToken {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            // SAFETY: this wrapper owns the duplicated primary token handle.
            unsafe { CloseHandle(self.handle) };
        }
    }
}

struct UserEnvironment {
    pointer: *mut c_void,
}

impl UserEnvironment {
    fn for_token(token: *mut c_void) -> PackageResult<Self> {
        let mut environment = std::ptr::null_mut();
        // SAFETY: token remains valid for the call and the returned block is
        // released by this wrapper with its matching userenv API.
        if unsafe { CreateEnvironmentBlock(&mut environment, token, 0) } == 0
            || environment.is_null()
        {
            return Err("active user environment is unavailable".into());
        }
        Ok(Self {
            pointer: environment,
        })
    }
}

impl Drop for UserEnvironment {
    fn drop(&mut self) {
        if !self.pointer.is_null() {
            // SAFETY: this wrapper owns the userenv environment block.
            unsafe { DestroyEnvironmentBlock(self.pointer) };
        }
    }
}

struct InheritableNull {
    handle: *mut c_void,
}

impl InheritableNull {
    fn open() -> PackageResult<Self> {
        let mut attributes = SecurityAttributes {
            length: std::mem::size_of::<SecurityAttributes>() as u32,
            security_descriptor: std::ptr::null_mut(),
            inherit_handle: 1,
        };
        let name: Vec<u16> = "NUL".encode_utf16().chain(Some(0)).collect();
        // SAFETY: name and attributes remain valid throughout CreateFileW.
        let handle = unsafe {
            CreateFileW(
                name.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                &mut attributes,
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                std::ptr::null_mut(),
            )
        };
        if handle.is_null() || handle as isize == -1 {
            return Err("interactive worker standard handles are unavailable".into());
        }
        Ok(Self { handle })
    }
}

impl Drop for InheritableNull {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            // SAFETY: this wrapper owns the NUL device handle.
            unsafe { CloseHandle(self.handle) };
        }
    }
}

fn active_user_session() -> PackageResult<Option<u32>> {
    let mut sessions = std::ptr::null_mut();
    let mut count = 0;
    // SAFETY: WTS allocates the session array and WTSFreeMemory frees it below.
    if unsafe { WTSEnumerateSessionsW(std::ptr::null_mut(), 0, 1, &mut sessions, &mut count) } == 0
    {
        return Err("interactive session enumeration is unavailable".into());
    }
    let console = unsafe { WTSGetActiveConsoleSessionId() };
    let active = if sessions.is_null() || count == 0 {
        None
    } else {
        // SAFETY: WTS returned `count` contiguous session entries and no field
        // reference escapes the allocation before it is freed.
        let sessions = unsafe { std::slice::from_raw_parts(sessions, count as usize) };
        sessions
            .iter()
            .filter(|session| session.state == WTS_ACTIVE)
            .map(|session| session.session_id)
            .find(|session| *session == console && console != INVALID_SESSION_ID)
            .or_else(|| {
                sessions
                    .iter()
                    .find(|session| session.state == WTS_ACTIVE)
                    .map(|session| session.session_id)
            })
    };
    if !sessions.is_null() {
        // SAFETY: this is the exact allocation returned by WTSEnumerateSessionsW.
        unsafe { WTSFreeMemory(sessions.cast()) };
    }
    Ok(active)
}

fn wide_os(value: &OsStr) -> PackageResult<Vec<u16>> {
    let mut wide: Vec<u16> = value.encode_wide().collect();
    if wide.contains(&0) {
        return Err("interactive worker path contains a NUL character".into());
    }
    wide.push(0);
    Ok(wide)
}

fn command_line(arguments: &[&OsStr]) -> PackageResult<Vec<u16>> {
    let mut line = String::new();
    for (index, argument) in arguments.iter().enumerate() {
        if index != 0 {
            line.push(' ');
        }
        let argument = argument.to_string_lossy();
        if argument.contains('\0') {
            return Err("interactive worker command contains a NUL character".into());
        }
        line.push_str(&quote_windows_argument(&argument));
    }
    Ok(line.encode_utf16().chain(Some(0)).collect())
}

fn quote_windows_argument(argument: &str) -> String {
    if !argument.is_empty() && !argument.contains([' ', '\t', '"']) {
        return argument.into();
    }
    let mut quoted = String::from('"');
    let mut slashes = 0usize;
    for character in argument.chars() {
        match character {
            '\\' => slashes += 1,
            '"' => {
                quoted.push_str(&"\\".repeat(slashes * 2 + 1));
                quoted.push('"');
                slashes = 0;
            }
            _ => {
                quoted.push_str(&"\\".repeat(slashes));
                quoted.push(character);
                slashes = 0;
            }
        }
    }
    quoted.push_str(&"\\".repeat(slashes * 2));
    quoted.push('"');
    quoted
}

#[cfg(test)]
mod tests {
    use super::{BoundUserProfile, CloseHandle, TOKEN_QUERY, quote_windows_argument};
    use std::ffi::c_void;

    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn OpenProcessToken(process: *mut c_void, access: u32, token: *mut *mut c_void) -> i32;
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentProcess() -> *mut c_void;
    }

    #[test]
    fn profile_directory_and_sid_are_bound_to_the_same_token() {
        let mut token = std::ptr::null_mut();
        // SAFETY: the process pseudo handle is valid and OpenProcessToken
        // returns one separately owned token handle on success.
        assert_ne!(
            unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) },
            0
        );
        let profile = BoundUserProfile::for_token(token);
        // SAFETY: OpenProcessToken returned this owned handle.
        unsafe { CloseHandle(token) };
        let profile = profile.unwrap();
        assert!(profile.path.is_dir());
        assert!(profile.sid.starts_with("S-1-"));
    }

    #[test]
    fn quotes_windows_arguments_without_rewriting_safe_paths() {
        assert_eq!(
            quote_windows_argument(r"C:\Games\Example.exe"),
            r"C:\Games\Example.exe"
        );
        assert_eq!(
            quote_windows_argument(r"C:\Program Files\Example\\"),
            r#""C:\Program Files\Example\\\\""#
        );
        assert_eq!(
            quote_windows_argument(r#"say "hello""#),
            r#""say \"hello\"""#
        );
    }
}
