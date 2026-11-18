//! Host-owned wiring for the official Linux RTX HDR add-on.
//!
//! A catalog entry may supply the signed provider libraries. The host selects
//! known SDK roles and derives their grants and retirement edges independently.
use super::composition::{PluginSource, VerifiedPlugin};
use crate::{BootstrapConfig, ComponentManifest, PackageResult, processing_policy, verification};
use processing_policy::rtx_hdr::{DISPLAY, FOREGROUND, PROVIDER, default_rtx_hdr_selection_for};
use serde_json::json;
use std::path::{Component, Path};

const CORE_REPOSITORY: &str = "amaterasu";
const CORE_PUBLISHER: &str = "e89ff0faa32cd120fa58642edff3c0f3e118c9d1099e302b1234dd33870c706c";

pub fn is_official(source: &PluginSource) -> bool {
    source.plugin == PROVIDER
        && source.repository == CORE_REPOSITORY
        && source.publisher == CORE_PUBLISHER
}

/// The first runtime version that understands the host's add-on capability.
pub fn runtime_supports_addon(version: &str) -> bool {
    if !cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        return false;
    }
    let version = version.split('-').next().unwrap_or_default();
    let mut parts = version.split('.').map(str::parse::<u32>);
    matches!((parts.next(), parts.next(), parts.next(), parts.next()),
        (Some(Ok(major)), Some(Ok(minor)), Some(Ok(patch)), None) if (major, minor, patch) >= (0, 1, 1))
}

/// The same protected state location used by the complete-set installer.
pub fn runtime_parent(bootstrap: &BootstrapConfig) -> PackageResult<&Path> {
    bootstrap
        .policy
        .settings
        .get("amaterasu.management")
        .and_then(|settings| settings["rtx_hdr_runtime_parent"].as_str())
        .map(Path::new)
        .filter(|path| {
            path.is_absolute()
                && path
                    .components()
                    .all(|part| matches!(part, Component::RootDir | Component::Normal(_)))
        })
        .ok_or_else(|| "rtx_hdr_runtime_location_unavailable".into())
}

pub(super) fn apply(
    bootstrap: &mut BootstrapConfig,
    core: &mut [ComponentManifest],
    plugins: &[VerifiedPlugin],
) -> PackageResult<()> {
    let Some(plugin) = plugins.iter().find(|plugin| plugin.manifest.id == PROVIDER) else {
        return Ok(());
    };
    if !is_official(&plugin.source) || plugin.manifest.target != "linux-x86_64" {
        return Err("rtx_hdr_addon_publisher_or_platform_unsupported".into());
    }
    if !plugin.manifest.settings.is_empty()
        || !plugin.manifest.api_methods.is_empty()
        || !plugin.manifest.drivers.is_empty()
        || !plugin
            .manifest
            .components
            .iter()
            .any(|component| component.id == DISPLAY)
        || !plugin
            .manifest
            .components
            .iter()
            .any(|component| component.id == PROVIDER)
        || plugin
            .manifest
            .components
            .iter()
            .any(|component| !matches!(component.id.as_str(), PROVIDER | DISPLAY | FOREGROUND))
    {
        return Err("rtx_hdr_addon_profile_invalid".into());
    }
    if core.iter().any(|component| component.id == PROVIDER) {
        return Err("rtx_hdr_already_in_core".into());
    }
    if !core
        .iter()
        .any(|component| component.runtime && runtime_supports_addon(&component.version))
    {
        return Err("rtx_hdr_addon_host_update_required".into());
    }
    let runtime = runtime_parent(bootstrap)?.join("runtime");
    let policy = &mut bootstrap.policy;
    let selection = default_rtx_hdr_selection_for(policy);
    if let Some(video) = policy.settings.get(&selection.frame_consumer.principal)
        && [
            "cpu_processor_service",
            "resource_processor_service",
            "gpu_processor_service",
        ]
        .iter()
        .any(|field| video.get(*field).is_some_and(|value| !value.is_null()))
    {
        return Err("rtx_hdr_addon_conflicts_with_selected_processor".into());
    }
    if let (Some(owner), Some(display)) = (&selection.stream_owner, &selection.display_settings)
        && let Some(current) = policy
            .settings
            .get(&owner.principal)
            .and_then(|settings| settings.get("source_preparation"))
            .and_then(|settings| settings.get("display_settings_service"))
            .and_then(serde_json::Value::as_str)
        && current != display.service
    {
        return Err("rtx_hdr_addon_conflicts_with_display_setup".into());
    }
    if selection.foreground.is_some() {
        policy
            .settings
            .insert(FOREGROUND.into(), json!({"allowed_principals": [PROVIDER]}));
    }
    let original_grants = policy.principals.clone();
    policy.settings.insert(
        PROVIDER.into(),
        json!({
            "runtime_directory": runtime, "require_verified_runtime": true, "max_streams": 2,
        }),
    );
    policy.settings.insert(
        DISPLAY.into(),
        json!({
            "enabled": true, "registrars": ["amaterasu.stream-session"], "consumers": [PROVIDER],
        }),
    );
    processing_policy::install(
        policy,
        &selection,
        include_str!("../../../../modules/rtx-hdr/metadata/linux.json"),
    )?;

    // This in-memory view contains only the Core requests added by the host's
    // SDK selection. Neither the base nor the add-on manifest is rewritten.
    for component in core.iter_mut().filter(|component| !component.runtime) {
        let Some(after) = policy.principals.get(&component.id) else {
            continue;
        };
        let Some(before) = original_grants.get(&component.id) else {
            continue;
        };
        component
            .requested
            .call
            .extend(after.call.difference(&before.call).cloned());
        component
            .requested
            .native_use
            .extend(after.native_use.difference(&before.native_use).cloned());
    }
    let versions = core
        .iter()
        .chain(&plugin.manifest.components)
        .map(|component| (component.id.clone(), component.version.clone()))
        .collect::<std::collections::BTreeMap<_, _>>();
    verification::visit_processing_lifetimes(policy, Some(PROVIDER), |caller, provider, native| {
        let Some(component) = core.iter_mut().find(|component| component.id == caller) else {
            return Ok(());
        };
        if native {
            &mut component.requested.native_use
        } else {
            &mut component.requested.call
        }
        .insert(provider.service.clone());
        if caller != provider.principal {
            let version = versions
                .get(&provider.principal)
                .ok_or("rtx_hdr_addon_dependency_missing")?;
            if component
                .requires
                .get(&provider.principal)
                .is_some_and(|current| current != version)
            {
                return Err("rtx_hdr_addon_dependency_version_conflict".into());
            }
            component
                .requires
                .insert(provider.principal.clone(), version.clone());
        }
        Ok(())
    })
}

#[cfg(all(test, target_os = "linux", target_arch = "x86_64"))]
mod tests {
    use super::*;
    use crate::plugins::PluginManifest;

    #[test]
    fn an_alternate_publisher_cannot_request_the_core_rtx_profile() {
        let plugin = VerifiedPlugin {
            source: PluginSource {
                repository: CORE_REPOSITORY.into(),
                publisher: "a".repeat(64),
                catalog_digest: "b".repeat(64),
                manifest_digest: "c".repeat(64),
                plugin: PROVIDER.into(),
                version: "0.1.0".into(),
                sequence: 1,
            },
            manifest: PluginManifest {
                schema: 1,
                host_context_schema: 1,
                project: "amaterasu".into(),
                id: PROVIDER.into(),
                version: "0.1.0".into(),
                sequence: 1,
                expires_unix: u64::MAX,
                target: "linux-x86_64".into(),
                components: Vec::new(),
                drivers: Vec::new(),
                api_methods: Vec::new(),
                settings: Default::default(),
            },
            directory: Default::default(),
        };
        let mut bootstrap = BootstrapConfig {
            development: false,
            allow_unsigned: false,
            trusted_keys: Default::default(),
            policy: Default::default(),
        };
        assert_eq!(
            apply(&mut bootstrap, &mut [], &[plugin]),
            Err("rtx_hdr_addon_publisher_or_platform_unsupported".into())
        );
        assert!(bootstrap.policy.processing_compositions.is_empty());
    }
}
