//! Download a publisher-signed RTX HDR component set using the Network SDK.
//!
//! This only prepares a candidate in the repository cache. The host must check
//! current approvals and verify the cached set again at the activation boundary.
use crate::repository_transport::NetworkRepository;
use amaterasu_module_sdk::network::Network;
use amaterasu_package::repository::{
    Candidate, CandidateKind, RepositoryListing, RepositoryStore, download_candidate,
    refresh_catalog, verify_candidate,
};
use amaterasu_package::{
    BootstrapConfig, PackageResult, current_target, random_identifier, read_json, unix_timestamp,
};
use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

const SLOT: &str = "amaterasu.rtx-hdr";
const IMPLEMENTATION: &str = "amaterasu-linux-session-rtx-hdr";
const OFFICIAL_REPOSITORY: &str = "amaterasu-rtx-hdr";
const DOWNLOAD_LIMIT: std::time::Duration = std::time::Duration::from_secs(600);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PreparedCandidate {
    pub repository_id: String,
    pub catalog_digest: String,
    pub digest: String,
}

/// Both paths come from trusted startup composition, never from a browser.
/// The caller must use an asynchronous worker and join it before module unload.
pub(crate) fn prepare(
    repository_directory: &Path,
    package_root: &Path,
    network: Network,
    cancelled: Arc<AtomicBool>,
    transferred: Arc<AtomicU64>,
) -> PackageResult<PreparedCandidate> {
    if !repository_directory.is_absolute() || !package_root.is_absolute() {
        return Err("candidate roots must be absolute".into());
    }
    check_cancelled(&cancelled)?;
    let bootstrap: BootstrapConfig = read_json(&package_root.join("bootstrap.json"))?;
    let store = RepositoryStore::new(repository_directory.to_path_buf())?;
    let now = unix_timestamp();
    let target = current_target();
    if let Ok(approved) = store.approved(OFFICIAL_REPOSITORY) {
        let transport = NetworkRepository::new(
            &approved.location,
            network.clone(),
            cancelled.clone(),
            transferred.clone(),
            DOWNLOAD_LIMIT,
        );
        refresh_catalog(&store, OFFICIAL_REPOSITORY, &transport, &target, now)?;
    }
    let (repository_id, candidate) = choose_candidate(store.list(&target, now)?)?;
    let approved = store.approved(&repository_id)?;
    let catalog = store.catalog(&repository_id, &target, now)?;
    let reference = PreparedCandidate {
        repository_id: repository_id.clone(),
        catalog_digest: catalog.digest().into(),
        digest: candidate.manifest_sha256.clone(),
    };
    let destination =
        store.native_candidate_cache_path(&repository_id, &candidate.manifest_sha256)?;
    check_cancelled(&cancelled)?;
    if destination
        .try_exists()
        .map_err(|error| error.to_string())?
    {
        verify_cached_directory(&destination)?;
        verify_candidate(
            &approved,
            &catalog,
            &candidate,
            &destination,
            &bootstrap,
            unix_timestamp(),
        )?;
        return Ok(reference);
    }
    let parent = destination
        .parent()
        .ok_or("candidate cache has no parent")?;
    let temporary = TemporaryDirectory::create(parent)?;
    let source = temporary.path.join("package");
    let transport = NetworkRepository::new(
        &approved.location,
        network,
        cancelled.clone(),
        transferred,
        DOWNLOAD_LIMIT,
    );
    download_candidate(
        &transport,
        &approved,
        &catalog,
        &candidate,
        &source,
        &bootstrap,
        unix_timestamp(),
    )?;
    check_cancelled(&cancelled)?;
    match publish_without_replacement(&source, &destination) {
        Ok(()) => fs::File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| error.to_string())?,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            verify_cached_directory(&destination)?;
            verify_candidate(
                &approved,
                &catalog,
                &candidate,
                &destination,
                &bootstrap,
                unix_timestamp(),
            )?;
        }
        Err(error) => return Err(error.to_string()),
    }
    check_cancelled(&cancelled)?;
    // Approval, catalog and bootstrap may have changed while bytes were fetched;
    // the host MUST re-read and verify them atomically before activation.
    Ok(reference)
}

fn verify_cached_directory(path: &Path) -> PackageResult<()> {
    let metadata = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err("candidate cache is not a real directory".into());
    }
    Ok(())
}

fn choose_candidate(repositories: Vec<RepositoryListing>) -> PackageResult<(String, Candidate)> {
    let mut choices = Vec::new();
    for repository in repositories {
        if repository.revoked {
            continue;
        }
        for candidate in repository.candidates {
            if candidate.kind == CandidateKind::NativeSet
                && candidate.slot == SLOT
                && candidate.implementation == IMPLEMENTATION
                && candidate.version == env!("CARGO_PKG_VERSION")
            {
                choices.push((repository.id.clone(), candidate));
            }
        }
    }
    if choices.len() != 1 {
        return Err("expected exactly one approved RTX HDR native candidate".into());
    }
    Ok(choices.pop().expect("one candidate exists"))
}

fn check_cancelled(cancelled: &AtomicBool) -> PackageResult<()> {
    if cancelled.load(Ordering::Acquire) {
        Err("candidate download cancelled".into())
    } else {
        Ok(())
    }
}

struct TemporaryDirectory {
    path: PathBuf,
}
impl TemporaryDirectory {
    fn create(parent: &Path) -> PackageResult<Self> {
        use std::os::unix::fs::DirBuilderExt;
        let path = parent.join(format!(".rtx-hdr-download-{}", random_identifier()?));
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .map_err(|error| error.to_string())?;
        Ok(Self { path })
    }
}
impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn publish_without_replacement(source: &Path, target: &Path) -> io::Result<()> {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};
    let source =
        CString::new(source.as_os_str().as_bytes()).map_err(|_| io::ErrorKind::InvalidInput)?;
    let target =
        CString::new(target.as_os_str().as_bytes()).map_err(|_| io::ErrorKind::InvalidInput)?;
    // SAFETY: both C strings are live and NUL terminated; AT_FDCWD permits
    // absolute paths, and RENAME_NOREPLACE prevents replacing any prior cache.
    let result = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            source.as_ptr(),
            libc::AT_FDCWD,
            target.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(digest: char) -> Candidate {
        Candidate {
            slot: SLOT.into(),
            implementation: IMPLEMENTATION.into(),
            version: "0.1.0".into(),
            kind: CandidateKind::NativeSet,
            manifest_sha256: digest.to_string().repeat(64),
        }
    }
    fn repository(id: &str, revoked: bool, candidates: Vec<Candidate>) -> RepositoryListing {
        RepositoryListing {
            id: id.into(),
            location: "https://packages.example.test".into(),
            fingerprint: "a".repeat(64),
            revoked,
            candidates,
            catalog: amaterasu_package::repository::CatalogListing::Missing,
        }
    }
    #[test]
    fn candidate_selection_refuses_missing_or_ambiguous_publishers() {
        assert!(choose_candidate(vec![]).is_err());
        assert!(choose_candidate(vec![repository("revoked", true, vec![candidate('a')])]).is_err());
        assert!(
            choose_candidate(vec![
                repository("one", false, vec![candidate('a')]),
                repository("two", false, vec![candidate('b')])
            ])
            .is_err()
        );
        let mut older = candidate('c');
        older.version = "0.0.1".into();
        assert!(
            choose_candidate(vec![repository("historical", false, vec![older.clone()])]).is_err()
        );
        let (source, item) = choose_candidate(vec![
            repository("one", false, vec![candidate('a'), older]),
            repository("revoked", true, vec![candidate('b')]),
        ])
        .unwrap();
        assert_eq!(source, "one");
        assert_eq!(item.manifest_sha256, "a".repeat(64));
    }
    #[test]
    fn cache_publish_never_replaces_existing_content_and_cleans_temporary_parent() {
        let root = std::env::temp_dir().join(format!(
            "amaterasu-rtx-candidate-{}",
            random_identifier().unwrap()
        ));
        fs::create_dir(&root).unwrap();
        let existing = root.join("existing");
        fs::create_dir(&existing).unwrap();
        fs::write(existing.join("manifest"), b"original").unwrap();
        let alias = root.join("alias");
        std::os::unix::fs::symlink(&existing, &alias).unwrap();
        assert!(verify_cached_directory(&alias).is_err());
        assert!(verify_cached_directory(&existing).is_ok());
        let temporary = TemporaryDirectory::create(&root).unwrap();
        let source = temporary.path.join("package");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("manifest"), b"replacement").unwrap();
        assert_eq!(
            publish_without_replacement(&source, &existing)
                .unwrap_err()
                .kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(fs::read(existing.join("manifest")).unwrap(), b"original");
        let temp_path = temporary.path.clone();
        drop(temporary);
        assert!(!temp_path.exists());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn cancelled_preparation_fails_before_io() {
        assert!(check_cancelled(&AtomicBool::new(true)).is_err());
    }
}
