//! What the host says about the set it loaded, read once when the module
//! starts. A module instance never outlives its set: the host unloads every
//! module before it changes the selection.
use crate::host_activation::{self, Receipt, ReceiptStatus};
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
    /// The set the host's last attempt failed to reach, by digest: one from
    /// that repository it could not activate, or a retained set it could not
    /// return to.
    pub failed_activation: Option<String>,
    /// What the host's receipt says against an activation nobody watches.
    pub unattended_caution: UnattendedCaution,
    /// The host root, when it is the real directory startup composition named.
    pub package_root: Option<PathBuf>,
    /// Where the approved repositories are recorded.
    pub repository_directory: Option<PathBuf>,
}

/// What the host's record of its last activation attempt says against asking
/// it for another one unattended. A set that does not start can cost a
/// restart of the service at every attempt, so nothing that went wrong is
/// tried again on a schedule.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) enum UnattendedCaution {
    /// The last attempt ended well, or the host records none.
    #[default]
    None,
    /// The host failed to activate this set, or started on it and never
    /// finished. It is not the set that is running.
    Set(String),
    /// The host recovered an interrupted activation. Its receipt then names
    /// the running set, so the set that failed is not known.
    AnySet,
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
            failed_activation: None,
            unattended_caution: UnattendedCaution::None,
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
        // One receipt speaks for every attempt, whichever flow asked for it.
        let receipt = host_activation::receipt(root);
        environment.failed_activation = receipt
            .as_ref()
            .and_then(|receipt| failed_restore(root, &digest, receipt));
        let Some(manifest) = package_status::running_manifest(root, &digest) else {
            return environment;
        };
        environment.running_sequence = Some(manifest.sequence);
        let Some(source) = update_source(repository_directory, root, digest, manifest) else {
            return environment;
        };
        if let Some(receipt) = receipt {
            environment.unattended_caution = unattended_caution(&source.running.digest, &receipt);
            if let Some(failed) = failed_activation(&source, receipt) {
                environment.failed_activation = Some(failed);
            }
        }
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

/// The set the host's receipt says it failed to activate, when this
/// repository's cache holds it. The receipt has one slot for every
/// activation, so a set cached from another repository is another flow's
/// failure, and a receipt naming the running set is not a failure to reach it.
fn failed_activation(source: &UpdateSource, receipt: Receipt) -> Option<String> {
    if receipt.status != ReceiptStatus::Failed || receipt.digest == source.running.digest {
        return None;
    }
    RepositoryStore::new(source.repository_directory.clone())
        .and_then(|store| {
            store.existing_native_candidate_cache_path(&source.repository, &receipt.digest)
        })
        .ok()
        .and_then(|cached| std::fs::symlink_metadata(cached).ok())
        .is_some_and(|metadata| metadata.is_dir())
        .then_some(receipt.digest)
}

/// Reads the receipt for any sign that the host's last attempt went wrong,
/// whichever flow asked for it. The host writes `activating` before it starts
/// and `committed` once the set runs. It writes `failed` for the set it could
/// not reach, and for the running set when it rolled an interrupted
/// activation back at startup.
fn unattended_caution(running_digest: &str, receipt: &Receipt) -> UnattendedCaution {
    match (receipt.status, receipt.digest == running_digest) {
        (ReceiptStatus::Committed, _) | (ReceiptStatus::Activating, true) => {
            UnattendedCaution::None
        }
        (ReceiptStatus::Failed, true) => UnattendedCaution::AnySet,
        (ReceiptStatus::Failed | ReceiptStatus::Activating, false) => {
            UnattendedCaution::Set(receipt.digest.clone())
        }
    }
}

/// The retained set the host's receipt says it failed to return to: one the
/// selection records as having run healthy here, other than the set that is
/// running. A set that only ever sat in a repository's cache is an update's
/// failure, which `failed_activation` reads.
fn failed_restore(package_root: &Path, running: &str, receipt: &Receipt) -> Option<String> {
    if receipt.status != ReceiptStatus::Failed || receipt.digest == running {
        return None;
    }
    amaterasu_package::read_bounded(&package_root.join("selection.json"), 1024 * 1024)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<amaterasu_package::ActivationState>(&bytes).ok())
        .is_some_and(|selection| selection.healthy_sets.contains(&receipt.digest))
        .then(|| receipt.digest.clone())
}
