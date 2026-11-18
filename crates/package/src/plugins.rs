//! Independently signed plugin packages. Composition preserves original manifests.
use crate::{
    BootstrapConfig, ComponentManifest, PackageResult, ReleaseSignature, read_bounded, read_json,
    sha256, verify_detached_signature,
};
use amaterasu_contracts::valid_name;
use amaterasu_module_sdk::drivers::{Artifact, DriverRequirement, MAX_REQUIREMENTS, SCHEMA};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

pub mod composition;
pub mod installed;
pub mod operation;
pub mod preparation;
pub mod rtx_hdr;
#[cfg(test)]
mod tests;

pub const MAX_PLUGIN_BYTES: u64 = 512 * 1024 * 1024;
pub const MAX_ARTIFACT_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginManifest {
    pub schema: u32,
    pub host_context_schema: u32,
    pub project: String,
    pub id: String,
    pub version: String,
    pub sequence: u64,
    pub expires_unix: u64,
    pub target: String,
    pub components: Vec<ComponentManifest>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub drivers: Vec<DriverRequirement>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub api_methods: Vec<amaterasu_contracts::ApiMethodApproval>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub settings: BTreeMap<String, serde_json::Value>,
}

/// Reject redirection without treating Windows' extended path prefix or case
/// normalization as a different directory. Every existing ancestor is checked.
pub fn validate_directory(path: &Path) -> PackageResult<()> {
    if !path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir | std::path::Component::CurDir
            )
        })
    {
        return Err("plugin_unsafe_directory".into());
    }
    for ancestor in path.ancestors() {
        let metadata = std::fs::symlink_metadata(ancestor).map_err(|e| e.to_string())?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err("plugin_unsafe_directory".into());
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if metadata.file_attributes() & 0x400 != 0 {
                return Err("plugin_unsafe_directory".into());
            }
        }
    }
    Ok(())
}

pub fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

fn text(value: &str, maximum: usize) -> bool {
    !value.is_empty() && value.len() <= maximum && !value.chars().any(char::is_control)
}

pub fn validate_artifact(artifact: &Artifact) -> PackageResult<()> {
    let stem = artifact
        .file
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let reserved = matches!(stem.as_str(), "con" | "prn" | "aux" | "nul")
        || (stem.len() == 4
            && (stem.starts_with("com") || stem.starts_with("lpt"))
            && matches!(stem.as_bytes()[3], b'1'..=b'9'));
    if !valid_name(&artifact.file)
        || artifact.file.starts_with('.')
        || artifact.file.contains('/')
        || artifact.file.ends_with('.')
        || reserved
        || matches!(artifact.file.as_str(), "manifest.json" | "manifest.sig")
        || !digest(&artifact.sha256)
        || artifact.size == 0
        || artifact.size > MAX_ARTIFACT_BYTES
    {
        return Err("plugin_invalid_artifact".into());
    }
    Ok(())
}

pub fn validate_requirement(requirement: &DriverRequirement) -> PackageResult<()> {
    if requirement.schema != SCHEMA
        || !valid_name(&requirement.id)
        || requirement.id.contains('/')
        || !text(&requirement.name, 128)
        || !valid_name(&requirement.feature)
        || !text(&requirement.reason, 1024)
        || !text(&requirement.compatibility, 128)
        || requirement.target != crate::current_target()
        || requirement
            .offered_version
            .as_ref()
            .is_some_and(|version| !version.validate())
        || requirement.payloads.len() > 32
    {
        return Err("driver_invalid_requirement".into());
    }
    validate_artifact(&requirement.installer)?;
    for artifact in &requirement.payloads {
        validate_artifact(artifact)?;
    }
    Ok(())
}

impl PluginManifest {
    pub fn validate(&self, now: u64) -> PackageResult<()> {
        if self.schema != 1
            || self.host_context_schema != 1
            || self.project != "amaterasu"
            || !valid_name(&self.id)
            || !valid_name(&self.version)
            || self.target != crate::current_target()
            || self.expires_unix <= now
            || self.sequence == 0
            || self.components.is_empty()
            || self.components.len() > 32
            || self.drivers.len() > MAX_REQUIREMENTS
        {
            return Err("plugin_incompatible_manifest".into());
        }
        let mut identities = BTreeSet::new();
        for component in &self.components {
            if component.runtime || !identities.insert(&component.id) {
                return Err("plugin_cannot_replace_runtime_or_duplicate_component".into());
            }
            crate::verification::validate_component_identity(component, true)?;
            for grants in [
                &component.requested.call,
                &component.requested.provide,
                &component.requested.native_provide,
                &component.requested.native_use,
                &component.requested.publish,
                &component.requested.subscribe,
                &component.requested.hook,
            ] {
                if grants.len() > 256 || grants.iter().any(|name| !valid_name(name)) {
                    return Err("plugin_invalid_grants".into());
                }
            }
        }
        let mut drivers = BTreeSet::new();
        for requirement in &self.drivers {
            validate_requirement(requirement)?;
            if !drivers.insert((&requirement.id, &requirement.feature)) {
                return Err("driver_duplicate_requirement".into());
            }
        }
        self.artifacts()?;
        Ok(())
    }

    /// A shared payload is stored once, but conflicting claims are rejected.
    pub fn artifacts(&self) -> PackageResult<Vec<Artifact>> {
        let mut files = BTreeMap::<String, Artifact>::new();
        for artifact in self
            .components
            .iter()
            .map(|c| Artifact {
                file: c.file.clone(),
                sha256: c.sha256.clone(),
                size: c.size,
            })
            .chain(self.drivers.iter().flat_map(|driver| {
                std::iter::once(driver.installer.clone()).chain(driver.payloads.clone())
            }))
        {
            validate_artifact(&artifact)?;
            if let Some(old) = files.insert(artifact.file.clone(), artifact.clone())
                && old != artifact
            {
                return Err("plugin_conflicting_artifact".into());
            }
        }
        if files
            .values()
            .try_fold(0u64, |total, file| total.checked_add(file.size))
            .is_none_or(|total| total > MAX_PLUGIN_BYTES)
        {
            return Err("plugin_payload_too_large".into());
        }
        Ok(files.into_values().collect())
    }
}

pub fn verify_manifest(
    raw: &[u8],
    signature: &ReleaseSignature,
    keys: &BTreeMap<String, String>,
    now: u64,
) -> PackageResult<PluginManifest> {
    if raw.len() > 1024 * 1024 {
        return Err("plugin_manifest_too_large".into());
    }
    verify_detached_signature(keys, raw, signature)?;
    let manifest: PluginManifest = serde_json::from_slice(raw).map_err(|e| e.to_string())?;
    manifest.validate(now)?;
    Ok(manifest)
}

pub fn verify_package(
    directory: &Path,
    bootstrap: &BootstrapConfig,
    now: u64,
) -> PackageResult<PluginManifest> {
    let raw = read_bounded(&directory.join("manifest.json"), 1024 * 1024)?;
    let signature = read_json(&directory.join("manifest.sig"))?;
    let manifest = verify_manifest(&raw, &signature, &bootstrap.trusted_keys, now)?;
    for artifact in manifest.artifacts()? {
        verify_artifact(directory, &artifact)?;
    }
    Ok(manifest)
}

pub fn verify_artifact(directory: &Path, artifact: &Artifact) -> PackageResult<()> {
    validate_artifact(artifact)?;
    let path = directory.join(&artifact.file);
    let metadata = std::fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("plugin_unsafe_artifact".into());
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err("plugin_unsafe_artifact".into());
        }
    }
    let bytes = read_bounded(&path, MAX_ARTIFACT_BYTES)?;
    if bytes.len() as u64 != artifact.size || sha256(&bytes) != artifact.sha256 {
        return Err("plugin_artifact_integrity_failed".into());
    }
    Ok(())
}

/// Validate a complete graph, keeping existing modules and their providers intact.
pub fn compose<'a>(
    base: &'a [ComponentManifest],
    plugins: impl IntoIterator<Item = &'a PluginManifest>,
    bootstrap: &BootstrapConfig,
) -> PackageResult<Vec<ComponentManifest>> {
    if base.iter().filter(|component| component.runtime).count() != 1 {
        return Err("plugin_composition_requires_one_runtime".into());
    }
    let mut components = base.to_vec();
    let mut ids: BTreeSet<String> = base.iter().map(|c| c.id.clone()).collect();
    if ids.len() != base.len() {
        return Err("plugin_duplicate_base_component".into());
    }
    for plugin in plugins {
        for component in &plugin.components {
            crate::verification::validate_component_identity(component, true)?;
            if component.runtime {
                return Err("plugin_cannot_replace_runtime".into());
            }
            if !ids.insert(component.id.clone()) {
                return Err(format!("plugin_component_conflict:{}", component.id));
            }
            let policy = bootstrap
                .policy
                .principals
                .get(&component.id)
                .ok_or_else(|| format!("plugin_grants_not_approved:{}", component.id))?;
            if serde_json::to_value(component.requested.intersect(policy))
                .map_err(|e| e.to_string())?
                != serde_json::to_value(&component.requested).map_err(|e| e.to_string())?
            {
                return Err(format!("plugin_grants_not_approved:{}", component.id));
            }
            components.push(component.clone());
        }
    }
    if components.len() > 128 {
        return Err("plugin_composition_too_large".into());
    }
    let versions: BTreeMap<_, _> = components.iter().map(|c| (&c.id, &c.version)).collect();
    for component in &components {
        for (id, version) in &component.requires {
            if versions.get(id) != Some(&version) {
                return Err(format!("plugin_dependency_conflict:{}:{id}", component.id));
            }
        }
    }
    crate::ordered_components(&components)?;
    let runtime = runtime_configuration(bootstrap, &components);
    amaterasu_contracts::api_validation::validate_approvals(&runtime)?;
    let mut providers = BTreeMap::new();
    let mut native_providers = BTreeMap::new();
    for component in &components {
        for service in &component.requested.provide {
            if !runtime
                .principals
                .get(&component.id)
                .is_some_and(|grants| grants.provide.contains(service))
            {
                continue;
            }
            if providers.insert(service, &component.id).is_some() {
                return Err(format!("plugin_ambiguous_provider:{service}"));
            }
        }
        for service in &component.requested.native_provide {
            if !runtime
                .principals
                .get(&component.id)
                .is_some_and(|grants| grants.native_provide.contains(service))
            {
                continue;
            }
            if native_providers.insert(service, &component.id).is_some() {
                return Err(format!("plugin_ambiguous_native_provider:{service}"));
            }
        }
    }
    for component in &components {
        for service in &component.requested.native_use {
            if !runtime
                .principals
                .get(&component.id)
                .is_some_and(|grants| grants.native_use.contains(service))
            {
                continue;
            }
            let provider = native_providers
                .get(service)
                .ok_or_else(|| format!("plugin_native_provider_missing:{service}"))?;
            if !base.iter().any(|base| base.id == component.id)
                && *provider != &component.id
                && component.requires.get(*provider) != versions.get(provider).copied()
            {
                return Err(format!(
                    "plugin_native_dependency_missing:{}:{provider}",
                    component.id
                ));
            }
        }
    }
    crate::verification::validate_service_layers(&runtime)?;
    crate::verification::validate_processing_compositions(&runtime)?;
    crate::verification::validate_processing_lifetimes(&runtime, &components)?;
    Ok(components)
}

/// Bind management only to explicitly approved user APIs from this composition.
/// Core publisher bytes stay unchanged; this is host-owned routing policy.
pub(super) fn runtime_configuration(
    bootstrap: &BootstrapConfig,
    components: &[ComponentManifest],
) -> amaterasu_contracts::RuntimeConfig {
    let mut bindings = components.to_vec();
    if let Some(management) = bindings
        .iter_mut()
        .find(|component| component.id == "amaterasu.management")
    {
        for approval in &bootstrap.policy.api_methods {
            if approval.method.privilege == amaterasu_contracts::ApiPrivilege::User
                && approval.callers.contains("amaterasu.management")
            {
                management
                    .requested
                    .call
                    .insert(approval.method.service.clone());
            }
        }
    }
    crate::verification::resolve_runtime_grants(bootstrap, &bindings)
}
