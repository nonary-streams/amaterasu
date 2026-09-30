//! First-run provisioning for the host-configured TLS identity files.
use crate::{crypto, protocol::Identity};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Component, Path, PathBuf},
};

#[derive(Clone, Copy, Eq, PartialEq)]
enum TargetState {
    Missing,
    Existing,
}

/// Creates the configured identity only when both files are absent. Existing
/// or partial material is never replaced; the normal loader validates it next.
pub(super) fn create_missing_identity_pair(
    certificate_value: &str,
    private_key_value: &str,
    now: u64,
) -> Result<u64, &'static str> {
    let certificate_path = Path::new(certificate_value);
    let private_key_path = Path::new(private_key_value);
    if same_path(certificate_path, private_key_path) {
        return Err("unsafe_identity_path");
    }

    let certificate_state = inspect_target(certificate_path)?;
    let private_key_state = inspect_target(private_key_path)?;
    match (certificate_state, private_key_state) {
        (TargetState::Existing, TargetState::Existing) => return Ok(now),
        (TargetState::Missing, TargetState::Missing) => (),
        _ => return Err("identity_file_unavailable"),
    }

    let identity = crypto::generate_identity()?;
    let validation_time = now.max(current_time()?);
    crypto::validate_identity(&identity, validation_time).map_err(|_| "invalid_identity")?;
    ensure_parent_directories(certificate_path)?;
    ensure_parent_directories(private_key_path)?;

    // Recheck after creating parents. create_new below remains the final
    // no-overwrite guard if another process provisions concurrently.
    if inspect_target(certificate_path)? != TargetState::Missing
        || inspect_target(private_key_path)? != TargetState::Missing
    {
        return Err("identity_file_unavailable");
    }

    // Publish the public certificate first. If creating the private key then
    // fails, remove only the exact certificate file created by this call.
    create_private_identity_pair(
        certificate_path,
        identity.certificate_pem.as_bytes(),
        private_key_path,
        identity.private_key_pem.as_bytes(),
    )?;
    Ok(validation_time)
}

/// Materializes a validated migrated identity at its effective TLS paths.
/// Existing files must be the exact imported pair; partial or conflicting
/// paths never get repaired or replaced.
pub(super) fn materialize_imported_identity_pair(
    certificate_value: &str,
    private_key_value: &str,
    identity: &Identity,
    now: u64,
) -> Result<(), &'static str> {
    let certificate_path = Path::new(certificate_value);
    let private_key_path = Path::new(private_key_value);
    if same_path(certificate_path, private_key_path) {
        return Err("unsafe_identity_path");
    }
    crypto::validate_identity(identity, now).map_err(|_| "invalid_identity")?;

    match (
        inspect_target(certificate_path)?,
        inspect_target(private_key_path)?,
    ) {
        (TargetState::Existing, TargetState::Existing) => {
            let certificate = super::snapshot::read_identity_file(certificate_value)?;
            let private_key = super::snapshot::read_identity_file(private_key_value)?;
            if certificate != identity.certificate_pem || private_key != identity.private_key_pem {
                return Err("identity_file_conflict");
            }
            crypto::validate_identity(
                &Identity {
                    host_id: identity.host_id.clone(),
                    certificate_pem: certificate,
                    private_key_pem: private_key,
                },
                now,
            )
            .map_err(|_| "invalid_identity")?;
            amaterasu_storage::protect_machine_private_file(certificate_path)
                .map_err(|_| "identity_file_unavailable")?;
            amaterasu_storage::protect_machine_private_file(private_key_path)
                .map_err(|_| "identity_file_unavailable")?;
            Ok(())
        }
        (TargetState::Missing, TargetState::Missing) => {
            ensure_parent_directories(certificate_path)?;
            ensure_parent_directories(private_key_path)?;
            if inspect_target(certificate_path)? != TargetState::Missing
                || inspect_target(private_key_path)? != TargetState::Missing
            {
                return Err("identity_file_conflict");
            }
            create_private_identity_pair(
                certificate_path,
                identity.certificate_pem.as_bytes(),
                private_key_path,
                identity.private_key_pem.as_bytes(),
            )
        }
        _ => Err("identity_file_conflict"),
    }
}

fn current_time() -> Result<u64, &'static str> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|_| "clock_failure")
}

fn inspect_target(path: &Path) -> Result<TargetState, &'static str> {
    validate_path_shape(path)?;
    let components: Vec<_> = path.components().collect();
    let mut current = PathBuf::new();
    for (index, component) in components.iter().enumerate() {
        match component {
            Component::Prefix(prefix) => current.push(prefix.as_os_str()),
            Component::RootDir => current.push(component.as_os_str()),
            Component::Normal(name) => current.push(name),
            Component::CurDir | Component::ParentDir => return Err("unsafe_identity_path"),
        }
        if matches!(component, Component::Prefix(_)) {
            continue;
        }
        let is_target = index + 1 == components.len();
        let metadata = match fs::symlink_metadata(&current) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(TargetState::Missing);
            }
            Err(_) => return Err("identity_file_unavailable"),
        };
        if is_reparse_or_symlink(&metadata) {
            return Err("unsafe_identity_path");
        }
        if is_target {
            if !metadata.is_file() {
                return Err("invalid_identity_file");
            }
            let canonical = path
                .canonicalize()
                .map_err(|_| "identity_file_unavailable")?;
            if !same_path(&canonical, path) {
                return Err("unsafe_identity_path");
            }
            return Ok(TargetState::Existing);
        }
        if !metadata.is_dir() {
            return Err("unsafe_identity_path");
        }
    }
    Err("unsafe_identity_path")
}

fn validate_path_shape(path: &Path) -> Result<(), &'static str> {
    if !path.is_absolute()
        || path.as_os_str().to_string_lossy().len() > 4096
        || path.to_string_lossy().chars().any(char::is_control)
    {
        return Err("unsafe_identity_path");
    }
    if path
        .components()
        .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        return Err("unsafe_identity_path");
    }
    Ok(())
}

fn ensure_parent_directories(path: &Path) -> Result<(), &'static str> {
    validate_path_shape(path)?;
    let parent = path.parent().ok_or("unsafe_identity_path")?;
    let mut current = PathBuf::new();
    for component in parent.components() {
        match component {
            Component::Prefix(prefix) => current.push(prefix.as_os_str()),
            Component::RootDir => current.push(component.as_os_str()),
            Component::Normal(name) => {
                current.push(name);
                match fs::symlink_metadata(&current) {
                    Ok(metadata) if is_reparse_or_symlink(&metadata) || !metadata.is_dir() => {
                        return Err("unsafe_identity_path");
                    }
                    Ok(_) => (),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        create_private_directory(&current)?;
                    }
                    Err(_) => return Err("identity_file_unavailable"),
                }
            }
            Component::CurDir | Component::ParentDir => return Err("unsafe_identity_path"),
        }
    }
    let canonical = parent
        .canonicalize()
        .map_err(|_| "identity_file_unavailable")?;
    if !same_path(&canonical, parent) {
        return Err("unsafe_identity_path");
    }
    Ok(())
}

fn create_private_directory(path: &Path) -> Result<(), &'static str> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = fs::DirBuilder::new();
        builder.mode(0o700);
        match builder.create(path) {
            Ok(()) => (),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(_) => return Err("identity_file_unavailable"),
        }
    }
    #[cfg(not(unix))]
    match fs::create_dir(path) {
        Ok(()) => (),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
        Err(_) => return Err("identity_file_unavailable"),
    }

    let metadata = fs::symlink_metadata(path).map_err(|_| "identity_file_unavailable")?;
    if is_reparse_or_symlink(&metadata) || !metadata.is_dir() {
        return Err("unsafe_identity_path");
    }
    Ok(())
}

fn create_private_identity_pair(
    certificate_path: &Path,
    certificate_bytes: &[u8],
    private_key_path: &Path,
    private_key_bytes: &[u8],
) -> Result<(), &'static str> {
    let certificate_file = create_private_file(certificate_path, certificate_bytes)?;
    match create_private_file(private_key_path, private_key_bytes) {
        Ok(private_key_file) => {
            drop(private_key_file);
            drop(certificate_file);
            Ok(())
        }
        Err(error) => {
            remove_created_file_if_same(certificate_path, certificate_file);
            Err(error)
        }
    }
}

fn create_private_file(path: &Path, bytes: &[u8]) -> Result<File, &'static str> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
        #[cfg(any(target_os = "linux", target_os = "android"))]
        const O_NOFOLLOW: i32 = 0x0002_0000;
        #[cfg(any(
            target_os = "macos",
            target_os = "ios",
            target_os = "freebsd",
            target_os = "openbsd",
            target_os = "netbsd",
            target_os = "dragonfly"
        ))]
        const O_NOFOLLOW: i32 = 0x0000_0100;
        #[cfg(any(
            target_os = "linux",
            target_os = "android",
            target_os = "macos",
            target_os = "ios",
            target_os = "freebsd",
            target_os = "openbsd",
            target_os = "netbsd",
            target_os = "dragonfly"
        ))]
        options.custom_flags(O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }

    let mut file: File = options
        .open(path)
        .map_err(|_| "identity_file_unavailable")?;
    if file
        .write_all(bytes)
        .and_then(|()| file.sync_all())
        .is_err()
    {
        remove_created_file_if_same(path, file);
        return Err("identity_file_unavailable");
    }
    if amaterasu_storage::protect_machine_private_file(path).is_err() {
        remove_created_file_if_same(path, file);
        return Err("identity_file_unavailable");
    }
    Ok(file)
}

fn remove_created_file_if_same(path: &Path, file: File) {
    let Ok(created_metadata) = file.metadata() else {
        return;
    };
    drop(file);
    let Ok(current_metadata) = fs::symlink_metadata(path) else {
        return;
    };
    if is_reparse_or_symlink(&current_metadata) || !same_file(&created_metadata, &current_metadata)
    {
        return;
    }
    let _ = fs::remove_file(path);
}

#[cfg(unix)]
fn same_file(created: &fs::Metadata, current: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    created.dev() == current.dev() && created.ino() == current.ino()
}

#[cfg(windows)]
fn same_file(created: &fs::Metadata, current: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    created.volume_serial_number().is_some()
        && created.volume_serial_number() == current.volume_serial_number()
        && created.file_index().is_some()
        && created.file_index() == current.file_index()
}

#[cfg(not(any(unix, windows)))]
fn same_file(_created: &fs::Metadata, _current: &fs::Metadata) -> bool {
    false
}

fn is_reparse_or_symlink(metadata: &fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        false
    }
}

fn same_path(left: &Path, right: &Path) -> bool {
    #[cfg(windows)]
    {
        let normalize = |path: &Path| {
            let value = path.to_string_lossy();
            value
                .strip_prefix(r"\\?\")
                .unwrap_or(value.as_ref())
                .replace('/', "\\")
        };
        normalize(left).eq_ignore_ascii_case(&normalize(right))
    }
    #[cfg(not(windows))]
    {
        left == right
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct TemporaryDirectory(PathBuf);

    impl TemporaryDirectory {
        fn new() -> Self {
            static SEQUENCE: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "amaterasu-pairing-provision-{}-{}",
                std::process::id(),
                SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TemporaryDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn second_identity_file_failure_removes_only_the_first_file_created_here() {
        let directory = TemporaryDirectory::new();
        let certificate_path = directory.0.join("certificate.pem");
        let private_key_path = directory.0.join("private-key.pem");
        fs::create_dir(&private_key_path).unwrap();

        assert_eq!(
            create_private_identity_pair(
                &certificate_path,
                b"test-certificate",
                &private_key_path,
                b"test-private-key",
            ),
            Err("identity_file_unavailable")
        );
        assert!(!certificate_path.exists());
        assert!(private_key_path.is_dir());
    }
}
