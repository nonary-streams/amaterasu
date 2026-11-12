//! The download worker: place the offered set, fully verified, in the source
//! repository's candidate cache.
//!
//! Files the running set already holds are copied from it instead of fetched.
//! Where a byte came from is irrelevant to trust: the assembled candidate is
//! verified as a whole against the signed catalog and manifest before it is
//! published to the cache, and the host verifies it again before a trial.
use super::Failure;
use super::check::{approved_source, stop_reason};
use super::environment::UpdateSource;
use super::plan::Offer;
use super::transport::{Connector, Observed, Reusing, held_files};
use crate::candidate_cache::{Placement, PreparedCandidate, place_verified};
use amaterasu_package::repository::RepositoryStore;
use amaterasu_package::{BootstrapConfig, current_target, read_json, unix_timestamp};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64},
    },
    time::Duration,
};

/// Bounds one artifact request, as the RTX HDR download does.
const ARTIFACT_REQUEST_LIMIT: Duration = Duration::from_secs(600);

/// Runs on a worker thread; `cancelled` stops it between reads and
/// `transferred` counts the bytes received from the repository.
pub(crate) fn download<C: Connector>(
    source: &UpdateSource,
    offer: &Offer,
    connector: &C,
    cancelled: &Arc<AtomicBool>,
    transferred: &Arc<AtomicU64>,
) -> Result<PreparedCandidate, Failure> {
    let stopped = |code| stop_reason(cancelled, code);
    let store = RepositoryStore::new(source.repository_directory.clone())
        .map_err(|_| stopped("repository_unavailable"))?;
    let approved = approved_source(&store, source).map_err(stopped)?;
    let bootstrap: BootstrapConfig = read_json(&source.package_root.join("bootstrap.json"))
        .map_err(|_| stopped("verification_failed"))?;
    // The offer is placed only while the saved catalog still lists it.
    let catalog = store
        .catalog(&approved.id, &current_target(), unix_timestamp())
        .map_err(|_| stopped("catalog_rejected"))?;
    let repository = Observed::new(connector.connect(
        &approved.location,
        cancelled.clone(),
        transferred.clone(),
        ARTIFACT_REQUEST_LIMIT,
    ));
    let transport = Reusing::new(&repository, held_files(&source.running, offer));
    place_verified(
        &Placement {
            store: &store,
            approved: &approved,
            catalog: &catalog,
            candidate: &offer.candidate,
            bootstrap: &bootstrap,
        },
        &transport,
        cancelled,
    )
    .map_err(|_| {
        stopped(if repository.failed() {
            "download_failed"
        } else {
            "verification_failed"
        })
    })
}
