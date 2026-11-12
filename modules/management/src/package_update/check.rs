//! The check worker: refresh the source repository's catalog and find out
//! whether it offers a set newer than the running one.
//!
//! Only signed metadata is fetched here. Every manifest is authenticated
//! under the approved publisher key before its contents are compared.
use super::environment::UpdateSource;
use super::plan::{self, Checked};
use super::transport::{Connector, Observed};
use super::{CANDIDATE_AMBIGUOUS, Failure};
use crate::repository_admin::{fetch_catalog, retry_while_locked};
use amaterasu_package::repository::{
    ApprovedRepository, RepositoryStore, RepositoryTransport, fetch_native_manifest,
};
use amaterasu_package::{current_target, unix_timestamp};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

/// A catalog or manifest is at most a megabyte; a repository that cannot
/// serve one in this time is reported as unreachable.
const METADATA_REQUEST_LIMIT: Duration = Duration::from_secs(30);

/// Runs on a worker thread; `cancelled` stops it between requests.
pub(crate) fn check<C: Connector>(
    source: &UpdateSource,
    connector: &C,
    cancelled: &Arc<AtomicBool>,
) -> Result<Checked, Failure> {
    let stopped = |code| stop_reason(cancelled, code);
    let store = RepositoryStore::new(source.repository_directory.clone())
        .map_err(|_| stopped("repository_unavailable"))?;
    let approved = approved_source(&store, source).map_err(stopped)?;
    let transport = Observed::new(connector.connect(
        &approved.location,
        cancelled.clone(),
        // Metadata is not part of the download the console shows progress for.
        Arc::new(AtomicU64::new(0)),
        METADATA_REQUEST_LIMIT,
    ));
    let target = current_target();
    refresh_catalog(&store, &approved.id, &transport, &target).map_err(stopped)?;
    let catalog = store
        .catalog(&approved.id, &target, unix_timestamp())
        .map_err(|_| stopped("catalog_rejected"))?;
    let line = plan::update_line(catalog.candidates(), &source.running.digest)
        .map_err(|_| stopped(CANDIDATE_AMBIGUOUS))?;

    let mut offered = Vec::new();
    for candidate in line {
        // The running set's manifest is already known and verified.
        if candidate.manifest_sha256 == source.running.digest {
            continue;
        }
        let manifest =
            fetch_native_manifest(&transport, &approved, &catalog, candidate, unix_timestamp())
                .map_err(|_| {
                    stopped(if transport.failed() {
                        "manifest_download_failed"
                    } else {
                        "manifest_rejected"
                    })
                })?;
        offered.push((candidate.clone(), manifest));
    }
    if cancelled.load(Ordering::Acquire) {
        return Err(Failure::Cancelled);
    }
    plan::select_update(offered, &source.running).map_err(|_| stopped(CANDIDATE_AMBIGUOUS))
}

/// The approval of the repository recorded as the running set's supplier,
/// which must still be held under the publisher key the set was accepted for.
pub(super) fn approved_source(
    store: &RepositoryStore,
    source: &UpdateSource,
) -> Result<ApprovedRepository, &'static str> {
    store
        .approved(&source.repository)
        .ok()
        .filter(|approved| approved.fingerprint == source.fingerprint)
        .ok_or("repository_unavailable")
}

fn refresh_catalog(
    store: &RepositoryStore,
    repository: &str,
    transport: &impl RepositoryTransport,
    target: &str,
) -> Result<(), &'static str> {
    let (raw, signature) = fetch_catalog(transport).map_err(|refusal| {
        if refusal == "catalog_download_failed" {
            refusal
        } else {
            "catalog_rejected"
        }
    })?;
    retry_while_locked(|| {
        store.accept_catalog(repository, &raw, &signature, target, unix_timestamp())
    })
    .map_err(|error| {
        if error.starts_with(amaterasu_package::ROOT_BUSY) {
            "repository_unavailable"
        } else {
            "catalog_rejected"
        }
    })
}

/// A worker that fails after cancellation was requested stopped because of it.
pub(super) fn stop_reason(cancelled: &AtomicBool, code: &'static str) -> Failure {
    if cancelled.load(Ordering::Acquire) {
        Failure::Cancelled
    } else {
        Failure::Failed(code)
    }
}
