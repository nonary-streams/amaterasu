//! What the host says about the set it loaded, read once when the module
//! starts. A module instance never outlives its set: the host unloads every
//! module before it changes the selection.
use crate::host_activation::{self, ReceiptStatus};
use crate::package_status;
use amaterasu_package::ReleaseManifest;
use amaterasu_package::repository::{RepositoryStore, observe_native_selection};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub(crate) struct Environment {
    /// The digest of the set the host verified and loaded.
    pub running_digest: Option<String>,
    /// That set's release sequence, when its manifest is readable.
    pub running_sequence: Option<u64>,
    /// Absent when the selection records no repository as the set's supplier.
    pub source: Option<Arc<UpdateSource>>,
    /// The host's last activation of a set from that repository, or its last
    /// return to a retained set, failed.
    pub last_activation_failed: bool,
    /// The host root, when it is the real directory startup composition named.
    pub package_root: Option<PathBuf>,
    /// Where the approved repositories are recorded.
    pub repository_directory: Option<PathBuf>,
}

/// The running set and the repository recorded as its supplier: everything a
/// check or download needs. Both directories come from startup composition.
pub(crate) struct UpdateSource {
    pub repository_directory: PathBuf,
    pub package_root: PathBuf,
    pub repository: String,
    /// The publisher key the running set was accepted under.
    pub fingerprint: String,
    pub running: RunningSet,
}

pub(crate) struct RunningSet {
    pub digest: String,
    pub manifest: ReleaseManifest,
    /// Where the host keeps the running set's verified files.
    pub directory: PathBuf,
    /// The highest release sequence the host has accepted.
    pub accepted_sequence: u64,
}

impl Environment {
    /// Reads the running set's manifest and provenance. Anything unreadable
    /// leaves the corresponding part absent; it never stands in a guess.
    pub(crate) fn load(
        repository_directory: Option<PathBuf>,
        package_root: Option<PathBuf>,
        running_digest: Option<String>,
    ) -> Self {
        let mut environment = Self {
            running_digest: running_digest.filter(|digest| package_status::is_sha256(digest)),
            running_sequence: None,
            source: None,
            last_activation_failed: false,
            package_root: None,
            repository_directory: repository_directory
                .clone()
                .filter(|directory| directory.is_absolute()),
        };
        let Some(digest) = environment.running_digest.clone() else {
            return environment;
        };
        let Some(root) = package_root
            .as_deref()
            .and_then(Path::to_str)
            .and_then(package_status::trusted_root)
        else {
            return environment;
        };
        environment.package_root = Some(root.to_path_buf());
        environment.last_activation_failed = last_restore_failed(root, &digest);
        let Some(manifest) = package_status::running_manifest(root, &digest) else {
            return environment;
        };
        environment.running_sequence = Some(manifest.sequence);
        let Some(source) = update_source(repository_directory, root, digest, manifest) else {
            return environment;
        };
        environment.last_activation_failed |= last_activation_failed(&source);
        environment.source = Some(Arc::new(source));
        environment
    }
}

/// The supplier the selection records for the running set, with the set.
fn update_source(
    repository_directory: Option<PathBuf>,
    package_root: &Path,
    digest: String,
    manifest: ReleaseManifest,
) -> Option<UpdateSource> {
    let repository_directory = repository_directory.filter(|directory| directory.is_absolute())?;
    let observed = observe_native_selection(package_root).ok()?;
    let supplier = observed.repository_sets.get(&digest)?;
    if !package_status::is_repository_id(&supplier.repository_id)
        || !package_status::is_sha256(&supplier.key_fingerprint)
    {
        return None;
    }
    let directory = amaterasu_package::package_directory(package_root, &digest).ok()?;
    Some(UpdateSource {
        repository_directory,
        package_root: package_root.to_path_buf(),
        repository: supplier.repository_id.clone(),
        fingerprint: supplier.key_fingerprint.clone(),
        running: RunningSet {
            digest,
            manifest,
            directory,
            accepted_sequence: observed.selection.highest_sequence,
        },
    })
}

/// True when the host's receipt says it failed to activate a set this
/// repository's cache holds. The receipt has one slot for every activation,
/// so a set cached from another repository is another flow's failure, and a
/// receipt naming the running set is not a failure to reach it.
fn last_activation_failed(source: &UpdateSource) -> bool {
    let Some(receipt) = host_activation::receipt(&source.package_root) else {
        return false;
    };
    if receipt.status != ReceiptStatus::Failed || receipt.digest == source.running.digest {
        return false;
    }
    RepositoryStore::new(source.repository_directory.clone())
        .and_then(|store| {
            store.existing_native_candidate_cache_path(&source.repository, &receipt.digest)
        })
        .ok()
        .and_then(|cached| std::fs::symlink_metadata(cached).ok())
        .is_some_and(|metadata| metadata.is_dir())
}

/// True when the host's receipt says it failed to return to a retained set:
/// one the selection records as having run healthy here, other than the set
/// that is running. A set that only ever sat in a repository's cache is an
/// update's failure, which `last_activation_failed` reads.
fn last_restore_failed(package_root: &Path, running: &str) -> bool {
    let Some(receipt) = host_activation::receipt(package_root) else {
        return false;
    };
    if receipt.status != ReceiptStatus::Failed || receipt.digest == running {
        return false;
    }
    amaterasu_package::read_bounded(&package_root.join("selection.json"), 1024 * 1024)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<amaterasu_package::ActivationState>(&bytes).ok())
        .is_some_and(|selection| selection.healthy_sets.contains(&receipt.digest))
}
