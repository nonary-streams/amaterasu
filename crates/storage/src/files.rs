use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;

use crate::{StorageResult, random_identifier};

const MAX_METADATA_BYTES: u64 = 1_048_576;

pub fn read_bounded(path: &Path, maximum_bytes: u64) -> StorageResult<Vec<u8>> {
    let metadata =
        fs::symlink_metadata(path).map_err(|error| format!("{}: {error}", path.display()))?;
    if !metadata.is_file() || metadata.len() > maximum_bytes {
        return Err(format!("not a regular bounded file: {}", path.display()));
    }

    let mut bytes = Vec::new();
    File::open(path)
        .map_err(|error| error.to_string())?
        .take(maximum_bytes + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;

    if bytes.len() as u64 > maximum_bytes {
        return Err(format!("file exceeds size limit: {}", path.display()));
    }
    Ok(bytes)
}

pub fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> StorageResult<T> {
    let bytes = read_bounded(path, MAX_METADATA_BYTES)?;
    serde_json::from_slice(&bytes).map_err(|error| format!("{}: {error}", path.display()))
}

pub fn create_private_directory(path: &Path) -> StorageResult<()> {
    if path.try_exists().map_err(|error| error.to_string())? {
        let metadata = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(format!("not a real directory: {}", path.display()));
        }
    }
    fs::create_dir_all(path).map_err(|error| error.to_string())?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

/// Creates a directory for machine-owned installer data. When called by
/// LocalSystem on Windows, its owner is Administrators and its protected DACL
/// grants only LocalSystem and Administrators; descendants inherit that ACL.
pub fn create_machine_private_directory(path: &Path) -> StorageResult<()> {
    create_private_directory(path)?;
    #[cfg(windows)]
    if running_as_local_system()? {
        set_machine_private_dacl(path)?;
    }
    Ok(())
}

/// Explicit installer operation. Unlike `create_machine_private_directory`,
/// this assigns a trusted Administrators owner and seals the directory when
/// called by an elevated administrator too. The caller must establish that
/// every parent is installer-owned.
pub fn seal_machine_private_directory(path: &Path) -> StorageResult<()> {
    create_private_directory(path)?;
    #[cfg(windows)]
    set_machine_private_dacl(path)?;
    Ok(())
}

/// Protects an existing regular file containing machine-owned secrets.
/// On Windows, only LocalSystem assigns the trusted Administrators owner and
/// applies the installed SY/BA ACL, matching
/// `create_machine_private_directory`; non-SYSTEM development runs retain
/// their caller-owned ACL. Callers must protect the parent separately.
pub fn protect_machine_private_file(path: &Path) -> StorageResult<()> {
    let metadata = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(format!("not a regular file: {}", path.display()));
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err(format!(
                "reparse point is not a regular file: {}",
                path.display()
            ));
        }
        if running_as_local_system()? {
            set_machine_private_dacl(path)?;
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

/// Explicit elevated-installer counterpart to `protect_machine_private_file`.
/// It assigns ownership to Administrators and sets the SYSTEM/Administrators
/// DACL even when the caller is an admin rather than LocalSystem. The caller
/// must establish a protected parent.
pub fn seal_machine_private_file(path: &Path) -> StorageResult<()> {
    let metadata = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(format!("not a regular file: {}", path.display()));
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err(format!(
                "reparse point is not a regular file: {}",
                path.display()
            ));
        }
        set_machine_private_dacl(path)?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

#[cfg(windows)]
fn running_as_local_system() -> StorageResult<bool> {
    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn GetUserNameW(buffer: *mut u16, length: *mut u32) -> i32;
    }
    let mut buffer = [0u16; 256];
    let mut length = buffer.len() as u32;
    // SAFETY: buffer is writable and its capacity is supplied in UTF-16 units.
    if unsafe { GetUserNameW(buffer.as_mut_ptr(), &mut length) } == 0 || length == 0 {
        return Err("current account could not be determined".into());
    }
    Ok(String::from_utf16_lossy(&buffer[..length as usize - 1]).eq_ignore_ascii_case("SYSTEM"))
}

#[cfg(windows)]
fn set_machine_private_dacl(path: &Path) -> StorageResult<()> {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;

    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn ConvertStringSecurityDescriptorToSecurityDescriptorW(
            source: *const u16,
            revision: u32,
            descriptor: *mut *mut c_void,
            length: *mut u32,
        ) -> i32;
        fn SetFileSecurityW(path: *const u16, information: u32, descriptor: *const c_void) -> i32;
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn LocalFree(memory: *mut c_void) -> *mut c_void;
    }

    const OWNER_SECURITY_INFORMATION: u32 = 0x0000_0001;
    const DACL_SECURITY_INFORMATION: u32 = 0x0000_0004;
    // A protected DACL alone is insufficient: the owner can replace it.
    // Assign ownership to the trusted Administrators group as part of the
    // same operation, then grant full control only to SYSTEM and Administrators.
    let sddl: Vec<u16> = "O:BAG:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)"
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let target: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut descriptor = std::ptr::null_mut();
    let mut length = 0u32;
    // SAFETY: both buffers are NUL-terminated and remain live for these calls.
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
        return Err("machine-private ACL could not be constructed".into());
    }
    // SAFETY: descriptor is allocated by the preceding Windows API call.
    let applied = unsafe {
        SetFileSecurityW(
            target.as_ptr(),
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            descriptor,
        )
    };
    unsafe { LocalFree(descriptor) };
    if applied == 0 {
        return Err("machine-private ACL could not be applied".into());
    }
    Ok(())
}

pub fn write_json_atomically(path: &Path, value: &impl Serialize) -> StorageResult<()> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|error| error.to_string())?;
    write_atomically(path, &bytes)
}

pub fn write_atomically(path: &Path, bytes: &[u8]) -> StorageResult<()> {
    let parent = path.parent().ok_or("file has no parent directory")?;
    let temporary_path = parent.join(format!(".write-{}", random_identifier()?));
    let result = write_and_replace(&temporary_path, path, bytes);
    if result.is_err() {
        let _ = fs::remove_file(&temporary_path);
    }
    result
}

fn write_and_replace(temporary: &Path, destination: &Path, bytes: &[u8]) -> StorageResult<()> {
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }

    let mut file = options.open(temporary).map_err(|error| error.to_string())?;
    file.write_all(bytes).map_err(|error| error.to_string())?;
    file.sync_all().map_err(|error| error.to_string())?;
    drop(file);

    replace_file(temporary, destination)?;
    sync_directory(destination.parent().ok_or("file has no parent directory")?)
}

pub fn sync_directory(path: &Path) -> StorageResult<()> {
    #[cfg(unix)]
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| error.to_string())?;

    #[cfg(not(unix))]
    let _ = path;

    Ok(())
}

#[cfg(not(windows))]
fn replace_file(source: &Path, destination: &Path) -> StorageResult<()> {
    fs::rename(source, destination).map_err(|error| error.to_string())
}

#[cfg(windows)]
fn replace_file(source: &Path, destination: &Path) -> StorageResult<()> {
    use std::os::windows::ffi::OsStrExt;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn MoveFileExW(source: *const u16, destination: *const u16, flags: u32) -> i32;
    }

    const REPLACE_EXISTING: u32 = 0x1;
    const WRITE_THROUGH: u32 = 0x8;
    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();

    // SAFETY: both paths are live, NUL-terminated UTF-16 buffers.
    let succeeded = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            REPLACE_EXISTING | WRITE_THROUGH,
        )
    };
    if succeeded == 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    Ok(())
}

/// Excludes another supervisor or staging operation while this host root is in use.
/// The operating system releases the lock if the process exits unexpectedly.
pub struct RootLock {
    _file: File,
}

impl RootLock {
    pub fn acquire(root: &Path) -> StorageResult<Self> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(root.join("host.lock"))
            .map_err(|error| error.to_string())?;

        file.try_lock()
            .map_err(|error| format!("host root is busy: {error}"))?;
        Ok(Self { _file: file })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn machine_secret_protection_requires_a_regular_file() {
        let root = std::env::temp_dir().join(format!(
            "amaterasu-machine-secret-{}-{}",
            std::process::id(),
            crate::random_identifier().unwrap()
        ));
        fs::create_dir(&root).unwrap();
        let file = root.join("key.pem");
        fs::write(&file, b"private key fixture").unwrap();
        protect_machine_private_file(&file).unwrap();
        #[cfg(windows)]
        if !running_as_local_system().unwrap() {
            assert_eq!(fs::read(&file).unwrap(), b"private key fixture");
        }
        #[cfg(unix)]
        assert_eq!(fs::read(&file).unwrap(), b"private key fixture");
        assert!(protect_machine_private_file(&root).is_err());

        #[cfg(unix)]
        {
            use std::os::unix::fs::{PermissionsExt, symlink};
            assert_eq!(
                fs::metadata(&file).unwrap().permissions().mode() & 0o777,
                0o600
            );
            let link = root.join("key-link.pem");
            symlink(&file, &link).unwrap();
            assert!(protect_machine_private_file(&link).is_err());
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "requires an elevated token able to assign Administrators as owner"]
    fn sealing_machine_private_file_assigns_a_trusted_owner() {
        use std::ffi::c_void;
        use std::os::windows::ffi::OsStrExt;

        #[link(name = "advapi32")]
        unsafe extern "system" {
            fn GetNamedSecurityInfoW(
                name: *mut u16,
                object_type: u32,
                security_info: u32,
                owner: *mut *mut c_void,
                group: *mut *mut c_void,
                dacl: *mut *mut c_void,
                sacl: *mut *mut c_void,
                descriptor: *mut *mut c_void,
            ) -> u32;
            fn ConvertSidToStringSidW(sid: *const c_void, string: *mut *mut u16) -> i32;
        }
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn LocalFree(memory: *mut c_void) -> *mut c_void;
        }

        let root = std::env::temp_dir().join(format!(
            "amaterasu-machine-seal-{}-{}",
            std::process::id(),
            crate::random_identifier().unwrap()
        ));
        fs::create_dir(&root).unwrap();
        let file = root.join("policy.json");
        fs::write(&file, b"machine grant").unwrap();
        seal_machine_private_file(&file).unwrap();

        let mut path: Vec<u16> = file.as_os_str().encode_wide().chain(Some(0)).collect();
        let mut owner = std::ptr::null_mut();
        let mut descriptor = std::ptr::null_mut();
        // SAFETY: path is NUL-terminated; returned owner/descriptor are valid until LocalFree.
        let status = unsafe {
            GetNamedSecurityInfoW(
                path.as_mut_ptr(),
                1,
                0x1,
                &mut owner,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut descriptor,
            )
        };
        assert_eq!(status, 0);
        let mut owner_text = std::ptr::null_mut();
        // SAFETY: owner is the SID returned by GetNamedSecurityInfoW.
        assert_ne!(unsafe { ConvertSidToStringSidW(owner, &mut owner_text) }, 0);
        let mut length = 0;
        // SAFETY: owner_text is a NUL-terminated UTF-16 string allocated by Windows.
        while unsafe { *owner_text.add(length) } != 0 {
            length += 1;
        }
        // SAFETY: owner_text points to `length + 1` readable UTF-16 units.
        let owner_sid =
            String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(owner_text, length) });
        assert_eq!(owner_sid, "S-1-5-32-544");
        // SAFETY: both allocations were returned by Windows LocalAlloc APIs.
        unsafe {
            LocalFree(owner_text.cast());
            LocalFree(descriptor);
        }
        fs::remove_file(file).unwrap();
        fs::remove_dir(root).unwrap();
    }
}
