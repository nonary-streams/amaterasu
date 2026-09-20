use amaterasu_contracts::{RuntimeConfig, valid_name};
use ed25519_dalek::{Signature, VerifyingKey};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::{
    BootstrapConfig, ComponentManifest, PackageResult, ReleaseManifest, ReleaseSignature,
    VerifiedPackage, current_target, decode_hex, ordered_components, read_bounded, read_json,
    sha256,
};

const MAX_MANIFEST_BYTES: u64 = 1_048_576;
pub(super) const MAX_ARTIFACT_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Clone, Copy)]
pub struct VerificationPolicy {
    pub minimum_sequence: u64,
    pub allow_historical: bool,
    pub now: u64,
}

pub fn verify_package(
    directory: &Path,
    bootstrap: &BootstrapConfig,
    policy: VerificationPolicy,
) -> PackageResult<VerifiedPackage> {
    verify_package_with_progress(directory, bootstrap, policy, &mut |_, _| Ok(()))
}

pub fn verify_package_with_progress(
    directory: &Path,
    bootstrap: &BootstrapConfig,
    policy: VerificationPolicy,
    progress: &mut dyn FnMut(usize, usize) -> PackageResult<()>,
) -> PackageResult<VerifiedPackage> {
    let raw_manifest = read_bounded(&directory.join("manifest.json"), MAX_MANIFEST_BYTES)?;
    let signed = verify_signature(directory, bootstrap, &raw_manifest)?;
    let manifest: ReleaseManifest =
        serde_json::from_slice(&raw_manifest).map_err(|error| error.to_string())?;

    validate_release(&manifest, policy)?;
    progress(0, manifest.components.len())?;
    validate_component_set(directory, &manifest.components, signed, progress)?;
    let runtime_config = resolve_runtime_grants(bootstrap, &manifest.components);

    Ok(VerifiedPackage {
        manifest,
        digest: sha256(&raw_manifest),
        signed,
        runtime_config,
    })
}

fn verify_signature(
    directory: &Path,
    bootstrap: &BootstrapConfig,
    raw_manifest: &[u8],
) -> PackageResult<bool> {
    let signature_path = directory.join("manifest.sig");
    if !signature_path
        .try_exists()
        .map_err(|error| error.to_string())?
    {
        if bootstrap.allow_unsigned && bootstrap.development {
            return Ok(false);
        }
        return Err("unsigned component set rejected".into());
    }

    let signature: ReleaseSignature = read_json(&signature_path)?;
    verify_detached_signature(&bootstrap.trusted_keys, raw_manifest, &signature)?;
    Ok(true)
}

pub fn verify_detached_signature(
    trusted_keys: &BTreeMap<String, String>,
    raw_manifest: &[u8],
    signature: &ReleaseSignature,
) -> PackageResult<()> {
    let trusted_key = trusted_keys
        .get(&signature.key_id)
        .ok_or("unknown release signer")?;
    let public_key = VerifyingKey::from_bytes(&decode_hex::<32>(trusted_key)?)
        .map_err(|error| error.to_string())?;
    let signature_bytes = decode_hex::<64>(&signature.signature)?;

    public_key
        .verify_strict(raw_manifest, &Signature::from_bytes(&signature_bytes))
        .map_err(|_| "release signature verification failed")?;
    Ok(())
}

fn validate_release(manifest: &ReleaseManifest, policy: VerificationPolicy) -> PackageResult<()> {
    if manifest.schema != 1 || manifest.project != "amaterasu" {
        return Err("wrong project or package schema".into());
    }
    if manifest.target != current_target() {
        return Err("wrong operating system or architecture".into());
    }
    if manifest.expires_unix <= policy.now {
        return Err("release metadata expired".into());
    }
    if manifest.sequence < policy.minimum_sequence && !policy.allow_historical {
        return Err("release sequence would roll back trusted metadata".into());
    }

    let component_count = manifest.components.len();
    let runtime_count = manifest
        .components
        .iter()
        .filter(|component| component.runtime)
        .count();
    if !(1..=64).contains(&component_count) || runtime_count != 1 {
        return Err("component set requires exactly one runtime and at most 64 artifacts".into());
    }
    Ok(())
}

fn validate_component_set(
    directory: &Path,
    components: &[ComponentManifest],
    signed: bool,
    progress: &mut dyn FnMut(usize, usize) -> PackageResult<()>,
) -> PackageResult<()> {
    let mut selected_versions = BTreeMap::new();
    let mut artifact_paths = BTreeSet::new();

    for (index, component) in components.iter().enumerate() {
        validate_component_identity(component, signed)?;
        verify_artifact(directory, component)?;
        progress(index + 1, components.len())?;

        let duplicate_id = selected_versions
            .insert(&component.id, &component.version)
            .is_some();
        let duplicate_path = !artifact_paths.insert(&component.file);
        if duplicate_id || duplicate_path {
            return Err("duplicate component identity or artifact path".into());
        }
    }

    validate_dependencies(components, &selected_versions)?;
    ordered_components(components)?;
    Ok(())
}

fn validate_component_identity(component: &ComponentManifest, signed: bool) -> PackageResult<()> {
    if !valid_name(&component.id) || component.id == "amaterasu.console" {
        return Err(format!("invalid component identity: {}", component.id));
    }
    if !valid_name(&component.version) {
        return Err(format!("invalid version for {}", component.id));
    }
    if !valid_name(&component.file)
        || component.file.contains('/')
        || component.file.starts_with('.')
    {
        return Err("artifact name must be a simple filename".into());
    }
    if component.abi_major != amaterasu_abi::ABI_MAJOR {
        return Err(format!("incompatible host ABI for {}", component.id));
    }
    if component.runtime && !component.requires.is_empty() {
        return Err("bootstrap runtime cannot depend on a feature library".into());
    }
    if component.size > MAX_ARTIFACT_BYTES {
        return Err(format!("artifact exceeds size limit: {}", component.id));
    }
    if !signed && component.id.starts_with("amaterasu.") {
        return Err("unsigned components cannot claim official identities".into());
    }
    Ok(())
}

fn verify_artifact(directory: &Path, component: &ComponentManifest) -> PackageResult<()> {
    let bytes = read_bounded(&directory.join(&component.file), MAX_ARTIFACT_BYTES)?;
    if bytes.len() as u64 != component.size || sha256(&bytes) != component.sha256 {
        return Err(format!("artifact integrity failed: {}", component.id));
    }
    Ok(())
}

fn validate_dependencies(
    components: &[ComponentManifest],
    selected_versions: &BTreeMap<&String, &String>,
) -> PackageResult<()> {
    for component in components {
        for (dependency, required_version) in &component.requires {
            if selected_versions.get(dependency) != Some(&required_version) {
                return Err(format!(
                    "incompatible dependency: {} requires {dependency} {required_version}",
                    component.id,
                ));
            }
        }
    }
    Ok(())
}

fn resolve_runtime_grants(
    bootstrap: &BootstrapConfig,
    components: &[ComponentManifest],
) -> RuntimeConfig {
    let mut runtime = RuntimeConfig {
        principals: BTreeMap::new(),
        hooks: bootstrap.policy.hooks.clone(),
        settings: BTreeMap::new(),
        // A package policy is deliberately unable to specify process-local
        // host facts. The host injects those after package verification.
        host_context: Default::default(),
    };

    for component in components.iter().filter(|component| !component.runtime) {
        let installed_grants = bootstrap
            .policy
            .principals
            .get(&component.id)
            .cloned()
            .unwrap_or_default();
        let effective_grants = component.requested.intersect(&installed_grants);
        runtime
            .principals
            .insert(component.id.clone(), effective_grants);
        if let Some(settings) = bootstrap.policy.settings.get(&component.id) {
            runtime
                .settings
                .insert(component.id.clone(), settings.clone());
        }
    }

    if let Some(console) = bootstrap.policy.principals.get("amaterasu.console") {
        runtime
            .principals
            .insert("amaterasu.console".into(), console.clone());
    }
    runtime
}
