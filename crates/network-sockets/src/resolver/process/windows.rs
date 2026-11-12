use super::*;
use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::windows::{
        ffi::OsStrExt,
        fs::{MetadataExt, OpenOptionsExt},
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
        process::ExitStatusExt,
    },
    path::{Component, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};
use windows_sys::Win32::{
    Foundation::{
        ERROR_ALREADY_EXISTS, ERROR_BROKEN_PIPE, HANDLE, HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE,
        LocalFree, SetHandleInformation, WAIT_OBJECT_0, WAIT_TIMEOUT,
    },
    Security::{
        Authorization::{
            ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
            SDDL_REVISION_1,
        },
        GetTokenInformation, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser,
    },
    Storage::FileSystem::{
        CREATE_ALWAYS, CreateDirectoryW, CreateFileW, FILE_ATTRIBUTE_NORMAL,
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_DISPOSITION_INFO, FILE_FLAG_BACKUP_SEMANTICS,
        FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ, FILE_SHARE_WRITE, FileDispositionInfo,
        SetFileInformationByHandle,
    },
    System::{
        JobObjects::{
            CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            JobObjectBasicAccountingInformation, JobObjectExtendedLimitInformation,
            QueryInformationJobObject, SetInformationJobObject, TerminateJobObject,
        },
        Pipes::{CreatePipe, PeekNamedPipe},
        Threading::{
            CREATE_NO_WINDOW, CreateProcessW, DeleteProcThreadAttributeList,
            EXTENDED_STARTUPINFO_PRESENT, GetCurrentProcess, GetExitCodeProcess,
            InitializeProcThreadAttributeList, OpenProcessToken, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
            PROC_THREAD_ATTRIBUTE_JOB_LIST, PROCESS_INFORMATION, STARTF_USESTDHANDLES,
            STARTUPINFOEXW, UpdateProcThreadAttribute, WaitForSingleObject,
        },
    },
};

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);
const MAX_CACHED_HELPERS: usize = 16;
const HELPER_PREFIX: &str = "amaterasu-network-resolver-";
const HELPER_SUFFIX: &str = ".exe";

pub(super) struct Executable {
    path: PathBuf,
    file: Option<File>,
    directories: Vec<File>,
    temporary_directory: Option<PathBuf>,
    #[cfg(test)]
    arguments: Vec<String>,
}

impl Executable {
    pub(super) fn new(bytes: &[u8], private_state_root: Option<&Path>) -> io::Result<Self> {
        let security = PrivateDirectorySecurity::current_user()?;
        let (directory, directories, temporary_directory) = match private_state_root {
            Some(root) => (
                root.to_owned(),
                lock_directory_chain(root, &security)?,
                None,
            ),
            None => {
                let root = std::env::temp_dir();
                let mut locks = lock_directory_chain(&root, &security)?;
                let directory = create_temporary_directory(&root, &security)?;
                locks.push(lock_directory(&directory)?);
                (directory.clone(), locks, Some(directory))
            }
        };
        let path = directory.join(helper_name(bytes));
        let mut executable = Self {
            path,
            file: None,
            directories,
            temporary_directory,
            #[cfg(test)]
            arguments: Vec::new(),
        };
        let _cache_lock = lock_cache(&directory)?;
        reserve_cache_slot(&directory, &executable.path)?;
        match OpenOptions::new()
            .write(true)
            .access_mode(0x4000_0000 | 0x0001_0000)
            .create_new(true)
            .share_mode(0)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(&executable.path)
        {
            Ok(mut file) => {
                initialize_new_helper(&mut file, |file| {
                    file.write_all(bytes).and_then(|()| file.sync_all())
                })?;
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
        let mut file = OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(&executable.path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file()
            || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
            || metadata.len() != bytes.len() as u64
        {
            return Err(invalid_helper());
        }
        let mut stored = Vec::with_capacity(bytes.len());
        Read::by_ref(&mut file)
            .take(bytes.len() as u64 + 1)
            .read_to_end(&mut stored)?;
        if stored != bytes {
            return Err(invalid_helper());
        }
        // Keep every ancestor and the no-write/no-delete file handle while
        // CreateProcess resolves this exact verified absolute path.
        executable.file = Some(file);
        Ok(executable)
    }

    #[cfg(test)]
    pub(super) fn set_test_arguments(&mut self, arguments: &[&str]) {
        self.arguments = arguments
            .iter()
            .map(|argument| (*argument).to_owned())
            .collect();
    }

    pub(super) fn spawn(&self, request: &[u8]) -> io::Result<Process> {
        let (input, mut request_pipe) = pipe()?;
        clear_inherit(request_pipe.as_raw_handle())?;
        request_pipe.write_all(request)?;
        drop(request_pipe);
        let (output, child_output) = pipe()?;
        clear_inherit(output.as_raw_handle())?;
        let error_output = null_output()?;
        let job = new_job()?;
        let mut inherited = [
            input.as_raw_handle(),
            child_output.as_raw_handle(),
            error_output.as_raw_handle(),
        ];
        let mut jobs = [job.as_raw_handle()];
        let attributes = ProcessAttributes::new(&mut inherited, &mut jobs)?;
        let mut startup = STARTUPINFOEXW::default();
        startup.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
        startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        startup.StartupInfo.hStdInput = input.as_raw_handle();
        startup.StartupInfo.hStdOutput = child_output.as_raw_handle();
        startup.StartupInfo.hStdError = error_output.as_raw_handle();
        startup.lpAttributeList = attributes.pointer();
        let application = wide(&self.path)?;
        let command_line = format!(
            "amaterasu-network-resolver --parent-pid {}",
            std::process::id()
        );
        #[cfg(test)]
        let command_line = format!(
            "{command_line}{}",
            self.arguments
                .iter()
                .map(|argument| format!(" {}", quote_argument(argument)))
                .collect::<String>()
        );
        let mut command_line: Vec<u16> = command_line.encode_utf16().chain(Some(0)).collect();
        let mut information = PROCESS_INFORMATION::default();
        // SAFETY: all strings, standard handles, attribute backing storage,
        // and output records remain live. Job assignment and the explicit
        // inherited-handle allowlist are applied atomically at creation.
        if unsafe {
            CreateProcessW(
                application.as_ptr(),
                command_line.as_mut_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                1,
                EXTENDED_STARTUPINFO_PRESENT | CREATE_NO_WINDOW,
                std::ptr::null(),
                std::ptr::null(),
                &startup.StartupInfo,
                &mut information,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: CreateProcess transferred two new owned handles. The
        // initial thread handle is closed; the job contains its descendants.
        let process = unsafe { OwnedHandle::from_raw_handle(information.hProcess) };
        drop(unsafe { OwnedHandle::from_raw_handle(information.hThread) });
        Ok(Process {
            process,
            job,
            output,
            exit: None,
        })
    }
}

impl Drop for Executable {
    fn drop(&mut self) {
        if let Some(directory) = self.temporary_directory.take() {
            drop(self.file.take());
            let _ = std::fs::remove_file(&self.path);
            let _ = std::fs::remove_file(directory.join("cache.lock"));
            drop(self.directories.pop());
            let _ = std::fs::remove_dir(directory);
        }
    }
}

pub(super) struct Process {
    process: OwnedHandle,
    job: OwnedHandle,
    output: File,
    exit: Option<ExitStatus>,
}

impl Process {
    pub(super) fn try_read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        let mut available = 0;
        // SAFETY: this owned pipe has a sole reader, and only one writable
        // count is passed; no data buffer is borrowed by PeekNamedPipe.
        if unsafe {
            PeekNamedPipe(
                self.output.as_raw_handle(),
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                &mut available,
                std::ptr::null_mut(),
            )
        } == 0
        {
            let error = io::Error::last_os_error();
            return if error.raw_os_error() == Some(ERROR_BROKEN_PIPE as i32) {
                Ok(0)
            } else {
                Err(error)
            };
        }
        if available == 0 {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        let length = output.len().min(available as usize);
        // Peek established this many bytes and this object owns the only
        // parent read handle, so Read never waits for future child output.
        self.output.read(&mut output[..length])
    }

    pub(super) fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        if self.exit.is_none() {
            // SAFETY: zero timeout polls a live exact process handle.
            match unsafe { WaitForSingleObject(self.process.as_raw_handle(), 0) } {
                WAIT_TIMEOUT => return Ok(None),
                WAIT_OBJECT_0 => {}
                _ => return Err(io::Error::last_os_error()),
            }
            let mut code = 0;
            // SAFETY: process has exited and code is writable output.
            if unsafe { GetExitCodeProcess(self.process.as_raw_handle(), &mut code) } == 0 {
                return Err(io::Error::last_os_error());
            }
            self.exit = Some(ExitStatus::from_raw(code));
        }
        let mut accounting = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
        // SAFETY: full output structure and owned job handle remain live.
        if unsafe {
            QueryInformationJobObject(
                self.job.as_raw_handle(),
                JobObjectBasicAccountingInformation,
                (&mut accounting as *mut JOBOBJECT_BASIC_ACCOUNTING_INFORMATION).cast(),
                std::mem::size_of_val(&accounting) as u32,
                std::ptr::null_mut(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok((accounting.ActiveProcesses == 0)
            .then_some(self.exit)
            .flatten())
    }

    pub(super) fn kill(&mut self) -> io::Result<()> {
        // SAFETY: this unnamed, non-inheritable job contains only children
        // created for this resolver request; termination is not exit proof.
        if unsafe { TerminateJobObject(self.job.as_raw_handle(), 1) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

fn lock_directory_chain(
    directory: &Path,
    security: &PrivateDirectorySecurity,
) -> io::Result<Vec<File>> {
    if !directory.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "resolver state path is relative",
        ));
    }
    let mut current = PathBuf::new();
    let mut directories = Vec::new();
    for component in directory.components() {
        match component {
            Component::Prefix(prefix) => {
                if !matches!(
                    prefix.kind(),
                    std::path::Prefix::Disk(_) | std::path::Prefix::VerbatimDisk(_)
                ) {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "resolver state directory must use a local drive",
                    ));
                }
                current.push(prefix.as_os_str());
            }
            Component::RootDir => {
                current.push(component.as_os_str());
                directories.push(lock_directory(&current)?);
            }
            Component::Normal(name) => {
                current.push(name);
                security.create_directory(&current, false)?;
                directories.push(lock_directory(&current)?);
            }
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "resolver state path contains traversal",
                ));
            }
        }
    }
    Ok(directories)
}

fn helper_name(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let digest: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("{HELPER_PREFIX}{digest}{HELPER_SUFFIX}")
}

fn helper_digest(name: &str) -> Option<&str> {
    let digest = name
        .strip_prefix(HELPER_PREFIX)?
        .strip_suffix(HELPER_SUFFIX)?;
    (digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
    .then_some(digest)
}

fn lock_cache(directory: &Path) -> io::Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .share_mode(0)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(directory.join("cache.lock"))?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(invalid_helper());
    }
    Ok(file)
}

/// The exclusive cache lock serializes the count and creation across worker
/// processes. Only exact digest-named entries participate in this budget.
fn reserve_cache_slot(directory: &Path, wanted: &Path) -> io::Result<()> {
    let mut entries = Vec::new();
    for (index, entry) in std::fs::read_dir(directory)?.enumerate() {
        if index == 256 {
            return Err(io::Error::other(
                "resolver helper cache directory exceeds its scan bound",
            ));
        }
        let entry = entry?;
        if entry.file_name().to_str().and_then(helper_digest).is_some() {
            entries.push(entry.path());
        }
    }
    let additional = usize::from(!entries.iter().any(|path| path == wanted));
    entries.sort();
    let mut count = entries.len();
    for entry in &entries {
        if count + additional <= MAX_CACHED_HELPERS {
            return Ok(());
        }
        if entry != wanted && prune_cached_helper(entry).unwrap_or(false) {
            count -= 1;
        }
    }
    if count + additional > MAX_CACHED_HELPERS {
        return Err(io::Error::other(
            "resolver helper cache is full of retained or unverifiable entries",
        ));
    }
    Ok(())
}

fn prune_cached_helper(path: &Path) -> io::Result<bool> {
    let Some(expected) = path
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(helper_digest)
    else {
        return Ok(false);
    };
    // Request delete access with no sharing. Active images retain a read
    // handle denying deletion, so neither their bytes nor identity changes.
    let mut file = OpenOptions::new()
        .access_mode(0x8000_0000 | 0x0001_0000)
        .share_mode(0)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
        || metadata.len() == 0
        || metadata.len() > MAX_HELPER_BYTES as u64
    {
        return Ok(false);
    }
    let mut digest = Sha256::new();
    let mut buffer = [0; 16 * 1024];
    loop {
        let length = file.read(&mut buffer)?;
        if length == 0 {
            break;
        }
        digest.update(&buffer[..length]);
    }
    let actual: String = digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    if actual != expected {
        return Ok(false);
    }
    mark_delete(&file)?;
    drop(file);
    Ok(true)
}

fn mark_delete(file: &File) -> io::Result<()> {
    let disposition = FILE_DISPOSITION_INFO { DeleteFile: true };
    // SAFETY: this exact verified file is still exclusively held with delete
    // access. Marking this handle never follows a replacement pathname.
    if unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle(),
            FileDispositionInfo,
            (&disposition as *const FILE_DISPOSITION_INFO).cast(),
            std::mem::size_of_val(&disposition) as u32,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn initialize_new_helper(
    file: &mut File,
    write: impl FnOnce(&mut File) -> io::Result<()>,
) -> io::Result<()> {
    if let Err(error) = write(file) {
        // Only CREATE_NEW calls this function. Delete the exact incomplete
        // inode through its still-exclusive handle so retry can recreate it.
        mark_delete(file).map_err(|cleanup| {
            io::Error::other(format!(
                "resolver helper write failed: {error}; incomplete helper cleanup failed: {cleanup}"
            ))
        })?;
        return Err(error);
    }
    Ok(())
}

#[cfg(test)]
pub(super) fn assert_windows_substitution_guards(executable: &Executable) {
    assert!(
        OpenOptions::new()
            .write(true)
            .open(&executable.path)
            .is_err()
    );
    assert!(std::fs::rename(&executable.path, executable.path.with_extension("replaced")).is_err());
    assert!(std::fs::remove_file(&executable.path).is_err());
}

#[cfg(test)]
pub(super) fn assert_windows_cache_bound(bytes: &[u8]) {
    let executable = Executable::new(bytes, None).unwrap();
    let directory = executable.path.parent().unwrap();
    for index in 0..MAX_CACHED_HELPERS - 1 {
        let content = format!("known cache fixture {index}");
        std::fs::write(directory.join(helper_name(content.as_bytes())), content).unwrap();
    }
    let unrelated = directory.join("unrelated.txt");
    std::fs::write(&unrelated, b"preserve").unwrap();
    let replacement = Executable::new(b"next verified helper fixture", Some(directory)).unwrap();
    let entries = || {
        std::fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .and_then(helper_digest)
                    .is_some()
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(entries().len(), MAX_CACHED_HELPERS);
    assert!(executable.path.exists(), "active helper was pruned");
    assert_eq!(std::fs::read(&unrelated).unwrap(), b"preserve");
    let locks: Vec<_> = entries()
        .iter()
        .map(|path| {
            OpenOptions::new()
                .read(true)
                .share_mode(FILE_SHARE_READ)
                .open(path)
                .unwrap()
        })
        .collect();
    assert!(Executable::new(b"cannot fit while all images are retained", Some(directory)).is_err());
    assert_eq!(entries().len(), MAX_CACHED_HELPERS);
    drop(locks);
    drop(replacement);
    for path in entries() {
        if path != executable.path {
            std::fs::remove_file(path).unwrap();
        }
    }
    std::fs::remove_file(unrelated).unwrap();
}

#[cfg(test)]
pub(super) fn assert_windows_failed_write_is_retryable(bytes: &[u8]) {
    let executable = Executable::new(bytes, None).unwrap();
    let directory = executable.path.parent().unwrap();
    let fixture = b"retry this exact newly created helper";
    let path = directory.join(helper_name(fixture));
    let mut file = OpenOptions::new()
        .access_mode(0x4000_0000 | 0x0001_0000)
        .create_new(true)
        .share_mode(0)
        .open(&path)
        .unwrap();
    assert!(
        initialize_new_helper(&mut file, |file| {
            file.write_all(b"partial")?;
            Err(io::Error::other("injected persistence failure"))
        })
        .is_err()
    );
    drop(file);
    assert!(!path.exists(), "partial canonical helper poisoned retry");
    let replacement = Executable::new(fixture, Some(directory)).unwrap();
    drop(replacement);
    std::fs::remove_file(path).unwrap();
}

fn lock_directory(path: &Path) -> io::Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_dir() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(invalid_helper());
    }
    Ok(file)
}

fn create_temporary_directory(
    root: &Path,
    security: &PrivateDirectorySecurity,
) -> io::Result<PathBuf> {
    for _ in 0..128 {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = root.join(format!(
            "amaterasu-network-resolver-{}-{sequence}",
            std::process::id()
        ));
        match security.create_directory(&path, true) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "resolver private directory name budget exhausted",
    ))
}

struct PrivateDirectorySecurity {
    descriptor: *mut std::ffi::c_void,
}

impl PrivateDirectorySecurity {
    fn current_user() -> io::Result<Self> {
        let mut token = std::ptr::null_mut();
        // SAFETY: the pseudo process handle is valid and token is writable.
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: OpenProcessToken returned a new owned handle.
        let token = unsafe { OwnedHandle::from_raw_handle(token) };
        let mut required = 0;
        // SAFETY: null output with zero length queries the required size.
        unsafe {
            GetTokenInformation(
                token.as_raw_handle(),
                TokenUser,
                std::ptr::null_mut(),
                0,
                &mut required,
            )
        };
        if required == 0 || required > 64 * 1024 {
            return Err(io::Error::last_os_error());
        }
        let mut buffer = vec![0usize; (required as usize).div_ceil(std::mem::size_of::<usize>())];
        // SAFETY: usize storage is aligned for TOKEN_USER and advertises its
        // actual byte capacity. Its SID remains live through conversion.
        if unsafe {
            GetTokenInformation(
                token.as_raw_handle(),
                TokenUser,
                buffer.as_mut_ptr().cast(),
                (buffer.len() * std::mem::size_of::<usize>()) as u32,
                &mut required,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let user = unsafe { &*buffer.as_ptr().cast::<TOKEN_USER>() };
        let mut sid = std::ptr::null_mut();
        // SAFETY: Windows supplied the valid SID; sid is writable output.
        if unsafe { ConvertSidToStringSidW(user.User.Sid, &mut sid) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut length = 0;
        // SAFETY: conversion returned a terminated allocation retained until LocalFree.
        while unsafe { *sid.add(length) } != 0 {
            length += 1;
        }
        let sid_text = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(sid, length) });
        unsafe { LocalFree(sid.cast()) };
        let sddl: Vec<u16> = format!("O:{sid_text}D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;{sid_text})")
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let mut descriptor = std::ptr::null_mut();
        // SAFETY: the terminated SDDL and descriptor output are live.
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                std::ptr::null_mut(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { descriptor })
    }

    fn create_directory(&self, path: &Path, exclusive: bool) -> io::Result<()> {
        let path = wide(path)?;
        let attributes = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: self.descriptor,
            bInheritHandle: 0,
        };
        // SAFETY: path is terminated and this security descriptor remains
        // allocated through the call. Existing directory ACLs are preserved.
        if unsafe { CreateDirectoryW(path.as_ptr(), &attributes) } != 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if !exclusive && error.raw_os_error() == Some(ERROR_ALREADY_EXISTS as i32) {
            Ok(())
        } else {
            Err(error)
        }
    }
}

impl Drop for PrivateDirectorySecurity {
    fn drop(&mut self) {
        // SAFETY: SDDL conversion allocated this descriptor with LocalAlloc.
        unsafe { LocalFree(self.descriptor) };
    }
}

fn pipe() -> io::Result<(File, File)> {
    let mut read = std::ptr::null_mut();
    let mut write = std::ptr::null_mut();
    let attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: std::ptr::null_mut(),
        bInheritHandle: 1,
    };
    // SAFETY: output handles and security attributes are live. The empty
    // requested pipe buffer holds the complete bounded request before spawn.
    if unsafe { CreatePipe(&mut read, &mut write, &attributes, 4096) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful CreatePipe returned two unique owned handles.
    Ok(unsafe { (File::from_raw_handle(read), File::from_raw_handle(write)) })
}

fn clear_inherit(handle: HANDLE) -> io::Result<()> {
    // SAFETY: caller owns this handle and changes only its inheritance bit.
    if unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn null_output() -> io::Result<File> {
    let attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: std::ptr::null_mut(),
        bInheritHandle: 1,
    };
    // SAFETY: NUL is a fixed device name; its inheritable handle is included
    // in the explicit child handle list and closed in the parent after spawn.
    let name = [b'N' as u16, b'U' as u16, b'L' as u16, 0];
    let handle = unsafe {
        CreateFileW(
            name.as_ptr(),
            0x4000_0000,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            &attributes,
            CREATE_ALWAYS,
            FILE_ATTRIBUTE_NORMAL,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_handle(handle) })
}

fn new_job() -> io::Result<OwnedHandle> {
    // SAFETY: unnamed job is non-inheritable and is newly owned below.
    let raw = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
    if raw.is_null() {
        return Err(io::Error::last_os_error());
    }
    let job = unsafe { OwnedHandle::from_raw_handle(raw) };
    let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    // SAFETY: complete initialized information and owned job are live.
    if unsafe {
        SetInformationJobObject(
            raw,
            JobObjectExtendedLimitInformation,
            (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
            std::mem::size_of_val(&limits) as u32,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(job)
}

struct ProcessAttributes {
    storage: Vec<usize>,
    initialized: bool,
}

impl ProcessAttributes {
    fn new(handles: &mut [HANDLE], jobs: &mut [HANDLE]) -> io::Result<Self> {
        let mut bytes = 0;
        // SAFETY: null storage queries the required aligned allocation size.
        unsafe { InitializeProcThreadAttributeList(std::ptr::null_mut(), 2, 0, &mut bytes) };
        if bytes == 0 || bytes > 64 * 1024 {
            return Err(io::Error::last_os_error());
        }
        let mut attributes = Self {
            storage: vec![0; bytes.div_ceil(std::mem::size_of::<usize>())],
            initialized: false,
        };
        // SAFETY: usize allocation establishes required pointer alignment and
        // the queried byte capacity. Initialization precedes RAII deletion.
        if unsafe { InitializeProcThreadAttributeList(attributes.pointer(), 2, 0, &mut bytes) } == 0
        {
            return Err(io::Error::last_os_error());
        }
        attributes.initialized = true;
        for (attribute, values) in [
            (PROC_THREAD_ATTRIBUTE_HANDLE_LIST, handles),
            (PROC_THREAD_ATTRIBUTE_JOB_LIST, jobs),
        ] {
            // SAFETY: both arrays outlive CreateProcess and each byte length
            // describes exactly its initialized HANDLE array.
            if unsafe {
                UpdateProcThreadAttribute(
                    attributes.pointer(),
                    0,
                    attribute as usize,
                    values.as_mut_ptr().cast(),
                    std::mem::size_of_val(values),
                    std::ptr::null_mut(),
                    std::ptr::null(),
                )
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(attributes)
    }

    fn pointer(&self) -> windows_sys::Win32::System::Threading::LPPROC_THREAD_ATTRIBUTE_LIST {
        self.storage.as_ptr().cast_mut().cast()
    }
}

impl Drop for ProcessAttributes {
    fn drop(&mut self) {
        // SAFETY: storage remains allocated until the initialized list is destroyed.
        if self.initialized {
            unsafe { DeleteProcThreadAttributeList(self.pointer()) };
        }
    }
}

fn wide(path: &Path) -> io::Result<Vec<u16>> {
    let mut value: Vec<_> = path.as_os_str().encode_wide().collect();
    if value.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "resolver path contains NUL",
        ));
    }
    value.push(0);
    Ok(value)
}

fn invalid_helper() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "resolver helper path or bytes changed",
    )
}

#[cfg(test)]
fn quote_argument(argument: &str) -> String {
    let mut quoted = String::from("\"");
    let mut slashes = 0;
    for character in argument.chars() {
        if character == '\\' {
            slashes += 1;
        } else {
            if character == '"' {
                quoted.extend(std::iter::repeat_n('\\', slashes * 2 + 1));
            } else {
                quoted.extend(std::iter::repeat_n('\\', slashes));
            }
            slashes = 0;
            quoted.push(character);
        }
    }
    quoted.extend(std::iter::repeat_n('\\', slashes * 2));
    quoted.push('"');
    quoted
}
