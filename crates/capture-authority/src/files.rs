//! Root-owned immutable inputs used at the execution boundary.
use crate::AuthorityResult;
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::Read,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Component, Path},
};

/// Checks every named ancestor and opens the final inode without following a
/// symlink. This requires root-owned, non-writable ancestors, so an ordinary
/// user cannot replace a checked prefix between the checks and final open.
pub(crate) fn open_protected_file(path: &Path) -> AuthorityResult<File> {
    if !path.is_absolute() {
        return Err("protected file path must be absolute".into());
    }
    let mut prefix = std::path::PathBuf::from("/");
    let components: Vec<_> = path.components().collect();
    for (index, component) in components.iter().enumerate() {
        match component {
            Component::RootDir => continue,
            Component::Normal(name) => prefix.push(name),
            _ => return Err("protected path must be normalized".into()),
        }
        let metadata =
            std::fs::symlink_metadata(&prefix).map_err(|e| format!("{}: {e}", prefix.display()))?;
        if metadata.uid() != 0 || metadata.mode() & 0o022 != 0 || metadata.file_type().is_symlink()
        {
            return Err(format!(
                "{} must be root-owned, unshared and not a symlink",
                prefix.display()
            ));
        }
        if index + 1 != components.len() && !metadata.is_dir() {
            return Err("protected path ancestor is not a directory".into());
        }
    }
    let file = File::options()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|e| e.to_string())?;
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    if !metadata.is_file() || metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
        return Err("protected input must be a root-owned unshared regular file".into());
    }
    Ok(file)
}

pub(crate) fn verify_file(
    path: &Path,
    expected: &str,
    executable: bool,
    root_only: bool,
) -> AuthorityResult<File> {
    let mut file = open_protected_file(path)?;
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    if executable && metadata.mode() & 0o100 == 0 {
        return Err("protected executable lacks owner execute permission".into());
    }
    if metadata.mode() & 0o6000 != 0 {
        return Err("set-id protected artifacts are rejected".into());
    }
    if root_only && metadata.mode() & 0o033 != 0 {
        return Err("privileged helper must be executable only by root".into());
    }
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|e| e.to_string())?;
        if read == 0 {
            break;
        }
        hash.update(&buffer[..read]);
    }
    let actual = format!("{:x}", hash.finalize());
    if actual != expected {
        return Err("protected artifact digest changed".into());
    }
    // Reject both legacy file capabilities and any other privilege-bearing set.
    // SAFETY: the descriptor is open and name is a terminated constant string.
    use std::os::fd::AsRawFd;
    let result = unsafe {
        libc::fgetxattr(
            file.as_raw_fd(),
            c"security.capability".as_ptr(),
            std::ptr::null_mut(),
            0,
        )
    };
    if result >= 0 {
        return Err("protected artifacts must not carry file capabilities".into());
    }
    let error = std::io::Error::last_os_error().raw_os_error();
    if error != Some(libc::ENODATA) && error != Some(libc::ENOTSUP) {
        return Err("cannot inspect protected artifact capabilities".into());
    }
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn relative_parent_and_user_writable_ancestors_are_rejected() {
        for path in ["relative", "/tmp/../bin/sh", "/tmp/not-a-capture-helper"] {
            assert!(open_protected_file(Path::new(path)).is_err(), "{path}");
        }
    }
    #[test]
    fn capability_and_digest_checks_reject_untrusted_executables() {
        assert!(verify_file(Path::new("/usr/bin/true"), &"0".repeat(64), true, false).is_err());
    }
}
