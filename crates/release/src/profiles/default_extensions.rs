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
            "grants":grants,"approved_methods":metadata.methods,"migrations":[]}),
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

/// Add one explicitly approved extension to an existing lifecycle composition.
/// The caller supplies and authorizes the provider and consumers first. All
/// validation runs on a clone, so conflicts leave the original policy intact.
pub(super) fn add_extension(
    policy: &mut RuntimeConfig,
    source: &str,
    processing_consumer: Option<&str>,
) -> Result<(), String> {
    let mut candidate = policy.clone();
    append_extension(&mut candidate, source, processing_consumer)?;
    *policy = candidate;
    Ok(())
}

fn append_extension(
    policy: &mut RuntimeConfig,
    source: &str,
    processing_consumer: Option<&str>,
) -> Result<(), String> {
    let metadata: Metadata = serde_json::from_str(source).map_err(|error| error.to_string())?;
    amaterasu_extension_api::validate_descriptor(&metadata.descriptor)?;
    let provider = &metadata.descriptor.provider;
    let provider_grants = policy
        .principals
        .get(provider)
        .ok_or("extension provider missing")?;
    if metadata.methods.is_empty()
        || metadata
            .methods
            .iter()
            .any(|method| !provider_grants.provide.contains(&method.service))
    {
        return Err("extension method service is not provided by its principal".into());
    }
    let mut identities = std::collections::BTreeSet::new();
    for method in &metadata.methods {
        if !identities.insert((
            method.service.clone(),
            method.method.clone(),
            method.interface_major,
        )) || policy.api_methods.iter().any(|approval| {
            approval.provider == *provider
                && approval.method.service == method.service
                && approval.method.method == method.method
                && approval.method.interface_major == method.interface_major
        }) {
            return Err("duplicate extension API method".into());
        }
    }
    let method_ref = |name: &str| -> Result<MethodRef, String> {
        let mut matches = metadata
            .methods
            .iter()
            .filter(|method| method.method == name);
        let method = matches.next().ok_or("extension method missing")?;
        if matches.next().is_some() {
            return Err("ambiguous extension method".into());
        }
        Ok(MethodRef {
            service: method.service.clone(),
            method: name.into(),
            interface_major: method.interface_major,
            interface_minor: method.interface_minor,
            schema_revision: 1,
        })
    };
    let validation = method_ref(&metadata.descriptor.settings_validate.method)?;
    if validation != metadata.descriptor.settings_validate {
        return Err("extension validation method differs from descriptor".into());
    }
    for caller in [REGISTRY, COORDINATOR] {
        if !policy
            .principals
            .get(caller)
            .is_some_and(|grants| grants.call.contains(&validation.service))
        {
            return Err("extension settings caller lacks validation call grant".into());
        }
    }
    let callback = method_ref("lifecycle_callback")?;
    let plan = method_ref("resource_plan")?;
    let status = method_ref("lifecycle_status")?;
    if processing_consumer.is_some() {
        method_ref("processing_intent")?;
    }
    let fields = metadata
        .descriptor
        .options
        .iter()
        .map(|option| (option.id.clone(), option.definition.clone()))
        .collect();
    let defaults = amaterasu_configuration_schema::defaults(&fields)?;
    let participant = json!({"provider":provider,"plan_method":plan,"callback_method":callback,
        "status_method":status,"settings_revision":"builtin-1","settings":defaults,
        "capabilities":metadata.descriptor.capabilities,"cleanup_methods":[callback]});
    let digest = format!(
        "{:x}",
        Sha256::digest(
            serde_json::to_vec(&metadata.descriptor).map_err(|error| error.to_string())?
        )
    );
    let grants: Vec<Value> = [COORDINATOR, "amaterasu.management", "amaterasu.console"]
        .into_iter()
        .filter(|caller| policy.principals.contains_key(*caller))
        .flat_map(|caller| {
            [
                "host",
                "all_applications",
                "all_paired_clients",
                "all_sessions",
            ]
            .map(|kind| {
                json!({
                    "caller":caller,"scope":{"kind":kind},"read":true,"write":caller != COORDINATOR,
                })
            })
        })
        .collect();
    let descriptor = json!({"descriptor":metadata.descriptor,"descriptor_sha256":digest,
        "grants":grants,"approved_methods":metadata.methods,"migrations":[]});
    let registry = policy
        .settings
        .get_mut(REGISTRY)
        .and_then(Value::as_object_mut)
        .ok_or("extension registry settings missing")?;
    let descriptors = registry
        .get_mut("descriptors")
        .and_then(Value::as_array_mut)
        .ok_or("extension registry descriptors missing")?;
    if descriptors.iter().any(|entry| {
        entry
            .pointer("/descriptor/provider")
            .and_then(Value::as_str)
            == Some(provider)
    }) {
        return Err("extension descriptor already installed".into());
    }
    descriptors.push(descriptor);
    let lifecycle = policy
        .settings
        .get_mut(COORDINATOR)
        .and_then(Value::as_object_mut)
        .ok_or("extension lifecycle settings missing")?;
    let participants = lifecycle
        .get_mut("participants")
        .and_then(Value::as_array_mut)
        .ok_or("extension lifecycle participants missing")?;
    if participants
        .iter()
        .any(|entry| entry.get("provider").and_then(Value::as_str) == Some(provider))
    {
        return Err("extension participant already installed".into());
    }
    participants.push(participant);
    let operations = lifecycle
        .get_mut("operation_grants")
        .and_then(Value::as_object_mut)
        .ok_or("extension lifecycle operation grants missing")?;
    if operations.contains_key(provider) {
        return Err("extension lifecycle principal already configured".into());
    }
    operations.insert(provider.clone(), json!(["read_provider_context"]));
    let trusted_callers = lifecycle
        .get_mut("trusted_callers")
        .and_then(Value::as_array_mut)
        .ok_or("extension lifecycle trusted callers missing")?;
    if trusted_callers
        .iter()
        .any(|caller| caller.as_str() == Some(provider))
    {
        return Err("extension lifecycle principal already trusted".into());
    }
    trusted_callers.push(json!(provider));
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
        let callers: std::collections::BTreeSet<String> =
            if method.method == metadata.descriptor.settings_validate.method {
                validation_callers
                    .iter()
                    .filter(|caller| {
                        policy
                            .principals
                            .get(**caller)
                            .is_some_and(|grants| grants.call.contains(&method.service))
                    })
                    .map(|caller| (*caller).into())
                    .collect()
            } else if method.method == "processing_intent" {
                let consumer =
                    processing_consumer.ok_or("processing intent needs a selected consumer")?;
                if !policy
                    .principals
                    .get(consumer)
                    .is_some_and(|grants| grants.call.contains(&method.service))
                {
                    return Err("processing consumer lacks call grant".into());
                }
                if !policy
                    .principals
                    .get(COORDINATOR)
                    .is_some_and(|grants| grants.call.contains(&method.service))
                {
                    return Err("lifecycle coordinator lacks extension call grant".into());
                }
                [consumer, COORDINATOR, "amaterasu.nvhttp", "amaterasu.rtsp"]
                    .into_iter()
                    .filter(|caller| {
                        policy
                            .principals
                            .get(*caller)
                            .is_some_and(|grants| grants.call.contains(&method.service))
                    })
                    .map(str::to_owned)
                    .collect()
            } else {
                if !policy
                    .principals
                    .get(COORDINATOR)
                    .is_some_and(|grants| grants.call.contains(&method.service))
                {
                    return Err("lifecycle coordinator lacks extension call grant".into());
                }
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
    let plugin = policy
        .settings
        .entry(provider.clone())
        .or_insert(json!({}))
        .as_object_mut()
        .ok_or("extension provider settings must be an object")?;
    for (name, expected) in [
        ("extension_registry_principal", json!(REGISTRY)),
        (
            "lifecycle_coordinator",
            json!({"principal":COORDINATOR,"service":COORDINATOR_SERVICE}),
        ),
    ] {
        if plugin
            .get(name)
            .is_some_and(|existing| *existing != expected)
        {
            return Err("extension provider settings conflict".into());
        }
        plugin.insert(name.into(), expected);
    }
    if let Some(consumer) = processing_consumer {
        if consumer == COORDINATOR {
            return Err("processing consumer must differ from coordinator".into());
        }
        let expected = json!([consumer, COORDINATOR]);
        if plugin
            .get("processing_consumer_principals")
            .is_some_and(|existing| *existing != expected)
        {
            return Err("extension processing consumer conflicts".into());
        }
        plugin.insert("processing_consumer_principals".into(), expected);
    }
    Ok(())
}

#[cfg(test)]
mod append_tests {
    use super::*;
    use amaterasu_contracts::Grants;
    use std::collections::BTreeSet;

    const RTX_METADATA: &str = include_str!("../../../../modules/rtx-hdr/metadata/linux.json");
    const SERVICE: &str = "nonary.rtx-hdr/1";
    const PROVIDER: &str = "amaterasu.rtx-hdr";
    const CONSUMER: &str = "amaterasu.stream-session";

    fn policy() -> RuntimeConfig {
        let mut policy = RuntimeConfig::default();
        let callers = [
            REGISTRY,
            COORDINATOR,
            CONSUMER,
            "amaterasu.management",
            "amaterasu.console",
        ];
        for caller in callers {
            policy.principals.insert(
                caller.into(),
                Grants {
                    call: [SERVICE.into()].into(),
                    ..Default::default()
                },
            );
        }
        policy.principals.insert(
            PROVIDER.into(),
            Grants {
                provide: [SERVICE.into()].into(),
                ..Default::default()
            },
        );
        policy.settings.insert(REGISTRY.into(), json!({"descriptors":[{"descriptor":{"provider":"other"},"preserved":true}],"other_setting":42}));
        policy.settings.insert(COORDINATOR.into(), json!({"participants":[{"provider":"other","settings":{"keep":7}}],"operation_grants":{"other":["custom"]},"trusted_callers":["other"],"journal_path":"/unchanged"}));
        policy.settings.insert(
            PROVIDER.into(),
            json!({"runtime_directory":"/trusted/runtime","vendor_setting":true}),
        );
        policy
    }

    #[test]
    fn extension_append_preserves_existing_configuration_and_scopes_intent() {
        let mut policy = policy();
        add_extension(&mut policy, RTX_METADATA, Some(CONSUMER)).unwrap();
        assert_eq!(policy.settings[REGISTRY]["other_setting"], 42);
        assert_eq!(
            policy.settings[REGISTRY]["descriptors"][0]["preserved"],
            true
        );
        assert_eq!(
            policy.settings[REGISTRY]["descriptors"][1]["descriptor"]["provider"],
            PROVIDER
        );
        assert_eq!(policy.settings[COORDINATOR]["journal_path"], "/unchanged");
        assert_eq!(
            policy.settings[COORDINATOR]["participants"][0]["settings"]["keep"],
            7
        );
        assert_eq!(
            policy.settings[COORDINATOR]["participants"][1]["provider"],
            PROVIDER
        );
        assert_eq!(
            policy.settings[PROVIDER]["runtime_directory"],
            "/trusted/runtime"
        );
        assert_eq!(
            policy.settings[PROVIDER]["processing_consumer_principals"],
            json!([CONSUMER, COORDINATOR])
        );
        let intent = policy
            .api_methods
            .iter()
            .find(|approval| approval.method.method == "processing_intent")
            .unwrap();
        assert_eq!(
            intent.callers,
            BTreeSet::from([CONSUMER.into(), COORDINATOR.into()])
        );
        let plan = policy
            .api_methods
            .iter()
            .find(|approval| approval.method.method == "resource_plan")
            .unwrap();
        assert_eq!(plan.callers, BTreeSet::from([COORDINATOR.into()]));
    }

    #[test]
    fn conflicts_and_missing_grants_leave_the_original_policy_unchanged() {
        for conflict in ["participant", "call", "registry", "runtime"] {
            let mut policy = policy();
            match conflict {
                "participant" => {
                    policy.settings.get_mut(COORDINATOR).unwrap()["participants"] =
                        json!([{"provider":PROVIDER}])
                }
                "call" => {
                    policy.principals.get_mut(CONSUMER).unwrap().call.clear();
                }
                "registry" => {
                    policy.principals.get_mut(REGISTRY).unwrap().call.clear();
                }
                "runtime" => {
                    policy.settings.get_mut(PROVIDER).unwrap()["extension_registry_principal"] =
                        json!("spoofed")
                }
                _ => unreachable!(),
            }
            let before = serde_json::to_value(&policy).unwrap();
            assert!(
                add_extension(&mut policy, RTX_METADATA, Some(CONSUMER)).is_err(),
                "{conflict}"
            );
            assert_eq!(serde_json::to_value(&policy).unwrap(), before, "{conflict}");
        }
    }
}
