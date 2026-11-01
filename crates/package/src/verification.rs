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
    let signer_fingerprint = verify_signature(directory, bootstrap, &raw_manifest)?;
    let signed = signer_fingerprint.is_some();
    let manifest: ReleaseManifest =
        serde_json::from_slice(&raw_manifest).map_err(|error| error.to_string())?;

    validate_release(&manifest, policy)?;
    progress(0, manifest.components.len())?;
    validate_component_set(directory, &manifest.components, signed, progress)?;
    let digest = sha256(&raw_manifest);
    let privileged_requests = crate::privileged::verify_privileged_requests(
        directory,
        &manifest,
        &digest,
        signer_fingerprint.as_deref(),
    )?;
    let mut runtime_config = resolve_runtime_grants(bootstrap, &manifest.components);
    let service_diagnostics = crate::service_selection::resolve(
        &manifest.components,
        &bootstrap.policy.service_selections,
        &mut runtime_config,
    )?;

    Ok(VerifiedPackage {
        manifest,
        digest,
        signed,
        signer_fingerprint,
        privileged_requests,
        runtime_config,
        service_diagnostics,
    })
}

fn verify_signature(
    directory: &Path,
    bootstrap: &BootstrapConfig,
    raw_manifest: &[u8],
) -> PackageResult<Option<String>> {
    let signature_path = directory.join("manifest.sig");
    if !signature_path
        .try_exists()
        .map_err(|error| error.to_string())?
    {
        if bootstrap.allow_unsigned && bootstrap.development {
            return Ok(None);
        }
        return Err("unsigned component set rejected".into());
    }

    let signature: ReleaseSignature = read_json(&signature_path)?;
    verify_detached_signature(&bootstrap.trusted_keys, raw_manifest, &signature)?;
    let key = bootstrap
        .trusted_keys
        .get(&signature.key_id)
        .ok_or("unknown release signer")?;
    Ok(Some(sha256(&decode_hex::<32>(key)?)))
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
        service_selections: Default::default(),
        // The broker validates this trusted policy against effective grants.
        // Never silently discard a requested protection.
        effect_fence: bootstrap.policy.effect_fence.clone(),
        legacy_native_contracts: BTreeMap::new(),
        api_methods: Vec::new(),
        hooks: bootstrap.policy.hooks.clone(),
        settings: BTreeMap::new(),
        // A package policy is deliberately unable to specify process-local
        // host facts. The host injects those after package verification.
        host_context: Default::default(),
        // Package policy cannot create the reserved host lifecycle endpoint.
        host_lifecycle_control: false,
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
    runtime.legacy_native_contracts = bootstrap
        .policy
        .legacy_native_contracts
        .iter()
        .filter_map(|(principal, services)| {
            let grants = runtime.principals.get(principal)?;
            let approved: BTreeMap<_, _> = services
                .iter()
                .filter(|(service, _)| grants.native_provide.contains(*service))
                .map(|(service, contract)| (service.clone(), contract.clone()))
                .collect();
            (!approved.is_empty()).then(|| (principal.clone(), approved))
        })
        .collect();
    runtime.api_methods = bootstrap
        .policy
        .api_methods
        .iter()
        .filter(|approval| {
            runtime
                .principals
                .get(&approval.provider)
                .is_some_and(|grants| grants.provide.contains(&approval.method.service))
                && approval.callers.iter().all(|caller| {
                    runtime
                        .principals
                        .get(caller)
                        .is_some_and(|grants| grants.call.contains(&approval.method.service))
                })
        })
        .cloned()
        .collect();
    runtime
}

#[cfg(test)]
mod tests {
    use super::resolve_runtime_grants;
    use crate::BootstrapConfig;
    use amaterasu_contracts::{HOST_LIFECYCLE_CONTROL_PRINCIPAL, RuntimeConfig};
    use std::collections::BTreeMap;

    #[test]
    fn explicitly_requested_effect_fence_is_not_silently_discarded() {
        use amaterasu_contracts::effect_fence::{EffectFencePolicy, ProtectedMethod};
        let policy = EffectFencePolicy {
            authority: "authority".into(),
            issuer: "issuer".into(),
            methods: vec![ProtectedMethod {
                service: "example.effect/1".into(),
                method: "apply".into(),
                interface_major: 1,
            }],
            cleanup_methods: vec![],
        };
        let bootstrap = BootstrapConfig {
            development: false,
            allow_unsigned: false,
            trusted_keys: BTreeMap::new(),
            policy: RuntimeConfig {
                effect_fence: Some(policy.clone()),
                ..Default::default()
            },
        };
        assert_eq!(
            resolve_runtime_grants(&bootstrap, &[]).effect_fence,
            Some(policy)
        );
        // Missing effective principals and approvals are rejected by Broker::new,
        // rather than falling back to an unprotected route.
    }

    #[test]
    fn package_policy_cannot_claim_the_host_lifecycle_identity() {
        let bootstrap = BootstrapConfig {
            development: false,
            allow_unsigned: false,
            trusted_keys: BTreeMap::new(),
            policy: RuntimeConfig {
                principals: [(HOST_LIFECYCLE_CONTROL_PRINCIPAL.into(), Default::default())].into(),
                host_lifecycle_control: true,
                ..Default::default()
            },
        };

        let runtime = resolve_runtime_grants(&bootstrap, &[]);
        assert!(!runtime.host_lifecycle_control);
        assert!(
            !runtime
                .principals
                .contains_key(HOST_LIFECYCLE_CONTROL_PRINCIPAL)
        );
    }
    #[test]
    fn legacy_contracts_follow_effective_provider_grants() {
        use crate::ComponentManifest;
        use amaterasu_contracts::Grants;
        let service = "example.capture/1";
        let principal = "example.capture";
        let mut bootstrap = BootstrapConfig {
            development: false,
            allow_unsigned: false,
            trusted_keys: BTreeMap::new(),
            policy: RuntimeConfig::default(),
        };
        bootstrap.policy.principals.insert(
            principal.into(),
            Grants {
                native_provide: [service.into()].into(),
                ..Default::default()
            },
        );
        bootstrap.policy.legacy_native_contracts.insert(
            principal.into(),
            [(service.into(), "example.capture.native/1".into())].into(),
        );
        assert!(
            resolve_runtime_grants(&bootstrap, &[])
                .legacy_native_contracts
                .is_empty()
        );
        let mut component = ComponentManifest {
            service_provides: Vec::new(),
            service_requires: Vec::new(),
            id: principal.into(),
            version: "1".into(),
            file: "x".into(),
            sha256: "x".into(),
            size: 0,
            abi_major: 1,
            runtime: false,
            requires: BTreeMap::new(),
            requested: Grants::default(),
        };
        assert!(
            resolve_runtime_grants(&bootstrap, &[component.clone()])
                .legacy_native_contracts
                .is_empty()
        );
        component.requested.native_provide.insert(service.into());
        let resolved = resolve_runtime_grants(&bootstrap, &[component]);
        assert_eq!(
            resolved.legacy_native_contracts[principal][service],
            "example.capture.native/1"
        );
    }
}
