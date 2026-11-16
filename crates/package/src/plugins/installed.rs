//! Host-owned composition transactions. Original publisher manifests stay intact.
use super::{
    PluginManifest,
    composition::{PluginSource, VerifiedPlugin},
    operation::{Operation, Phase},
};
use crate::{
    BootstrapConfig, PackageResult, VerifiedPackage, read_json,
    repository::{CandidateKind, RepositoryStore, verify_candidate},
    write_json_atomically,
};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::Path};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Selection {
    #[serde(default)]
    pub approval_store: Option<std::path::PathBuf>,
    pub active: Vec<PluginSource>,
    /// Last runtime-user verification committed with the active composition.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub active_drivers: Vec<super::operation::Driver>,
    pub previous: Vec<PluginSource>,
    pub pending: Option<Operation>,
    pub trial: bool,
    pub floors: BTreeMap<String, (u64, String)>,
    pub error: Option<String>,
}

pub fn read(root: &Path) -> PackageResult<Selection> {
    let path = root.join("plugins/selection.json");
    match std::fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Selection::default()),
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            let raw = crate::read_bounded(&path, 256 * 1024)?;
            serde_json::from_slice(&raw).map_err(|error| error.to_string())
        }
        _ => Err("plugin_unsafe_selection".into()),
    }
}

pub fn revision(root: &Path, base: &str) -> PackageResult<String> {
    let selection = read(root)?;
    Ok(crate::sha256(
        &serde_json::to_vec(&(base, selection.active)).map_err(|e| e.to_string())?,
    ))
}

fn save(root: &Path, selection: &Selection) -> PackageResult<()> {
    crate::create_private_directory(&root.join("plugins"))?;
    let file = root.join("plugins/selection.json");
    write_json_atomically(&file, selection)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if std::fs::metadata(&file).map_err(|e| e.to_string())?.uid() != 0 {
            return Ok(());
        }
        let owner = std::fs::metadata(root).map_err(|e| e.to_string())?;
        std::os::unix::fs::chown(root.join("plugins"), Some(owner.uid()), Some(owner.gid()))
            .map_err(|e| e.to_string())?;
        std::os::unix::fs::chown(file, Some(owner.uid()), Some(owner.gid()))
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

pub fn stage(root: &Path, base: &str, operation: &Operation) -> PackageResult<()> {
    operation.validate()?;
    if operation.phase != Phase::ReadyToActivate
        || operation.installation_revision != revision(root, base)?
    {
        return Err("plugin_stale_activation".into());
    }
    let mut selection = read(root)?;
    if operation.preparation.is_some()
        || operation.sources.is_empty()
        || selection.active.iter().any(|active| {
            !operation.sources.iter().any(|source| {
                source.plugin == active.plugin && source.publisher == active.publisher
            })
        })
    {
        return Err("plugin_incomplete_composition".into());
    }
    if let Some(pending) = &selection.pending {
        if !selection.trial && pending.approval_digest() == operation.approval_digest() {
            return Ok(());
        }
        return Err("plugin_activation_in_progress".into());
    }
    check_floors(&selection, &operation.sources)?;
    selection.pending = Some(operation.clone());
    selection.error = None;
    save(root, &selection)
}

fn check_floors(selection: &Selection, sources: &[PluginSource]) -> PackageResult<()> {
    for source in sources {
        let key = format!("{}:{}", source.publisher, source.plugin);
        if let Some((sequence, digest)) = selection.floors.get(&key)
            && (source.sequence < *sequence
                || (source.sequence == *sequence && source.manifest_digest != *digest))
        {
            return Err("plugin_replay_rejected".into());
        }
    }
    Ok(())
}

/// Start a trial only in the supervisor after the previous graph has retired.
/// An interrupted trial is rolled back before any subsequent worker loads.
pub fn begin_trial(root: &Path) -> PackageResult<bool> {
    let mut selection = read(root)?;
    if selection.pending.is_none() {
        return Ok(false);
    }
    if selection.trial {
        selection.pending = None;
        selection.trial = false;
        selection.error = Some("plugin_trial_interrupted".into());
        save(root, &selection)?;
        return Ok(false);
    }
    selection.trial = true;
    save(root, &selection)?;
    Ok(true)
}

pub fn finish_trial(root: &Path, success: bool) -> PackageResult<()> {
    let mut selection = read(root)?;
    let Some(operation) = selection.pending.take() else {
        return Ok(());
    };
    if success {
        check_floors(&selection, &operation.sources)?;
        selection.active_drivers = operation.drivers;
        selection.previous = std::mem::replace(&mut selection.active, operation.sources);
        for source in &selection.active {
            selection.floors.insert(
                format!("{}:{}", source.publisher, source.plugin),
                (source.sequence, source.manifest_digest.clone()),
            );
        }
        selection.error = None;
    } else {
        selection.error = Some("plugin_trial_failed".into());
    }
    selection.trial = false;
    save(root, &selection)
}

/// Caller holds the repository approval lock through this verification AND load.
/// This variant deliberately does not reacquire the same process-wide lock.
pub fn verify_sources_locked(
    root: &Path,
    sources: &[PluginSource],
    bootstrap: &BootstrapConfig,
) -> PackageResult<Vec<VerifiedPlugin>> {
    let store = RepositoryStore::new(root.join("repositories"))?;
    verify_sources_from_store(&store, sources, bootstrap)
}

pub fn verify_sources_from_store(
    store: &RepositoryStore,
    sources: &[PluginSource],
    bootstrap: &BootstrapConfig,
) -> PackageResult<Vec<VerifiedPlugin>> {
    let now = crate::unix_timestamp();
    sources
        .iter()
        .map(|source| {
            let approved = store.approved(&source.repository)?;
            if approved.fingerprint != source.publisher {
                return Err("plugin_publisher_changed".into());
            }
            let catalog = store.catalog(&source.repository, &crate::current_target(), now)?;
            let candidate = catalog
                .candidates()
                .iter()
                .find(|candidate| {
                    candidate.kind == CandidateKind::NativePlugin
                        && candidate.slot == source.plugin
                        && candidate.version == source.version
                        && candidate.manifest_sha256 == source.manifest_digest
                })
                .ok_or("plugin_no_longer_listed")?;
            let directory = store.existing_native_candidate_cache_path(
                &source.repository,
                &source.manifest_digest,
            )?;
            super::validate_directory(&directory)?;
            verify_candidate(&approved, &catalog, candidate, &directory, bootstrap, now)?;
            let manifest: PluginManifest = read_json(&directory.join("manifest.json"))?;
            if manifest.sequence != source.sequence {
                return Err("plugin_sequence_changed".into());
            }
            Ok(VerifiedPlugin {
                source: source.clone(),
                manifest,
                directory,
            })
        })
        .collect()
}

/// Approvals are exact signed declarations from the admin-selected manifests.
/// A plugin can add its own principals, never override an existing principal.
pub fn policy_for(
    bootstrap: &BootstrapConfig,
    plugins: &[VerifiedPlugin],
) -> PackageResult<BootstrapConfig> {
    let mut approved = bootstrap.clone();
    for plugin in plugins {
        for component in &plugin.manifest.components {
            if let Some(existing) = approved.policy.principals.get(&component.id)
                && serde_json::to_value(existing.intersect(&component.requested))
                    .map_err(|e| e.to_string())?
                    != serde_json::to_value(&component.requested).map_err(|e| e.to_string())?
            {
                return Err("plugin_principal_conflict".into());
            }
            approved
                .policy
                .principals
                .insert(component.id.clone(), component.requested.clone());
        }
        for (principal, settings) in &plugin.manifest.settings {
            if !plugin
                .manifest
                .components
                .iter()
                .any(|component| component.id == *principal)
            {
                return Err("plugin_foreign_settings".into());
            }
            approved
                .policy
                .settings
                .insert(principal.clone(), settings.clone());
        }
        for method in &plugin.manifest.api_methods {
            let provider = plugin
                .manifest
                .components
                .iter()
                .find(|component| component.id == method.provider)
                .ok_or("plugin_api_provider_not_owned")?;
            if !provider.requested.provide.contains(&method.method.service)
                || method.method.service.starts_with("amaterasu.")
                || method.method.privilege != amaterasu_contracts::ApiPrivilege::User
            {
                return Err("plugin_api_not_owned".into());
            }
            for caller in &method.callers {
                if caller != "amaterasu.management"
                    && !plugin
                        .manifest
                        .components
                        .iter()
                        .any(|component| component.id == *caller)
                {
                    return Err("plugin_api_caller_not_approved".into());
                }
                if caller != "amaterasu.management"
                    && !plugin
                        .manifest
                        .components
                        .iter()
                        .find(|component| component.id == *caller)
                        .is_some_and(|component| {
                            component.requested.call.contains(&method.method.service)
                        })
                {
                    return Err("plugin_api_caller_grant_missing".into());
                }
                approved
                    .policy
                    .principals
                    .get_mut(caller)
                    .ok_or("plugin_api_caller_missing")?
                    .call
                    .insert(method.method.service.clone());
            }
            if approved.policy.api_methods.iter().any(|old| {
                old.method.service == method.method.service
                    && old.method.method == method.method.method
                    && old.method.interface_major == method.method.interface_major
            }) {
                return Err("plugin_api_conflict".into());
            }
            approved.policy.api_methods.push(method.clone());
        }
    }
    Ok(approved)
}

pub fn apply_locked(
    root: &Path,
    mut package: VerifiedPackage,
    bootstrap: &BootstrapConfig,
) -> PackageResult<VerifiedPackage> {
    let selection = read(root)?;
    let sources = if selection.trial {
        &selection
            .pending
            .as_ref()
            .ok_or("plugin_trial_missing")?
            .sources
    } else {
        &selection.active
    };
    if sources.is_empty() {
        return Ok(package);
    }
    check_floors(&selection, sources)?;
    if let Some(directory) = &selection.approval_store {
        let approvals = RepositoryStore::new(directory.clone())?;
        for source in sources {
            let approved = approvals.approved(&source.repository)?;
            if approved.fingerprint != source.publisher {
                return Err("plugin_publisher_changed".into());
            }
            let catalog = approvals.catalog(
                &source.repository,
                &crate::current_target(),
                crate::unix_timestamp(),
            )?;
            if !catalog
                .candidates()
                .iter()
                .any(|candidate| candidate.manifest_sha256 == source.manifest_digest)
            {
                return Err("plugin_no_longer_listed".into());
            }
        }
    }
    let plugins = verify_sources_locked(root, sources, bootstrap)?;
    let approved = policy_for(bootstrap, &plugins)?;
    let components = super::compose(
        &package.manifest.components,
        plugins.iter().map(|plugin| &plugin.manifest),
        &approved,
    )?;
    package.runtime_config = super::runtime_configuration(&approved, &components);
    // Absolute paths exist only in this verified, in-memory loader view. No
    // publisher manifest is rewritten or represented as newly signed bytes.
    package.manifest.components = components
        .into_iter()
        .map(|mut component| {
            if let Some(plugin) = plugins.iter().find(|plugin| {
                plugin
                    .manifest
                    .components
                    .iter()
                    .any(|item| item.id == component.id)
            }) {
                component.file = plugin
                    .directory
                    .join(&component.file)
                    .to_string_lossy()
                    .into_owned();
            }
            component
        })
        .collect();
    Ok(package)
}

/// Hold the live per-user approval store while loading its selected libraries.
pub fn with_overlay<T>(
    root: &Path,
    package: VerifiedPackage,
    bootstrap: &BootstrapConfig,
    load: impl FnOnce(VerifiedPackage) -> PackageResult<T>,
) -> PackageResult<T> {
    let selected = read(root)?;
    if let Some(directory) = selected.approval_store
        && directory != root.join("repositories")
    {
        let approvals = RepositoryStore::new(directory)?;
        return approvals.with_active_keys(|_| load(apply_locked(root, package, bootstrap)?));
    }
    load(apply_locked(root, package, bootstrap)?)
}
pub fn approval_store(root: &Path, directory: std::path::PathBuf) -> PackageResult<()> {
    let mut selected = read(root)?;
    selected.approval_store = Some(directory);
    save(root, &selected)
}
