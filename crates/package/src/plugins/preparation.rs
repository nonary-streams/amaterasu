//! Worker-only plugin preparation. This never loads native code or installs a driver.
use super::{
    composition::{Composition, PluginSource, source_for, verify_composition},
    digest,
    operation::Operation,
};
use crate::{
    BootstrapConfig, ComponentManifest, PackageResult,
    repository::{
        CandidateKind, RepositoryStore, RepositoryTransport, download_candidate, verify_candidate,
    },
};
use std::{
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};

/// Facts supplied by the trusted host, not accepted from a browser request.
pub struct InstallationContext<'a> {
    pub bootstrap: &'a BootstrapConfig,
    pub base_digest: &'a str,
    pub components: &'a [ComponentManifest],
    pub installation_revision: &'a str,
    pub runtime_identity: &'a str,
    pub installed: &'a [PluginSource],
}

/// Resolve one explicitly selected catalog entry, preserving the existing composition.
/// `transport` must be the Network SDK adapter for the already approved repository.
pub fn prepare(
    context: &InstallationContext<'_>,
    store: &RepositoryStore,
    repository: &str,
    manifest_digest: &str,
    transport: &impl RepositoryTransport,
    cancelled: &AtomicBool,
    now: u64,
) -> PackageResult<Operation> {
    if !digest(manifest_digest)
        || !digest(context.base_digest)
        || !digest(context.installation_revision)
    {
        return Err("plugin_invalid_preparation".into());
    }
    check_cancelled(cancelled)?;
    let approved = store.approved(repository)?;
    let catalog = store.catalog(repository, &crate::current_target(), now)?;
    let candidate = catalog
        .candidates()
        .iter()
        .find(|candidate| {
            candidate.kind == CandidateKind::NativePlugin
                && candidate.manifest_sha256 == manifest_digest
        })
        .ok_or("plugin_not_offered")?;
    let destination = store.native_candidate_cache_path(repository, manifest_digest)?;
    let parent = destination.parent().ok_or("plugin_invalid_cache")?;
    let _cache_lock = crate::RootLock::acquire(parent)?;
    if !destination.try_exists().map_err(|e| e.to_string())? {
        let temporary = parent.join(format!(".plugin-{}", crate::random_identifier()?));
        let result = (|| {
            download_candidate(
                transport,
                &approved,
                &catalog,
                candidate,
                &temporary,
                context.bootstrap,
                now,
            )?;
            check_cancelled(cancelled)?;
            std::fs::rename(&temporary, &destination).map_err(|e| e.to_string())
        })();
        if result.is_err() {
            let _ = std::fs::remove_dir_all(&temporary);
        }
        result?;
    }
    check_directory(&destination)?;
    verify_candidate(
        &approved,
        &catalog,
        candidate,
        &destination,
        context.bootstrap,
        now,
    )?;
    let source = source_for(
        repository,
        &approved.fingerprint,
        catalog.digest(),
        candidate,
        &destination,
    )?;
    let mut sources = context.installed.to_vec();
    if let Some(position) = sources
        .iter()
        .position(|installed| installed.plugin == source.plugin)
    {
        let previous = &sources[position];
        if previous.publisher != source.publisher {
            return Err("plugin_update_publisher_changed".into());
        }
        if previous.manifest_digest == source.manifest_digest {
            return Err("plugin_already_installed".into());
        }
        if source.sequence <= previous.sequence {
            return Err("plugin_update_replay_rejected".into());
        }
        sources[position] = source;
    } else {
        sources.push(source);
    }
    let composition = Composition {
        schema: 1,
        base: context.base_digest.into(),
        plugins: sources.clone(),
    };
    // Re-read every approval and catalog after retrieval; a revoked or replaced
    // candidate cannot acquire a prepared operation by winning a download race.
    let verified_sources = sources
        .iter()
        .map(|source| super::composition::verify_source(store, source, context.bootstrap, now))
        .collect::<PackageResult<Vec<_>>>()?;
    let approved = super::installed::policy_for(context.bootstrap, &verified_sources)?;
    let (_, verified) = verify_composition(
        &composition,
        context.base_digest,
        context.components,
        store,
        &approved,
        now,
    )?;
    check_cancelled(cancelled)?;
    let requirements = verified
        .iter()
        .flat_map(|plugin| {
            let id = plugin.manifest.id.clone();
            plugin
                .manifest
                .drivers
                .iter()
                .cloned()
                .map(move |requirement| (id.clone(), requirement))
        })
        .collect();
    let mut operation = Operation::prepare(
        context.installation_revision.into(),
        sources,
        context.runtime_identity.into(),
        requirements,
    )?;
    for driver in &mut operation.drivers {
        driver.required_components = verified
            .iter()
            .filter(|plugin| driver.required_by.contains(&plugin.manifest.id))
            .flat_map(|plugin| {
                plugin
                    .manifest
                    .components
                    .iter()
                    .map(|component| component.id.clone())
            })
            .collect();
    }
    operation.validate()?;
    Ok(operation)
}

fn check_cancelled(cancelled: &AtomicBool) -> PackageResult<()> {
    if cancelled.load(Ordering::Acquire) {
        Err("plugin_preparation_cancelled".into())
    } else {
        Ok(())
    }
}

fn check_directory(path: &Path) -> PackageResult<()> {
    super::validate_directory(path)
}
