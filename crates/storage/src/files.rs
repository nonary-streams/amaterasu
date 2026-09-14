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
