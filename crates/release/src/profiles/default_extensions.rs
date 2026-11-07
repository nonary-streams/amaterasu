//! Trusted default composition of ordinary extension contracts. All option
//! definitions and method schemas come from the independently built plugin.
use super::{ComponentDefinition, FRAME_LIMITER, RUNTIME_ID};
use amaterasu_contracts::{ApiMethod, RuntimeConfig};
use amaterasu_extension_api::{ExtensionDescriptor, MethodRef};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, path::Path};

/// Generic lifecycle and registry services used by default modules and plugins.
pub(super) const COORDINATOR_COMPONENT: ComponentDefinition = ComponentDefinition {
    id: "amaterasu.session-lifecycle",
    library: "amaterasu_session_lifecycle",
    provides: &["amaterasu.session-lifecycle/1"],
    calls: &[
        "amaterasu.launch/1",
        "amaterasu.state/1",
        "amaterasu.extension-registry/1",
        "nonary.frame-limiter/1",
    ],
    publishes: &[],
    dependencies: &[
        RUNTIME_ID,
        "amaterasu.extension-registry",
        "amaterasu.frame-limiter",
    ],
    native_provides: &[],
    native_uses: &[],
};
pub(super) const REGISTRY_COMPONENT: ComponentDefinition = ComponentDefinition {
    id: "amaterasu.extension-registry",
    library: "amaterasu_extension_registry",
    provides: &["amaterasu.extension-registry/1"],
    calls: &[
        "amaterasu.state/1",
        "amaterasu.session-lifecycle/1",
        "nonary.frame-limiter/1",
    ],
    publishes: &[],
    dependencies: &[RUNTIME_ID, "amaterasu.state"],
    native_provides: &[],
    native_uses: &[],
};

pub(super) const PROVIDER_COMPONENT: ComponentDefinition = ComponentDefinition {
    provides: &["amaterasu.frame-limiter/1", "nonary.frame-limiter/1"],
    calls: &["amaterasu.session-lifecycle/1"],
    ..FRAME_LIMITER
};

const COORDINATOR: &str = "amaterasu.session-lifecycle";
const COORDINATOR_SERVICE: &str = "amaterasu.session-lifecycle/1";
const REGISTRY: &str = "amaterasu.extension-registry";
const REGISTRY_SERVICE: &str = "amaterasu.extension-registry/1";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Metadata {
    descriptor: ExtensionDescriptor,
    methods: Vec<ApiMethod>,
    #[serde(default)]
    migrations: Vec<Value>,
}

pub(super) fn install(
    policy: &mut RuntimeConfig,
    metadata: &[&str],
    journal_directory: &Path,
) -> Result<(), String> {
    configure(policy, metadata, Some(journal_directory))
}

pub(super) fn install_with_runtime_journal(
    policy: &mut RuntimeConfig,
    metadata: &[&str],
) -> Result<(), String> {
    configure(policy, metadata, None)
}

fn configure(
    policy: &mut RuntimeConfig,
    metadata: &[&str],
    journal_directory: Option<&Path>,
) -> Result<(), String> {
    let mut descriptors = Vec::new();
    let mut participants = Vec::new();
    let mut operations = BTreeMap::from([
        (
            "amaterasu.nvhttp",
            vec![
                "prepare",
                "bind_application",
                "application_retired",
                "end_session",
                "lookup",
                "snapshot",
                "recover_session",
                "abandon_restarted_session",
            ],
        ),
        (
            "amaterasu.rtsp",
            vec!["prepare_stream", "resume_stream", "lookup", "snapshot"],
        ),
        (
            "amaterasu.stream-session",
            vec![
                "ack_stream",
                "start_stream",
                "pause_stream",
                "retire_stream",
                "resolve_stream_preparation",
                "lookup",
                "snapshot",
            ],
        ),
        ("amaterasu.launch", vec!["read_prepared_plans"]),
        (REGISTRY, vec!["resolve_settings_scope"]),
    ]);
    let mut plugin_operations = BTreeMap::new();
    for source in metadata {
        let metadata: Metadata = serde_json::from_str(source).map_err(|error| error.to_string())?;
        amaterasu_extension_api::validate_descriptor(&metadata.descriptor)?;
        let provider = &metadata.descriptor.provider;
        if !policy.principals.contains_key(provider) {
            return Err("default extension module missing".into());
        }
        let method = |name: &str| -> Result<MethodRef, String> {
            let method = metadata
                .methods
                .iter()
                .find(|method| method.method == name)
                .ok_or("default extension method missing")?;
            Ok(MethodRef {
                service: method.service.clone(),
                method: name.into(),
                interface_major: method.interface_major,
                interface_minor: method.interface_minor,
                schema_revision: 1,
            })
        };
        let callback = method("lifecycle_callback")?;
        let fields = metadata
            .descriptor
            .options
            .iter()
            .map(|option| (option.id.clone(), option.definition.clone()))
            .collect();
        let defaults = amaterasu_configuration_schema::defaults(&fields)?;
        participants.push(json!({"provider":provider,"plan_method":method("resource_plan")?,
            "callback_method":callback,"status_method":method("lifecycle_status")?,"settings_revision":"builtin-1",
            "settings":defaults,"capabilities":metadata.descriptor.capabilities,"cleanup_methods":[callback]}));
        plugin_operations.insert(provider.clone(), vec!["read_provider_context"]);
        let validation_callers = [
            REGISTRY,
            COORDINATOR,
            "amaterasu.management",
            "amaterasu.nvhttp",
            "amaterasu.rtsp",
            "amaterasu.stream-session",
            "amaterasu.console",
        ];
        for method in &metadata.methods {
            let callers = if method.method == metadata.descriptor.settings_validate.method {
                validation_callers
                    .iter()
                    .filter(|caller| {
                        policy
                            .principals
                            .get(**caller)
                            .is_some_and(|grants| grants.call.contains(&method.service))
                    })
                    .map(|value| (*value).to_owned())
                    .collect()
            } else {
                [COORDINATOR.to_owned()].into()
            };
            policy
                .api_methods
                .push(amaterasu_contracts::ApiMethodApproval {
                    provider: provider.clone(),
                    callers,
                    method: method.clone(),
                });
        }
        let grants: Vec<Value> = [COORDINATOR, "amaterasu.management", "amaterasu.console"]
            .into_iter().filter(|caller| policy.principals.contains_key(*caller)).flat_map(|caller| {
                ["host", "all_applications", "all_paired_clients", "all_sessions"].map(|kind| json!({
                    "caller":caller,"scope":{"kind":kind},"read":true,"write":caller != COORDINATOR,
                }))
            }).collect();
        let digest = format!(
            "{:x}",
            Sha256::digest(
                serde_json::to_vec(&metadata.descriptor).map_err(|error| error.to_string())?
            )
        );
        descriptors.push(
            json!({"descriptor":metadata.descriptor,"descriptor_sha256":digest,
            "grants":grants,"approved_methods":metadata.methods,"migrations":metadata.migrations}),
        );
        policy.settings.insert(
            provider.clone(),
            json!({"extension_registry_principal":REGISTRY,
            "lifecycle_coordinator":{"principal":COORDINATOR,"service":COORDINATOR_SERVICE}}),
        );
    }
    let mut operation_grants: BTreeMap<String, Vec<&str>> = operations
        .iter_mut()
        .map(|(key, value)| ((*key).to_owned(), std::mem::take(value)))
        .collect();
    operation_grants.extend(plugin_operations);
    policy
        .settings
        .insert(REGISTRY.into(), json!({"descriptors":descriptors}));
    policy.settings.insert(COORDINATOR.into(), json!({
        "journal_path":journal_directory.map(|path| path.join("journal.json")).unwrap_or_default(),
        "journal_in_runtime_state":journal_directory.is_none(),"capture_binding_mode":"external",
        "trusted_callers":operation_grants.keys().collect::<Vec<_>>(),"operation_grants":operation_grants,
        "session_delegates":["amaterasu.rtsp","amaterasu.stream-session"],
        "settings_registry_caller":REGISTRY,"settings_registry_service":REGISTRY_SERVICE,
        "consumers":{"capture":["amaterasu.stream-session"]},
        "consumer_requirements":{"execution_domain":"desktop","capture_providers":[],"selector_schemas":[]},
        "participants":participants,
    }));
    let launch = policy
        .settings
        .entry("amaterasu.launch".into())
        .or_insert(json!({}));
    launch["prepared_context_service"] = json!(COORDINATOR_SERVICE);
    launch["retirement_observers"] = json!([COORDINATOR]);
    let nvhttp = policy
        .settings
        .get_mut("amaterasu.nvhttp")
        .ok_or("default transport missing")?;
    nvhttp["session_lifecycle_required"] = json!(true);
    nvhttp["session_lifecycle_capture_mode"] = json!("external");
    Ok(())
}
