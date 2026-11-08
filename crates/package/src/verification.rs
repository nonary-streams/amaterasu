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
    let runtime_config = resolve_runtime_grants(bootstrap, &manifest.components);
    validate_processing_compositions(&runtime_config)?;
    validate_processing_lifetimes(&runtime_config, &manifest.components)?;

    Ok(VerifiedPackage {
        manifest,
        digest,
        signed,
        signer_fingerprint,
        privileged_requests,
        runtime_config,
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
        legacy_native_contracts: BTreeMap::new(),
        api_methods: Vec::new(),
        hooks: bootstrap.policy.hooks.clone(),
        settings: BTreeMap::new(),
        processing_compositions: BTreeMap::new(),
        // A package policy is deliberately unable to specify process-local
        // host facts. The host injects those after package verification.
        host_context: Default::default(),
        // Package policy cannot create the reserved host lifecycle endpoint.
        host_lifecycle_control: false,
        host_package_activation_control: false,
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
        if let Some(composition) = bootstrap.policy.processing_compositions.get(&component.id) {
            runtime
                .processing_compositions
                .insert(component.id.clone(), composition.clone());
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

fn validate_processing_compositions(runtime: &RuntimeConfig) -> PackageResult<()> {
    for (principal, composition) in &runtime.processing_compositions {
        amaterasu_module_sdk::processing_composition::validate(composition)?;
        if let Some(consumer) = &composition.frame_consumer
            && &consumer.principal == principal
        {
            let processor = composition
                .processor
                .as_ref()
                .ok_or("selected frame consumer lacks a processor")?;
            let grants = runtime
                .principals
                .get(principal)
                .ok_or("frame consumer is not packaged")?;
            if !grants.native_use.contains(&processor.service) {
                return Err("selected frame consumer lacks effective native use approval".into());
            }
            if composition.lifecycle.is_none() && composition.frame_settings.is_none() {
                return Err("frame-only processing requires explicit settings".into());
            }
        }
        if serde_json::to_vec(composition)
            .map_err(|error| error.to_string())?
            .len()
            > 16_384
        {
            return Err("processing composition exceeds payload limit".into());
        }
        for caller in &composition.prepared_context_consumers {
            let lifecycle = composition
                .lifecycle
                .as_ref()
                .ok_or("prepared context has no lifecycle")?;
            if !runtime
                .principals
                .get(caller)
                .is_some_and(|grants| grants.call.contains(&lifecycle.service))
            {
                return Err("prepared context consumer lacks lifecycle approval".into());
            }
        }
        for ancestor in &composition.request_ancestors {
            if composition.stream_owner.is_none() {
                return Err("processing request ancestor has no stream owner".into());
            }
            let grants = runtime
                .principals
                .get(ancestor)
                .ok_or("processing request ancestor is not selected")?;
            for binding in [
                &composition.processor_control,
                &composition.display_settings,
            ]
            .into_iter()
            .flatten()
            {
                if !grants.call.contains(&binding.service) {
                    return Err("processing request ancestor lacks effective approval".into());
                }
            }
        }
        for caller in composition
            .lifecycle_admitters
            .iter()
            .chain(&composition.history_owners)
        {
            let grants = runtime
                .principals
                .get(caller)
                .ok_or("processing caller is not packaged")?;
            let owner = composition
                .stream_owner
                .as_ref()
                .ok_or("processing caller has no stream owner")?;
            if !grants.call.contains(&owner.service) {
                return Err("processing caller lacks effective stream owner approval".into());
            }
        }
        for binding in composition.bindings() {
            let grants = runtime
                .principals
                .get(&binding.principal)
                .ok_or("processing binding provider is not packaged")?;
            let native = composition.processor.as_ref() == Some(binding)
                || composition.frame_consumer.as_ref() == Some(binding);
            let selected = if native {
                &grants.native_provide
            } else {
                &grants.provide
            };
            if !selected.contains(&binding.service) {
                return Err("processing binding provider lacks effective approval".into());
            }
            if runtime
                .principals
                .iter()
                .filter(|(_, candidate)| {
                    if native {
                        candidate.native_provide.contains(&binding.service)
                    } else {
                        candidate.provide.contains(&binding.service)
                    }
                })
                .count()
                != 1
            {
                return Err("processing binding has duplicate providers".into());
            }
        }
        for registration in &composition.registrations {
            let grants = runtime
                .principals
                .get(&registration.provider)
                .ok_or("processing registration provider is not packaged")?;
            if registration
                .methods
                .iter()
                .any(|method| !grants.provide.contains(&method.service))
                || registration
                    .consumers
                    .iter()
                    .any(|consumer| !runtime.principals.contains_key(consumer))
                || registration
                    .settings_grants
                    .iter()
                    .any(|grant| !runtime.principals.contains_key(&grant.caller))
                || registration
                    .lifecycle_grants
                    .iter()
                    .any(|grant| !runtime.principals.contains_key(&grant.caller))
                || registration
                    .depends_on
                    .iter()
                    .any(|provider| !runtime.principals.contains_key(provider))
            {
                return Err("processing registration is not effectively approved".into());
            }
            if let Some(lifecycle) = &composition.lifecycle {
                for grant in &registration.lifecycle_grants {
                    if !runtime
                        .principals
                        .get(&grant.caller)
                        .is_some_and(|grants| grants.call.contains(&lifecycle.service))
                    {
                        return Err("processing lifecycle caller lacks effective approval".into());
                    }
                }
                for name in ["resource_plan", "lifecycle_callback", "lifecycle_status"] {
                    let method = registration
                        .methods
                        .iter()
                        .find(|method| method.method == name)
                        .ok_or("processing lifecycle method is missing")?;
                    if !runtime.api_methods.iter().any(|approval| {
                        approval.provider == registration.provider
                            && approval.method == *method
                            && approval.callers.contains(&lifecycle.principal)
                    }) {
                        return Err("processing lifecycle method lacks effective approval".into());
                    }
                }
            }
            if let Some(registry) = &composition.settings_registry {
                for grant in &registration.settings_grants {
                    if !runtime
                        .principals
                        .get(&grant.caller)
                        .is_some_and(|grants| grants.call.contains(&registry.service))
                    {
                        return Err("processing settings caller lacks effective approval".into());
                    }
                }
                let descriptor: amaterasu_module_sdk::extension_api::ExtensionDescriptor =
                    serde_json::from_value(registration.descriptor.clone())
                        .map_err(|_| "invalid processing descriptor")?;
                let reference = &descriptor.settings_validate;
                if !runtime.api_methods.iter().any(|approval| {
                    approval.provider == registration.provider
                        && approval.method.service == reference.service
                        && approval.method.method == reference.method
                        && approval.method.interface_major == reference.interface_major
                        && approval.method.interface_minor == reference.interface_minor
                        && approval.callers.contains(&registry.principal)
                }) {
                    return Err("processing settings validation lacks effective approval".into());
                }
            }
        }
    }
    Ok(())
}

/// Ensure every selected processor is retained until its actual consumers and
/// participants have retired. Grants alone cannot establish this ordering.
fn validate_processing_lifetimes(
    runtime: &RuntimeConfig,
    components: &[ComponentManifest],
) -> PackageResult<()> {
    use amaterasu_contracts::processing_composition::CapabilityBinding;
    let require_edge =
        |caller: &str, provider: &CapabilityBinding, native: bool| -> PackageResult<()> {
            let grants = runtime
                .principals
                .get(caller)
                .ok_or("processing consumer is not packaged")?;
            let access = if native {
                &grants.native_use
            } else {
                &grants.call
            };
            if !access.contains(&provider.service) {
                return Err("processing dependency lacks an effective grant".into());
            }
            if caller == "amaterasu.console" || caller == provider.principal {
                return Ok(());
            }
            let consumer = components
                .iter()
                .find(|component| component.id == caller)
                .ok_or("processing consumer component is not packaged")?;
            let target = components
                .iter()
                .find(|component| component.id == provider.principal)
                .ok_or("processing dependency component is not packaged")?;
            if consumer.requires.get(&target.id) != Some(&target.version) {
                return Err("processing dependency has no signed retirement order".into());
            }
            Ok(())
        };
    for (principal, view) in &runtime.processing_compositions {
        let Some(processor) = &view.processor else {
            continue;
        };
        if &processor.principal != principal {
            continue;
        }
        let consumer = view
            .frame_consumer
            .as_ref()
            .ok_or("selected processor lacks a frame consumer")?;
        let consumer_view = runtime
            .processing_compositions
            .get(&consumer.principal)
            .ok_or("selected frame consumer composition is absent")?;
        if consumer_view.processor.as_ref() != Some(processor)
            || consumer_view.frame_consumer.as_ref() != Some(consumer)
            || consumer_view.lifecycle != view.lifecycle
        {
            return Err("processing consumers disagree on the selected graph".into());
        }
        require_edge(&consumer.principal, processor, true)?;
        for caller in &consumer_view.source_preflight_consumers {
            require_edge(caller, consumer, false)?;
            if runtime
                .processing_compositions
                .get(caller)
                .and_then(|view| view.frame_consumer.as_ref())
                != Some(consumer)
            {
                return Err("source preflight consumer disagrees with selected video".into());
            }
        }
        if let Some(control) = &view.processor_control {
            for selected in [&view.stream_owner, &view.lifecycle, &view.settings_registry]
                .into_iter()
                .flatten()
            {
                require_edge(&selected.principal, control, false)?;
            }
        }
        if let Some(owner) = &view.stream_owner {
            let owner_view = runtime
                .processing_compositions
                .get(&owner.principal)
                .ok_or("selected stream owner composition is absent")?;
            if owner_view.processor.as_ref() != Some(processor)
                || owner_view.frame_consumer.as_ref() != Some(consumer)
                || owner_view.stream_owner.as_ref() != Some(owner)
                || owner_view.lifecycle != view.lifecycle
                || owner_view.settings_registry != view.settings_registry
                || owner_view.display_settings != view.display_settings
            {
                return Err("stream owner disagrees with the selected graph".into());
            }
            require_edge(&owner.principal, consumer, false)?;
            if let Some(lifecycle) = &view.lifecycle {
                require_edge(&owner.principal, lifecycle, false)?;
            }
            if let Some(display) = &view.display_settings {
                require_edge(&owner.principal, display, false)?;
            }
            for ancestor in &owner_view.request_ancestors {
                require_edge(ancestor, owner, false)?;
                require_edge(ancestor, consumer, false)?;
                for selected in [&view.lifecycle, &view.settings_registry]
                    .into_iter()
                    .flatten()
                {
                    require_edge(ancestor, selected, false)?;
                }
                if ancestor != "amaterasu.console" {
                    let caller_view = runtime
                        .processing_compositions
                        .get(ancestor)
                        .ok_or("processing request ancestor composition is absent")?;
                    if caller_view.stream_owner.as_ref() != Some(owner)
                        || caller_view.frame_consumer.as_ref() != Some(consumer)
                        || caller_view.lifecycle != view.lifecycle
                        || caller_view.settings_registry != view.settings_registry
                    {
                        return Err(
                            "processing request ancestor disagrees with selected graph".into()
                        );
                    }
                }
                if let Some(control) = &view.processor_control {
                    require_edge(ancestor, control, false)?;
                }
                if let Some(display) = &view.display_settings {
                    require_edge(ancestor, display, false)?;
                }
            }
        }
        if let Some(lifecycle) = &view.lifecycle {
            let lifecycle_view = runtime
                .processing_compositions
                .get(&lifecycle.principal)
                .ok_or("selected lifecycle composition is absent")?;
            if lifecycle_view.processor.as_ref() != Some(processor)
                || lifecycle_view.lifecycle.as_ref() != Some(lifecycle)
                || lifecycle_view.settings_registry != view.settings_registry
                || !lifecycle_view
                    .registrations
                    .iter()
                    .any(|entry| entry.provider == processor.principal)
            {
                return Err("lifecycle disagrees with the selected graph".into());
            }
            for caller in &lifecycle_view.prepared_context_consumers {
                require_edge(caller, lifecycle, false)?;
                if runtime
                    .processing_compositions
                    .get(caller)
                    .and_then(|view| view.lifecycle.as_ref())
                    != Some(lifecycle)
                {
                    return Err(
                        "prepared context consumer disagrees with lifecycle selection".into(),
                    );
                }
            }
            if let Some(registry) = &view.settings_registry {
                require_edge(&consumer.principal, lifecycle, false)?;
                require_edge(&lifecycle.principal, registry, false)?;
            }
        }
        if let Some(registry) = &view.settings_registry {
            let registry_view = runtime
                .processing_compositions
                .get(&registry.principal)
                .ok_or("selected registry composition is absent")?;
            if registry_view.settings_registry.as_ref() != Some(registry)
                || registry_view.lifecycle != view.lifecycle
                || !registry_view
                    .registrations
                    .iter()
                    .any(|entry| entry.provider == processor.principal)
            {
                return Err("settings registry disagrees with the selected graph".into());
            }
            if let Some(lifecycle) = &view.lifecycle
                && !runtime
                    .principals
                    .get(&registry.principal)
                    .is_some_and(|grants| grants.call.contains(&lifecycle.service))
            {
                return Err("settings registry lacks scope authority approval".into());
            }
        }
        for capability in [&view.display_settings, &view.foreground]
            .into_iter()
            .flatten()
        {
            require_edge(&processor.principal, capability, false)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        resolve_runtime_grants, validate_processing_compositions, validate_processing_lifetimes,
    };
    use crate::BootstrapConfig;
    use amaterasu_contracts::{HOST_LIFECYCLE_CONTROL_PRINCIPAL, RuntimeConfig};
    use std::collections::BTreeMap;

    #[test]
    fn processing_views_follow_the_signed_component_set_and_effective_grants() {
        use amaterasu_contracts::{
            Grants,
            processing_composition::{CapabilityBinding, ProcessingComposition},
        };
        let mut bootstrap = BootstrapConfig {
            development: true,
            allow_unsigned: false,
            trusted_keys: BTreeMap::new(),
            policy: RuntimeConfig::default(),
        };
        let provider_grants = Grants {
            native_provide: ["effect/1".into()].into(),
            ..Default::default()
        };
        bootstrap
            .policy
            .principals
            .insert("effect".into(), provider_grants.clone());
        bootstrap
            .policy
            .principals
            .insert("consumer".into(), Grants::default());
        bootstrap.policy.processing_compositions.insert(
            "consumer".into(),
            ProcessingComposition {
                processor: Some(CapabilityBinding {
                    principal: "effect".into(),
                    service: "effect/1".into(),
                    contract: "amaterasu.frame-processor.native/1".into(),
                    interface_major: 1,
                    interface_minor: 0,
                }),
                ..Default::default()
            },
        );
        let component = |id: &str, requested| crate::ComponentManifest {
            id: id.into(),
            version: "1".into(),
            file: format!("{id}.so"),
            sha256: "fixture".into(),
            size: 0,
            abi_major: 1,
            runtime: false,
            requires: BTreeMap::new(),
            requested,
        };
        let consumer = component("consumer", Grants::default());
        let provider = component("effect", provider_grants);
        let without_consumer = resolve_runtime_grants(&bootstrap, std::slice::from_ref(&provider));
        assert!(without_consumer.processing_compositions.is_empty());
        let without_provider = resolve_runtime_grants(&bootstrap, std::slice::from_ref(&consumer));
        assert!(validate_processing_compositions(&without_provider).is_err());
        let mut package = vec![consumer, provider];
        let valid = resolve_runtime_grants(&bootstrap, &package);
        assert!(validate_processing_compositions(&valid).is_ok());
        package[1].requested.native_provide.clear();
        let ungranted = resolve_runtime_grants(&bootstrap, &package);
        assert!(validate_processing_compositions(&ungranted).is_err());
    }

    #[test]
    fn frame_only_graph_requires_matching_contracts_grants_and_retirement_order() {
        use amaterasu_contracts::{
            Grants,
            processing_composition::{CapabilityBinding, ProcessingComposition},
        };
        let binding = |principal: &str, service: &str, contract: &str| CapabilityBinding {
            principal: principal.into(),
            service: service.into(),
            contract: contract.into(),
            interface_major: 1,
            interface_minor: 0,
        };
        let processor = binding(
            "vendor.effect",
            "vendor.effect/1",
            amaterasu_module_sdk::media::native_contracts::FRAME_PROCESSOR,
        );
        let consumer = binding(
            "vendor.video",
            "vendor.video/1",
            amaterasu_module_sdk::video::CONTRACT,
        );
        let mut runtime = RuntimeConfig::default();
        runtime.principals.insert(
            processor.principal.clone(),
            Grants {
                native_provide: [processor.service.clone()].into(),
                ..Default::default()
            },
        );
        runtime.principals.insert(
            consumer.principal.clone(),
            Grants {
                provide: [consumer.service.clone()].into(),
                native_provide: [consumer.service.clone()].into(),
                native_use: [processor.service.clone()].into(),
                ..Default::default()
            },
        );
        runtime.principals.insert(
            "vendor.preflight".into(),
            Grants {
                call: [consumer.service.clone()].into(),
                ..Default::default()
            },
        );
        runtime.processing_compositions.insert(
            processor.principal.clone(),
            ProcessingComposition {
                processor: Some(processor.clone()),
                frame_consumer: Some(consumer.clone()),
                ..Default::default()
            },
        );
        runtime.processing_compositions.insert(
            consumer.principal.clone(),
            ProcessingComposition {
                processor: Some(processor.clone()),
                frame_consumer: Some(consumer.clone()),
                frame_settings: Some(serde_json::json!({"strength":1})),
                source_preflight_consumers: ["vendor.preflight".into()].into(),
                ..Default::default()
            },
        );
        runtime.processing_compositions.insert(
            "vendor.preflight".into(),
            ProcessingComposition {
                frame_consumer: Some(consumer.clone()),
                ..Default::default()
            },
        );
        let component = |id: &str, requires| crate::ComponentManifest {
            id: id.into(),
            version: "1".into(),
            file: format!("{id}.so"),
            sha256: "fixture".into(),
            size: 0,
            abi_major: 1,
            runtime: false,
            requires,
            requested: runtime.principals[id].clone(),
        };
        let mut components = vec![
            component(&processor.principal, BTreeMap::new()),
            component(
                &consumer.principal,
                [(processor.principal.clone(), "1".into())].into(),
            ),
            component(
                "vendor.preflight",
                [(consumer.principal.clone(), "1".into())].into(),
            ),
        ];
        assert!(validate_processing_compositions(&runtime).is_ok());
        assert!(validate_processing_lifetimes(&runtime, &components).is_ok());
        components[1].requires.clear();
        assert!(validate_processing_lifetimes(&runtime, &components).is_err());
        components[1]
            .requires
            .insert(processor.principal.clone(), "1".into());
        let mut bad = runtime.clone();
        bad.principals
            .get_mut("vendor.preflight")
            .unwrap()
            .call
            .clear();
        assert!(validate_processing_lifetimes(&bad, &components).is_err());
        let mut bad = runtime.clone();
        bad.processing_compositions.remove("vendor.preflight");
        assert!(validate_processing_lifetimes(&bad, &components).is_err());
        let mut bad = runtime.clone();
        bad.processing_compositions
            .get_mut(&consumer.principal)
            .unwrap()
            .frame_settings = None;
        assert!(validate_processing_compositions(&bad).is_err());
        let mut bad = runtime.clone();
        bad.processing_compositions
            .get_mut(&consumer.principal)
            .unwrap()
            .processor
            .as_mut()
            .unwrap()
            .contract = "other.contract/1".into();
        assert!(validate_processing_compositions(&bad).is_err());
        let mut bad = runtime.clone();
        bad.principals
            .get_mut(&consumer.principal)
            .unwrap()
            .native_use
            .clear();
        assert!(validate_processing_compositions(&bad).is_err());
        let mut bad = runtime.clone();
        bad.principals.insert(
            "other.effect".into(),
            Grants {
                native_provide: [processor.service.clone()].into(),
                ..Default::default()
            },
        );
        assert!(validate_processing_compositions(&bad).is_err());
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
