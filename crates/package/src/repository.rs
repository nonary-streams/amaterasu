//! Publisher approval and offline verification of repository choices.
//!
//! A repository URL locates bytes; only a separately confirmed public-key
//! fingerprint establishes publisher identity. Staging changes only the next
//! development selection; native loading happens later in a worker.
mod store;
mod transport;
use crate::{ActivationState, NativeReleaseFloor, RepositorySet, RootLock, write_json_atomically};
use crate::{
    BootstrapConfig, PackageResult, ReleaseSignature, VerificationPolicy, current_target,
    decode_hex, encode_hex, package_directory, read_bounded, read_json, sha256,
    verify_detached_signature, verify_package,
};
use ed25519_dalek::VerifyingKey;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::Path;

const SELECTION_REVISION_CONFLICT: &str = "selection_revision_conflict";

/// Read-only, privacy-limited view of native activation state. `revision` is
/// the SHA-256 of the exact persisted selection bytes and is used for CAS.
#[derive(Clone, Debug, Serialize)]
pub struct NativeSelectionSnapshot {
    pub revision: String,
    pub active: Option<String>,
    pub previous: Option<String>,
    pub pending: Option<String>,
    pub trial: Option<String>,
    pub highest_sequence: u64,
}

/// Conflicts are safe to report as a stale revision; all other failures may
/// have happened after durable work and should be reconciled by reading status.
#[derive(Debug)]
pub enum NativeSelectionError {
    Conflict,
    Failed(String),
}

impl NativeSelectionError {
    pub fn is_conflict(&self) -> bool {
        matches!(self, Self::Conflict)
    }
}

impl fmt::Display for NativeSelectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Conflict => formatter.write_str(SELECTION_REVISION_CONFLICT),
            Self::Failed(_) => formatter.write_str("native_selection_operation_failed"),
        }
    }
}

impl std::error::Error for NativeSelectionError {}

fn selection_operation_error(error: String) -> NativeSelectionError {
    if error == SELECTION_REVISION_CONFLICT {
        NativeSelectionError::Conflict
    } else {
        NativeSelectionError::Failed(error)
    }
}

/// Reads activation state and its opaque revision while holding the activation
/// lock. A missing selection is represented by the default state and the hash
/// of empty bytes, so it cannot collide with a persisted default document.
pub fn read_native_selection(root: &Path) -> PackageResult<NativeSelectionSnapshot> {
    let _lock = RootLock::acquire(root)?;
    let (selection, revision) = read_selection_with_revision(root)?;
    Ok(snapshot(&selection, revision))
}

/// Verify a production candidate while holding its repository approval lock.
///
/// The caller must already hold the host root lock and authorize the request.
/// The closure executes with current approval locked, and must enforce its own
/// product policy for the candidate. This function does not stage or select it.
/// Repository locks use nonblocking acquisition; contention returns an error.
/// Cached bytes can change, so a caller that copies them must verify the copy.
#[allow(clippy::too_many_arguments)]
pub fn with_verified_production_candidate<T>(
    root: &Path,
    bootstrap: &BootstrapConfig,
    store: &RepositoryStore,
    repository_id: &str,
    catalog_digest: &str,
    manifest_digest: &str,
    expected_active: &str,
    expected_revision: &str,
    now: u64,
    operation: impl FnOnce(&Path, &crate::VerifiedPackage, &Candidate) -> PackageResult<T>,
) -> Result<T, NativeSelectionError> {
    let result = (|| {
        if bootstrap.development || bootstrap.allow_unsigned || bootstrap.trusted_keys.is_empty() {
            return Err("production verification requires a signed production bootstrap".into());
        }
        validate_revision(expected_revision)?;
        if !canonical_digest(expected_active) || !canonical_digest(manifest_digest) {
            return Err("invalid native package digest".into());
        }
        let current: BootstrapConfig = read_json(&root.join("bootstrap.json"))?;
        if serde_json::to_vec(&current).map_err(|error| error.to_string())?
            != serde_json::to_vec(bootstrap).map_err(|error| error.to_string())?
        {
            return Err("host bootstrap changed; reload before native package operation".into());
        }
        let (selection, revision) = read_selection_with_revision(root)?;
        if revision != expected_revision || selection.active.as_deref() != Some(expected_active) {
            return Err(SELECTION_REVISION_CONFLICT.into());
        }
        if selection.pending.is_some() || selection.trial.is_some() {
            return Err("native activation already in progress".into());
        }
        let active = verify_package(
            &package_directory(root, expected_active)?,
            bootstrap,
            VerificationPolicy {
                minimum_sequence: selection.highest_sequence,
                allow_historical: false,
                now,
            },
        )?;
        if !active.signed
            || active.digest != expected_active
            || active.manifest.host_context_schema != 1
        {
            return Err("active production selection lost authenticated identity".into());
        }
        let source = store.existing_native_candidate_cache_path(repository_id, manifest_digest)?;
        let metadata = std::fs::symlink_metadata(&source)
            .map_err(|error| format!("cached native candidate is unavailable: {error}"))?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err("cached native candidate is not a private package directory".into());
        }
        store.with_current_catalog(
            repository_id,
            catalog_digest,
            &current_target(),
            now,
            |approved, catalog| {
                let mut candidates = catalog.candidates().iter().filter(|candidate| {
                    candidate.kind == CandidateKind::NativeSet
                        && candidate.manifest_sha256 == manifest_digest
                });
                let candidate = candidates
                    .next()
                    .ok_or("native candidate is absent from the current catalog")?;
                if candidates.next().is_some() {
                    return Err("native candidate digest has ambiguous catalog entries".into());
                }
                if bootstrap.trusted_keys.get(&approved.fingerprint) != Some(&approved.public_key) {
                    return Err(
                        "approved repository signer is not trusted by the production bootstrap"
                            .into(),
                    );
                }
                verify_candidate(approved, catalog, candidate, &source, bootstrap, now)?;
                let package = verify_package(
                    &source,
                    bootstrap,
                    VerificationPolicy {
                        minimum_sequence: selection.highest_sequence,
                        allow_historical: false,
                        now,
                    },
                )?;
                if !package.signed
                    || package.digest != manifest_digest
                    || package.manifest.host_context_schema != 1
                    || package.signer_fingerprint.as_deref() != Some(approved.fingerprint.as_str())
                {
                    return Err("production candidate signature, ABI or digest changed".into());
                }
                if package.manifest.sequence == selection.highest_sequence
                    && package.digest != expected_active
                {
                    return Err("production native release sequence was equivocated".into());
                }
                if let Some(floor) = selection.repository_floors.get(&approved.fingerprint)
                    && (package.manifest.sequence < floor.sequence
                        || (package.manifest.sequence == floor.sequence
                            && package.digest != floor.digest))
                {
                    return Err("production candidate violates publisher release floor".into());
                }
                operation(&source, &package, candidate)
            },
        )
    })();
    result.map_err(selection_operation_error)
}

/// Policy and package inputs for a production trial. The caller must load the
/// allowlist and approval from independently protected host authority; neither
/// the repository nor the requesting component may supply that authority.
pub struct ProductionTrialPolicy<'a> {
    pub current_bytes: &'a [u8],
    pub candidate_bytes: &'a [u8],
    pub allowlist: &'a crate::policy_approval::PolicyAllowlist,
    pub approval: &'a crate::policy_approval::PolicyApproval,
}

/// Verify the signed candidate and the exact authorized policy together while
/// the repository approval lock is held. This does not stage or activate it.
/// The host must hold its root lock, and must reverify any copied artifacts.
#[allow(clippy::too_many_arguments)]
pub fn with_verified_production_trial<T>(
    root: &Path,
    bootstrap: &BootstrapConfig,
    store: &RepositoryStore,
    repository_id: &str,
    catalog_digest: &str,
    manifest_digest: &str,
    expected_active: &str,
    expected_revision: &str,
    now: u64,
    policy: ProductionTrialPolicy<'_>,
    operation: impl FnOnce(&Path, &crate::VerifiedPackage, &BootstrapConfig) -> PackageResult<T>,
) -> Result<T, NativeSelectionError> {
    if read_bounded(&root.join("bootstrap.json"), 1024 * 1024)
        .map_err(NativeSelectionError::Failed)?
        != policy.current_bytes
    {
        return Err(NativeSelectionError::Conflict);
    }
    with_verified_production_candidate(
        root,
        bootstrap,
        store,
        repository_id,
        catalog_digest,
        manifest_digest,
        expected_active,
        expected_revision,
        now,
        |path, verified, _candidate| {
            let publisher = verified
                .signer_fingerprint
                .as_deref()
                .ok_or("candidate signer is unavailable")?;
            let context = crate::policy_approval::PolicyCandidateContext {
                selection_revision: expected_revision,
                publisher_fingerprint: publisher,
                manifest_sha256: manifest_digest,
            };
            let trial_bootstrap = crate::policy_approval::verify_policy_candidate(
                policy.allowlist,
                policy.approval,
                &context,
                policy.current_bytes,
                policy.candidate_bytes,
            )?;
            let trial = verify_package(
                path,
                &trial_bootstrap,
                VerificationPolicy {
                    minimum_sequence: verified.manifest.sequence,
                    allow_historical: false,
                    now,
                },
            )?;
            if !trial.signed
                || trial.digest != verified.digest
                || trial.signer_fingerprint != verified.signer_fingerprint
                || trial.manifest.host_context_schema != 1
            {
                return Err("candidate changed while preparing authorized trial".into());
            }
            operation(path, &trial, &trial_bootstrap)
        },
    )
}

/// Copy an authorized production trial into the host-owned set store and
/// reverify the copy before the host attempts to load it. This does not change
/// the active selection or release floor. The caller holds the host root lock.
#[allow(clippy::too_many_arguments)]
pub fn stage_verified_production_trial<T>(
    root: &Path,
    bootstrap: &BootstrapConfig,
    store: &RepositoryStore,
    repository_id: &str,
    catalog_digest: &str,
    manifest_digest: &str,
    expected_active: &str,
    expected_revision: &str,
    now: u64,
    policy: ProductionTrialPolicy<'_>,
    operation: impl FnOnce(&Path, &crate::VerifiedPackage, &BootstrapConfig) -> PackageResult<T>,
) -> Result<T, NativeSelectionError> {
    with_verified_production_trial(
        root,
        bootstrap,
        store,
        repository_id,
        catalog_digest,
        manifest_digest,
        expected_active,
        expected_revision,
        now,
        policy,
        |source, verified, trial_bootstrap| {
            let verification = VerificationPolicy {
                minimum_sequence: verified.manifest.sequence,
                allow_historical: false,
                now,
            };
            crate::staging::install_immutable_set(
                source,
                root,
                trial_bootstrap,
                verification,
                verified,
            )?;
            let staged_path = package_directory(root, &verified.digest)?;
            let staged = verify_package(&staged_path, trial_bootstrap, verification)?;
            if !staged.signed
                || staged.digest != verified.digest
                || staged.signer_fingerprint != verified.signer_fingerprint
                || staged.manifest.host_context_schema != 1
            {
                return Err("staged trial does not match authorized candidate".into());
            }
            operation(&staged_path, &staged, trial_bootstrap)
        },
    )
}

/// Verifies and stages the exact candidate named by a current approved catalog,
/// using only the digest-derived cache entry owned by `RepositoryStore`.
/// Approval lock order is repository store first, then activation root.
// These parameters are the explicit trust and CAS context for one operation;
// bundling them would obscure which host-owned values authorize the mutation.
#[allow(clippy::too_many_arguments)]
pub fn stage_cached_native_candidate(
    root: &Path,
    bootstrap: &BootstrapConfig,
    store: &RepositoryStore,
    repository_id: &str,
    catalog_digest: &str,
    candidate: &Candidate,
    expected_revision: &str,
    now: u64,
) -> Result<NativeSelectionSnapshot, NativeSelectionError> {
    let result = (|| {
        if !bootstrap.development || candidate.kind != CandidateKind::NativeSet {
            return Err("repository native staging requires a development native choice".into());
        }
        validate_revision(expected_revision)?;
        let cache_path =
            store.native_candidate_cache_path(repository_id, &candidate.manifest_sha256)?;
        let metadata = std::fs::symlink_metadata(&cache_path)
            .map_err(|error| format!("cached native candidate is unavailable: {error}"))?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err("cached native candidate is not a private package directory".into());
        }
        store.with_current_catalog(
            repository_id,
            catalog_digest,
            &current_target(),
            now,
            |approved, catalog| {
                stage_native_candidate_with_revision(
                    &cache_path,
                    root,
                    bootstrap,
                    approved,
                    catalog,
                    candidate,
                    now,
                    Some(expected_revision),
                )
            },
        )
    })();
    result.map_err(selection_operation_error)?;
    read_native_selection(root).map_err(NativeSelectionError::Failed)
}

/// Authorizes one exact staged digest for the next development worker trial.
/// It is intentionally separate from staging and never executes native code.
// Keep the caller, approval, catalog and revision inputs visible at this
// security boundary instead of hiding them in a loosely scoped context object.
#[allow(clippy::too_many_arguments)]
pub fn authorize_staged_native_trial(
    root: &Path,
    bootstrap: &BootstrapConfig,
    store: &RepositoryStore,
    repository_id: &str,
    catalog_digest: &str,
    digest: &str,
    expected_revision: &str,
    now: u64,
) -> Result<NativeSelectionSnapshot, NativeSelectionError> {
    let result = (|| {
        if !bootstrap.development {
            return Err("production repository native activation is not authorized".into());
        }
        validate_revision(expected_revision)?;
        store.with_current_catalog(
            repository_id,
            catalog_digest,
            &current_target(),
            now,
            |approved, catalog| {
                if !catalog.candidates().iter().any(|candidate| {
                    candidate.kind == CandidateKind::NativeSet
                        && candidate.manifest_sha256 == digest
                }) {
                    return Err("staged native set is absent from the current catalog".into());
                }
                authorize_staged_native_locked(
                    root,
                    bootstrap,
                    approved,
                    digest,
                    expected_revision,
                    now,
                )
            },
        )
    })();
    result.map_err(selection_operation_error)?;
    read_native_selection(root).map_err(NativeSelectionError::Failed)
}

/// Restores only the automatically recorded previous or a previously healthy
/// repository set. It preserves publisher floors and requires a caller CAS.
pub fn restore_previous_healthy_native(
    root: &Path,
    bootstrap: &BootstrapConfig,
    store: &RepositoryStore,
    expected_active_digest: Option<&str>,
    expected_revision: &str,
    now: u64,
) -> Result<NativeSelectionSnapshot, NativeSelectionError> {
    let result = (|| {
        if !bootstrap.development {
            return Err(
                "production native recovery requires installed-service authorization".into(),
            );
        }
        validate_revision(expected_revision)?;
        let repository_id = previous_repository_id(root, expected_revision)?;
        store.with_active_approval(&repository_id, |approved| {
            restore_previous_healthy_native_locked(
                root,
                bootstrap,
                approved,
                expected_active_digest,
                expected_revision,
                now,
            )
        })
    })();
    result.map_err(selection_operation_error)?;
    read_native_selection(root).map_err(NativeSelectionError::Failed)
}

fn previous_repository_id(root: &Path, expected_revision: &str) -> PackageResult<String> {
    let _lock = RootLock::acquire(root)?;
    let (selection, revision) = read_selection_with_revision(root)?;
    if revision != expected_revision {
        return Err(SELECTION_REVISION_CONFLICT.into());
    }
    let previous = selection
        .previous
        .as_deref()
        .ok_or("no previous native set is available")?;
    selection
        .repository_sets
        .get(previous)
        .map(|source| source.repository_id.clone())
        .ok_or_else(|| "previous native set has no approved publisher provenance".into())
}
pub use store::{RepositoryListing, RepositoryStore};
pub use transport::{HttpsRepository, RepositoryTransport, download_candidate, refresh_catalog};

/// Stage a complete native set for the next development worker trial. Callers
/// hold the repository approval lock while this acquires the host root lock.
/// No native code is loaded by this operation.
pub fn stage_native_candidate(
    source: &Path,
    root: &Path,
    bootstrap: &BootstrapConfig,
    approved: &ApprovedRepository,
    catalog: &VerifiedCatalog,
    candidate: &Candidate,
    now: u64,
) -> PackageResult<String> {
    stage_native_candidate_with_revision(
        source, root, bootstrap, approved, catalog, candidate, now, None,
    )
}

// This internal implementation receives the same explicit verification context
// as the public stage operation so the revision check stays adjacent to writes.
#[allow(clippy::too_many_arguments)]
fn stage_native_candidate_with_revision(
    source: &Path,
    root: &Path,
    bootstrap: &BootstrapConfig,
    approved: &ApprovedRepository,
    catalog: &VerifiedCatalog,
    candidate: &Candidate,
    now: u64,
    expected_revision: Option<&str>,
) -> PackageResult<String> {
    if !bootstrap.development || candidate.kind != CandidateKind::NativeSet {
        return Err("repository native staging requires a development native choice".into());
    }
    verify_candidate(approved, catalog, candidate, source, bootstrap, now)?;
    let _lock = RootLock::acquire(root)?;
    require_current_development_bootstrap(root, bootstrap)?;
    let selection_path = root.join("selection.json");
    let (mut selection, revision) = read_selection_with_revision(root)?;
    if expected_revision.is_some_and(|expected| expected != revision) {
        return Err(SELECTION_REVISION_CONFLICT.into());
    }
    let mut restricted = bootstrap.clone();
    restricted.allow_unsigned = false;
    restricted.trusted_keys = approved.keys();
    let floor = selection.repository_floors.get(&approved.fingerprint);
    let policy = VerificationPolicy {
        minimum_sequence: floor.map_or(0, |floor| floor.sequence),
        allow_historical: false,
        now,
    };
    let package = verify_package(source, &restricted, policy)?;
    if !package.signed
        || package.manifest.host_context_schema != 1
        || package.digest != candidate.manifest_sha256
        || !package.manifest.components.iter().any(|component| {
            component.id == candidate.slot && component.version == candidate.version
        })
    {
        return Err("repository native set signature, ABI, catalog digest or slot changed".into());
    }
    if floor.is_some_and(|floor| {
        floor.sequence == package.manifest.sequence && floor.digest != package.digest
    }) {
        return Err("publisher native release sequence was equivocated".into());
    }
    if selection
        .repository_sets
        .get(&package.digest)
        .is_some_and(|source| {
            source.repository_id != approved.id || source.key_fingerprint != approved.fingerprint
        })
    {
        return Err("native set digest already belongs to a different source".into());
    }
    crate::staging::install_immutable_set(source, root, &restricted, policy, &package)?;
    selection.repository_sets.insert(
        package.digest.clone(),
        RepositorySet {
            repository_id: approved.id.clone(),
            key_fingerprint: approved.fingerprint.clone(),
        },
    );
    selection.repository_floors.insert(
        approved.fingerprint.clone(),
        NativeReleaseFloor {
            sequence: package.manifest.sequence,
            digest: package.digest.clone(),
        },
    );
    write_json_atomically(&selection_path, &selection)?;
    Ok(package.digest)
}

/// Explicit action-time authorization to trial a previously staged native set
/// on the next development worker lifecycle. This still does not load code.
pub fn activate_staged_native(
    root: &Path,
    bootstrap: &BootstrapConfig,
    store: &RepositoryStore,
    digest: &str,
    now: u64,
) -> PackageResult<()> {
    if !bootstrap.development {
        return Err("production repository native activation is not authorized".into());
    }
    let selection_path = root.join("selection.json");
    let selection: ActivationState = read_json(&selection_path)?;
    let source = selection
        .repository_sets
        .get(digest)
        .ok_or("native digest was not staged from an approved repository")?;
    store.with_active_approval(&source.repository_id, |approved| {
        let catalog = store.catalog(&approved.id, &current_target(), now)?;
        if !catalog.candidates().iter().any(|candidate| {
            candidate.kind == CandidateKind::NativeSet && candidate.manifest_sha256 == digest
        }) {
            return Err("staged native set is absent from the current catalog".into());
        }
        let _lock = RootLock::acquire(root)?;
        require_current_development_bootstrap(root, bootstrap)?;
        let mut selection: ActivationState = read_json(&selection_path)?;
        let source = selection
            .repository_sets
            .get(digest)
            .ok_or("native digest was not staged from an approved repository")?;
        if source.repository_id != approved.id || source.key_fingerprint != approved.fingerprint {
            return Err("native publisher approval changed".into());
        }
        let floor = selection
            .repository_floors
            .get(&approved.fingerprint)
            .ok_or("native publisher release floor is missing")?;
        let mut restricted = bootstrap.clone();
        restricted.allow_unsigned = false;
        restricted.trusted_keys = approved.keys();
        let package = verify_package(
            &package_directory(root, digest)?,
            &restricted,
            VerificationPolicy {
                minimum_sequence: 0,
                allow_historical: true,
                now,
            },
        )?;
        if !package.signed
            || package.digest != digest
            || package.manifest.host_context_schema != 1
            || package.manifest.sequence != floor.sequence
            || package.digest != floor.digest
        {
            return Err("staged native set lost accepted publisher provenance".into());
        }
        selection.pending = Some(digest.into());
        write_json_atomically(&selection_path, &selection)
    })
}

fn authorize_staged_native_locked(
    root: &Path,
    bootstrap: &BootstrapConfig,
    approved: &ApprovedRepository,
    digest: &str,
    expected_revision: &str,
    now: u64,
) -> PackageResult<()> {
    if !canonical_digest(digest) {
        return Err("invalid staged native digest".into());
    }
    let selection_path = root.join("selection.json");
    let _lock = RootLock::acquire(root)?;
    require_current_development_bootstrap(root, bootstrap)?;
    let (mut selection, revision) = read_selection_with_revision(root)?;
    if revision != expected_revision {
        return Err(SELECTION_REVISION_CONFLICT.into());
    }
    if selection.trial.is_some() {
        return Err("a native activation trial is already in progress".into());
    }
    let source = selection
        .repository_sets
        .get(digest)
        .ok_or("native digest was not staged from an approved repository")?;
    if source.repository_id != approved.id || source.key_fingerprint != approved.fingerprint {
        return Err("native publisher approval changed".into());
    }
    let floor = selection
        .repository_floors
        .get(&approved.fingerprint)
        .ok_or("native publisher release floor is missing")?;
    let mut restricted = bootstrap.clone();
    restricted.allow_unsigned = false;
    restricted.trusted_keys = approved.keys();
    let package = verify_package(
        &package_directory(root, digest)?,
        &restricted,
        VerificationPolicy {
            minimum_sequence: 0,
            allow_historical: true,
            now,
        },
    )?;
    if !package.signed
        || package.digest != digest
        || package.manifest.host_context_schema != 1
        || package.manifest.sequence != floor.sequence
        || package.digest != floor.digest
    {
        return Err("staged native set lost accepted publisher provenance".into());
    }
    selection.pending = Some(digest.into());
    write_json_atomically(&selection_path, &selection)
}

fn restore_previous_healthy_native_locked(
    root: &Path,
    bootstrap: &BootstrapConfig,
    approved: &ApprovedRepository,
    expected_active_digest: Option<&str>,
    expected_revision: &str,
    now: u64,
) -> PackageResult<()> {
    let selection_path = root.join("selection.json");
    let _lock = RootLock::acquire(root)?;
    require_current_development_bootstrap(root, bootstrap)?;
    let (mut selection, revision) = read_selection_with_revision(root)?;
    if revision != expected_revision || selection.active.as_deref() != expected_active_digest {
        return Err(SELECTION_REVISION_CONFLICT.into());
    }
    if selection.trial.is_some() {
        return Err("native activation trial is in progress".into());
    }
    let previous = selection
        .previous
        .clone()
        .ok_or("no previous native set is available")?;
    if selection.active.as_deref() == Some(previous.as_str()) {
        return Err("previous native set is already active".into());
    }
    let provenance = selection
        .repository_sets
        .get(&previous)
        .ok_or("previous native set has no approved publisher provenance")?;
    if provenance.repository_id != approved.id || provenance.key_fingerprint != approved.fingerprint
    {
        return Err("previous native set publisher is no longer approved".into());
    }
    if !selection.healthy_sets.contains(&previous) {
        return Err("previous native set has not passed a worker health trial".into());
    }

    let mut restricted = bootstrap.clone();
    restricted.allow_unsigned = false;
    restricted.trusted_keys = approved.keys();
    let package = verify_package(
        &package_directory(root, &previous)?,
        &restricted,
        VerificationPolicy {
            minimum_sequence: 0,
            allow_historical: true,
            now,
        },
    )?;
    if !package.signed || package.digest != previous || package.manifest.host_context_schema != 1 {
        return Err("previous native set is not authenticated".into());
    }
    let floor = selection
        .repository_floors
        .get(&approved.fingerprint)
        .ok_or("native publisher release floor is missing")?;
    if package.manifest.sequence > floor.sequence
        || package.manifest.sequence == floor.sequence && package.digest != floor.digest
    {
        return Err("previous native set exceeds the accepted publisher release floor".into());
    }

    let old_active = selection.active.replace(previous);
    selection.previous = old_active;
    selection.pending = None;
    selection.trial = None;
    write_json_atomically(&selection_path, &selection)
}

fn validate_revision(revision: &str) -> PackageResult<()> {
    if canonical_digest(revision) {
        Ok(())
    } else {
        Err("invalid native selection revision".into())
    }
}

/// Revalidates the host-owned development policy while `RootLock` is held.
/// This prevents a caller that captured development settings before a root
/// transition from staging or authorizing against stale policy.
fn require_current_development_bootstrap(
    root: &Path,
    expected: &BootstrapConfig,
) -> PackageResult<()> {
    if !expected.development {
        return Err("repository native operation requires a development root".into());
    }
    let current_bytes = read_bounded(&root.join("bootstrap.json"), 1024 * 1024)?;
    let current: BootstrapConfig =
        serde_json::from_slice(&current_bytes).map_err(|error| error.to_string())?;
    if !current.development {
        return Err("current host bootstrap is not in development mode".into());
    }
    let expected_bytes = serde_json::to_vec(expected).map_err(|error| error.to_string())?;
    let current_bytes = serde_json::to_vec(&current).map_err(|error| error.to_string())?;
    if current_bytes != expected_bytes {
        return Err("host bootstrap changed; reload before native package operation".into());
    }
    Ok(())
}

fn read_selection_with_revision(root: &Path) -> PackageResult<(ActivationState, String)> {
    let path = root.join("selection.json");
    match std::fs::symlink_metadata(&path) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((ActivationState::default(), sha256(&[])));
        }
        Err(error) => return Err(error.to_string()),
    }
    let bytes = read_bounded(&path, 1024 * 1024)?;
    let selection = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    Ok((selection, sha256(&bytes)))
}

fn snapshot(selection: &ActivationState, revision: String) -> NativeSelectionSnapshot {
    NativeSelectionSnapshot {
        revision,
        active: selection.active.clone(),
        previous: selection.previous.clone(),
        pending: selection.pending.clone(),
        trial: selection.trial.clone(),
        highest_sequence: selection.highest_sequence,
    }
}

const MAX_CATALOG_BYTES: u64 = 256 * 1024;
const CATALOG_DOMAIN: &[u8] = b"amaterasu.repository.catalog.v1\n";

/// Approval is for *every* native module slot and the UI, not an add-on scope.
/// Callers must obtain `confirmed_fingerprint` through an independent channel.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovedRepository {
    pub id: String,
    pub location: String,
    pub public_key: String,
    pub fingerprint: String,
}

impl ApprovedRepository {
    pub fn approve(
        id: String,
        location: String,
        public_key: String,
        confirmed_fingerprint: String,
    ) -> PackageResult<Self> {
        if !valid_segment(&id) || !valid_https_location(&location) {
            return Err("invalid repository identity or HTTPS location".into());
        }
        let key = decode_hex::<32>(&public_key)?;
        VerifyingKey::from_bytes(&key).map_err(|_| "invalid publisher public key")?;
        let fingerprint = sha256(&key);
        if confirmed_fingerprint != fingerprint {
            return Err("publisher fingerprint does not match independent confirmation".into());
        }
        Ok(Self {
            id,
            location,
            public_key: encode_hex(&key),
            fingerprint,
        })
    }

    fn validate(&self) -> PackageResult<()> {
        Self::approve(
            self.id.clone(),
            self.location.clone(),
            self.public_key.clone(),
            self.fingerprint.clone(),
        )?;
        Ok(())
    }

    fn keys(&self) -> BTreeMap<String, String> {
        BTreeMap::from([(self.fingerprint.clone(), self.public_key.clone())])
    }
}

/// Persist this checkpoint with the approval to reject old or equivocated catalogs.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogCheckpoint {
    pub highest_sequence: u64,
    pub digest: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateKind {
    NativeSet,
    WebBundle,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Candidate {
    /// Native component identity, or `amaterasu.ui` for the web bundle.
    pub slot: String,
    pub implementation: String,
    pub version: String,
    pub kind: CandidateKind,
    /// SHA-256 of the signed native or web manifest bytes.
    pub manifest_sha256: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Catalog {
    pub schema: u32,
    pub repository: String,
    pub sequence: u64,
    pub expires_unix: u64,
    pub target: String,
    pub candidates: Vec<Candidate>,
}

pub struct VerifiedCatalog {
    catalog: Catalog,
    digest: String,
    publisher_fingerprint: String,
    next_checkpoint: CatalogCheckpoint,
}

impl VerifiedCatalog {
    pub fn candidates(&self) -> &[Candidate] {
        &self.catalog.candidates
    }

    pub fn next_checkpoint(&self) -> &CatalogCheckpoint {
        &self.next_checkpoint
    }

    pub fn digest(&self) -> &str {
        &self.digest
    }
}

/// Canonical serialization is compact serde JSON with declared field order.
/// Unknown fields, alternate whitespace, and reordered keys are rejected.
pub fn verify_catalog(
    approved: &ApprovedRepository,
    raw: &[u8],
    signature_hex: &str,
    checkpoint: &CatalogCheckpoint,
    target: &str,
    now: u64,
) -> PackageResult<VerifiedCatalog> {
    approved.validate()?;
    if raw.len() as u64 > MAX_CATALOG_BYTES {
        return Err("repository catalog exceeds size limit".into());
    }
    let catalog: Catalog = serde_json::from_slice(raw).map_err(|error| error.to_string())?;
    if serde_json::to_vec(&catalog).map_err(|error| error.to_string())? != raw {
        return Err("repository catalog is not canonical JSON".into());
    }
    let signature = ReleaseSignature {
        key_id: approved.fingerprint.clone(),
        signature: signature_hex.into(),
    };
    let mut signed = CATALOG_DOMAIN.to_vec();
    signed.extend_from_slice(raw);
    verify_detached_signature(&approved.keys(), &signed, &signature)?;
    if catalog.schema != 1
        || catalog.repository != approved.id
        || catalog.target != target
        || catalog.expires_unix <= now
        || catalog.candidates.len() > 256
    {
        return Err("invalid, expired, or wrong-target repository catalog".into());
    }
    let digest = sha256(raw);
    if (checkpoint.highest_sequence > 0 && checkpoint.digest.is_none())
        || checkpoint
            .digest
            .as_deref()
            .is_some_and(|digest| !canonical_digest(digest))
    {
        return Err("invalid repository catalog checkpoint".into());
    }
    if catalog.sequence < checkpoint.highest_sequence
        || (catalog.sequence == checkpoint.highest_sequence
            && checkpoint
                .digest
                .as_deref()
                .is_some_and(|old| old != digest))
    {
        return Err("repository catalog rollback or same-sequence substitution".into());
    }
    let mut identities = BTreeSet::new();
    for candidate in &catalog.candidates {
        if !valid_segment(&candidate.slot)
            || !valid_segment(&candidate.implementation)
            || !valid_segment(&candidate.version)
            || !canonical_digest(&candidate.manifest_sha256)
            || !identities.insert((
                &candidate.slot,
                &candidate.implementation,
                &candidate.version,
            ))
            || matches!(candidate.kind, CandidateKind::WebBundle)
                != (candidate.slot == "amaterasu.ui")
        {
            return Err("invalid or duplicate repository candidate".into());
        }
    }
    Ok(VerifiedCatalog {
        next_checkpoint: CatalogCheckpoint {
            highest_sequence: catalog.sequence,
            digest: Some(digest.clone()),
        },
        catalog,
        digest,
        publisher_fingerprint: approved.fingerprint.clone(),
    })
}

/// Verifies a locally obtained candidate against both the approved publisher
/// and the signed catalog. Never loads or activates its bytes.
pub fn verify_candidate(
    approved: &ApprovedRepository,
    catalog: &VerifiedCatalog,
    candidate: &Candidate,
    directory: &Path,
    bootstrap: &BootstrapConfig,
    now: u64,
) -> PackageResult<()> {
    approved.validate()?;
    if catalog.catalog.repository != approved.id
        || catalog.publisher_fingerprint != approved.fingerprint
        || catalog.catalog.expires_unix <= now
        || !catalog.catalog.candidates.iter().any(|listed| {
            listed.slot == candidate.slot
                && listed.implementation == candidate.implementation
                && listed.version == candidate.version
                && listed.kind == candidate.kind
                && listed.manifest_sha256 == candidate.manifest_sha256
        })
    {
        return Err("candidate is not in the current approved catalog".into());
    }
    let raw = read_bounded(&directory.join("manifest.json"), 1_048_576)?;
    if sha256(&raw) != candidate.manifest_sha256 {
        return Err("candidate manifest does not match repository catalog".into());
    }
    let canonical = match candidate.kind {
        CandidateKind::NativeSet => serde_json::to_vec(
            &serde_json::from_slice::<crate::ReleaseManifest>(&raw)
                .map_err(|error| error.to_string())?,
        ),
        CandidateKind::WebBundle => serde_json::to_vec(
            &serde_json::from_slice::<crate::web::Manifest>(&raw)
                .map_err(|error| error.to_string())?,
        ),
    }
    .map_err(|error| error.to_string())?;
    if canonical != raw {
        return Err("repository candidate manifest is not canonical JSON".into());
    }
    match candidate.kind {
        CandidateKind::NativeSet => {
            let mut restricted = bootstrap.clone();
            restricted.allow_unsigned = false;
            restricted.trusted_keys = approved.keys();
            let package = verify_package(
                directory,
                &restricted,
                VerificationPolicy {
                    minimum_sequence: 0,
                    allow_historical: false,
                    now,
                },
            )?;
            if !package.manifest.components.iter().any(|component| {
                component.id == candidate.slot && component.version == candidate.version
            }) {
                return Err("native set does not provide the selected slot and version".into());
            }
        }
        CandidateKind::WebBundle => {
            let bundle = crate::web::verify_bundle(directory, &approved.keys(), now)?;
            if bundle.digest != candidate.manifest_sha256
                || bundle.manifest.sequence.to_string() != candidate.version
            {
                return Err("web bundle identity or manifest changed".into());
            }
        }
    }
    Ok(())
}

fn canonical_digest(value: &str) -> bool {
    decode_hex::<32>(value).is_ok() && value == value.to_ascii_lowercase()
}

fn valid_segment(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value != "."
        && value != ".."
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        })
}

fn valid_https_location(value: &str) -> bool {
    let Some(rest) = value.strip_prefix("https://") else {
        return false;
    };
    let authority = rest.split('/').next().unwrap_or("");
    let (host, port) = authority
        .split_once(':')
        .map_or((authority, None), |(host, port)| (host, Some(port)));
    host.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && label
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            && !label.starts_with('-')
            && !label.ends_with('-')
    }) && !host.is_empty()
        && port.is_none_or(|port| port.parse::<u16>().is_ok_and(|value| value != 0))
        && !value.bytes().any(|byte| {
            byte.is_ascii_whitespace()
                || byte.is_ascii_control()
                || matches!(byte, b'\\' | b'?' | b'#' | b'@')
        })
}

#[cfg(test)]
mod tests;
