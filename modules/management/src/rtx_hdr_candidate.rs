//! Download a publisher-signed RTX HDR component set using the Network SDK.
//!
//! This only prepares a candidate in the repository cache. The host must check
//! current approvals and verify the cached set again at the activation boundary.
pub(crate) use crate::candidate_cache::PreparedCandidate;
use crate::candidate_cache::{Placement, check_cancelled, place_verified};
use crate::repository_transport::NetworkRepository;
use amaterasu_module_sdk::network::Network;
use amaterasu_package::repository::{
    Candidate, CandidateKind, RepositoryListing, RepositoryStore, refresh_catalog,
};
use amaterasu_package::{
    BootstrapConfig, PackageResult, current_target, read_json, unix_timestamp,
};
use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64},
    },
};

const SLOT: &str = "amaterasu.rtx-hdr";
const IMPLEMENTATION: &str = "amaterasu-linux-session-rtx-hdr";
const OFFICIAL_REPOSITORY: &str = "amaterasu-rtx-hdr";
const DOWNLOAD_LIMIT: std::time::Duration = std::time::Duration::from_secs(600);

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
    let transport = NetworkRepository::new(
        &approved.location,
        network,
        cancelled.clone(),
        transferred,
        DOWNLOAD_LIMIT,
    );
    place_verified(
        &Placement {
            store: &store,
            approved: &approved,
            catalog: &catalog,
            candidate: &candidate,
            bootstrap: &bootstrap,
        },
        &transport,
        &cancelled,
    )
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
}
