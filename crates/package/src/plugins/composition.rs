//! Host-owned selection records. Every source keeps its original signature.
use super::{PluginManifest, digest, installed::resolve_composition};
use crate::{
    BootstrapConfig, ComponentManifest, PackageResult, read_bounded, read_json,
    repository::{Candidate, CandidateKind, RepositoryStore, verify_candidate},
    sha256,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginSource {
    pub repository: String,
    pub publisher: String,
    pub catalog_digest: String,
    pub manifest_digest: String,
    pub plugin: String,
    pub version: String,
    pub sequence: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Composition {
    pub schema: u32,
    pub base: String,
    pub plugins: Vec<PluginSource>,
}

pub struct VerifiedPlugin {
    pub source: PluginSource,
    pub manifest: PluginManifest,
    pub directory: PathBuf,
}

pub fn verify_source(
    store: &RepositoryStore,
    source: &PluginSource,
    bootstrap: &BootstrapConfig,
    now: u64,
) -> PackageResult<VerifiedPlugin> {
    if !digest(&source.publisher)
        || !digest(&source.catalog_digest)
        || !digest(&source.manifest_digest)
    {
        return Err("plugin_invalid_source".into());
    }
    let current = store.catalog(&source.repository, &crate::current_target(), now)?;
    store.with_current_catalog(
        &source.repository,
        current.digest(),
        &crate::current_target(),
        now,
        |approved, catalog| {
            if approved.fingerprint != source.publisher {
                return Err("plugin_publisher_changed".into());
            }
            // A refreshed catalog may have a different digest, but it must still list the exact candidate.
            let candidate = catalog
                .candidates()
                .iter()
                .find(|candidate| {
                    candidate.kind == CandidateKind::NativePlugin
                        && candidate.manifest_sha256 == source.manifest_digest
                        && candidate.slot == source.plugin
                        && candidate.version == source.version
                })
                .ok_or("plugin_no_longer_listed")?;
            let directory = store.existing_native_candidate_cache_path(
                &source.repository,
                &source.manifest_digest,
            )?;
            verify_candidate(approved, catalog, candidate, &directory, bootstrap, now)?;
            let manifest: PluginManifest = read_json(&directory.join("manifest.json"))?;
            if manifest.sequence != source.sequence {
                return Err("plugin_sequence_changed".into());
            }
            Ok(VerifiedPlugin {
                source: source.clone(),
                manifest,
                directory,
            })
        },
    )
}

pub fn source_for(
    repository: &str,
    publisher: &str,
    catalog_digest: &str,
    candidate: &Candidate,
    directory: &Path,
) -> PackageResult<PluginSource> {
    let raw = read_bounded(&directory.join("manifest.json"), 1024 * 1024)?;
    if sha256(&raw) != candidate.manifest_sha256 {
        return Err("plugin_manifest_changed".into());
    }
    let manifest: PluginManifest = serde_json::from_slice(&raw).map_err(|e| e.to_string())?;
    Ok(PluginSource {
        repository: repository.into(),
        publisher: publisher.into(),
        catalog_digest: catalog_digest.into(),
        manifest_digest: candidate.manifest_sha256.clone(),
        plugin: manifest.id,
        version: manifest.version,
        sequence: manifest.sequence,
    })
}

pub fn verify_composition(
    composition: &Composition,
    base_digest: &str,
    base: &[ComponentManifest],
    store: &RepositoryStore,
    bootstrap: &BootstrapConfig,
    now: u64,
) -> PackageResult<(Vec<ComponentManifest>, Vec<VerifiedPlugin>)> {
    if composition.schema != 1
        || composition.base != base_digest
        || !digest(base_digest)
        || composition.plugins.len() > 32
    {
        return Err("plugin_stale_composition".into());
    }
    let mut ids = BTreeSet::new();
    let plugins: Vec<_> = composition
        .plugins
        .iter()
        .map(|source| {
            if !ids.insert(&source.plugin) {
                return Err("plugin_duplicate_source".into());
            }
            verify_source(store, source, bootstrap, now)
        })
        .collect::<PackageResult<_>>()?;
    let (components, _) = resolve_composition(base, bootstrap, &plugins)?;
    Ok((components, plugins))
}
