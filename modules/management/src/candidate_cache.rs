//! Places a publisher-signed native candidate in the repository cache.
//!
//! The cache holds prepared bytes only. The host re-reads the approval, the
//! catalog and every byte of a cached set before it trials that set.
use amaterasu_package::repository::{
    ApprovedRepository, Candidate, RepositoryStore, RepositoryTransport, VerifiedCatalog,
    download_candidate, verify_candidate,
};
use amaterasu_package::{BootstrapConfig, PackageResult, random_identifier, unix_timestamp};
use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

/// The identity the host needs to find and re-verify a cached candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PreparedCandidate {
    pub repository_id: String,
    pub catalog_digest: String,
    pub digest: String,
}

/// Host-owned state that decides whether one candidate may be cached.
#[derive(Clone, Copy)]
pub(crate) struct Placement<'a> {
    pub store: &'a RepositoryStore,
    pub approved: &'a ApprovedRepository,
    pub catalog: &'a VerifiedCatalog,
    pub candidate: &'a Candidate,
    pub bootstrap: &'a BootstrapConfig,
}

/// Leaves the fully verified candidate in its digest-addressed cache entry.
///
/// An entry that already exists is verified and kept; it is never replaced.
/// A new one is downloaded beside the cache, verified there and published by
/// an atomic rename, so the cache never holds an unverified set.
pub(crate) fn place_verified(
    placement: &Placement<'_>,
    transport: &impl RepositoryTransport,
    cancelled: &AtomicBool,
) -> PackageResult<PreparedCandidate> {
    let Placement {
        store,
        approved,
        catalog,
        candidate,
        bootstrap,
    } = *placement;
    let reference = PreparedCandidate {
        repository_id: approved.id.clone(),
        catalog_digest: catalog.digest().into(),
        digest: candidate.manifest_sha256.clone(),
    };
    let destination =
        store.native_candidate_cache_path(&approved.id, &candidate.manifest_sha256)?;
    check_cancelled(cancelled)?;
    let verify_cached = || {
        verify_cached_directory(&destination)?;
        verify_candidate(
            approved,
            catalog,
            candidate,
            &destination,
            bootstrap,
            unix_timestamp(),
        )
    };
    if destination
        .try_exists()
        .map_err(|error| error.to_string())?
    {
        verify_cached()?;
        return Ok(reference);
    }
    let parent = destination
        .parent()
        .ok_or("candidate cache has no parent")?;
    let temporary = TemporaryDirectory::create(parent)?;
    let source = temporary.path.join("package");
    download_candidate(
        transport,
        approved,
        catalog,
        candidate,
        &source,
        bootstrap,
        unix_timestamp(),
    )?;
    check_cancelled(cancelled)?;
    match publish_without_replacement(&source, &destination) {
        Ok(()) => fs::File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| error.to_string())?,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => verify_cached()?,
        Err(error) => return Err(error.to_string()),
    }
    check_cancelled(cancelled)?;
    // Approval, catalog and bootstrap may have changed while bytes were fetched;
    // the host MUST re-read and verify them atomically before activation.
    Ok(reference)
}

pub(crate) fn check_cancelled(cancelled: &AtomicBool) -> PackageResult<()> {
    if cancelled.load(Ordering::Acquire) {
        Err("candidate download cancelled".into())
    } else {
        Ok(())
    }
}

fn verify_cached_directory(path: &Path) -> PackageResult<()> {
    let metadata = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err("candidate cache is not a real directory".into());
    }
    Ok(())
}

struct TemporaryDirectory {
    path: PathBuf,
}

impl TemporaryDirectory {
    fn create(parent: &Path) -> PackageResult<Self> {
        use std::os::unix::fs::DirBuilderExt;
        let path = parent.join(format!(".candidate-download-{}", random_identifier()?));
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

    #[test]
    fn cache_publish_never_replaces_existing_content_and_cleans_temporary_parent() {
        let root = std::env::temp_dir().join(format!(
            "amaterasu-candidate-cache-{}",
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
