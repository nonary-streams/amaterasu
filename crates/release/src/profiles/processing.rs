//! Trusted assembly of a selected SDK frame-processing graph.
use amaterasu_contracts::{ApiMethod, ApiMethodApproval, RuntimeConfig};
use amaterasu_module_sdk::{
    extension_api::ExtensionDescriptor,
    media::native_contracts,
    processing_composition::{
        self as sdk, CapabilityBinding, ProcessingComposition, ProcessingLifecycleGrant,
        ProcessingRegistration, ProcessingSelection, SettingsAccessGrant, SettingsScopeGrant,
    },
};
use amaterasu_package::PackageResult;
use serde::Deserialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

const STREAM_CONTRACT: &str = sdk::STREAM_OWNER_CONTRACT;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Metadata {
    descriptor: ExtensionDescriptor,
    methods: Vec<ApiMethod>,
    #[serde(default)]
    migrations: Vec<Value>,
}

/// Prepare only trusted selections and approvals. Provider-private settings
/// remain with their owning implementations.
pub(super) fn install(
    policy: &mut RuntimeConfig,
    selection: &ProcessingSelection,
    metadata: &str,
) -> PackageResult<()> {
    let mut candidate = policy.clone();
    install_into(&mut candidate, selection, metadata)?;
    *policy = candidate;
    Ok(())
}

fn install_into(
    policy: &mut RuntimeConfig,
    selection: &ProcessingSelection,
    metadata: &str,
) -> PackageResult<()> {
    selection.validate().map_err(str::to_owned)?;
    sdk::require_contract(
        &selection.processor,
        native_contracts::FRAME_PROCESSOR,
        1,
        0,
    )?;
    if selection.frame_consumer.contract != amaterasu_module_sdk::video::CONTRACT
        && selection.frame_consumer.contract != native_contracts::FRAME_RESOURCE_CONSUMER
    {
        return Err("selected frame consumer has an incompatible contract".into());
    }
    sdk::require_contract(
        &selection.frame_consumer,
        &selection.frame_consumer.contract,
        1,
        0,
    )?;
    if selection.display_settings.is_some() && selection.lifecycle.is_none() {
        return Err("display preparation requires a selected lifecycle contract".into());
    }
    check_provider(policy, &selection.processor, true)?;
    check_provider(policy, &selection.frame_consumer, true)?;
    if selection.stream_owner.is_some() || !selection.source_preflight_consumers.is_empty() {
        if selection.frame_consumer.contract != amaterasu_module_sdk::video::CONTRACT {
            return Err("stream owner requires a video control contract".into());
        }
        check_provider(policy, &selection.frame_consumer, false)?;
    }
    for (binding, contract) in [
        (&selection.stream_owner, STREAM_CONTRACT),
        (
            &selection.lifecycle,
            amaterasu_module_sdk::extension_api::SESSION_LIFECYCLE_SERVICE,
        ),
        (
            &selection.settings_registry,
            amaterasu_module_sdk::extension_api::EXTENSION_REGISTRY_SERVICE,
        ),
        (
            &selection.display_settings,
            amaterasu_module_sdk::display_settings::SERVICE,
        ),
        (
            &selection.foreground,
            amaterasu_module_sdk::foreground_context::SERVICE,
        ),
    ] {
        if let Some(binding) = binding {
            sdk::require_contract(binding, contract, 1, 0)?;
            check_provider(policy, binding, false)?;
        }
    }
    if selection.settings_registry.is_none() && !selection.settings_editors.is_empty() {
        return Err("settings editors require a selected registry".into());
    }
    for principal in selection
        .request_ancestors
        .iter()
        .chain(&selection.source_preflight_consumers)
        .chain(&selection.settings_editors)
        .chain(&selection.prepared_context_consumers)
        .chain(&selection.lifecycle_admitters)
        .chain(&selection.history_owners)
    {
        if !policy.principals.contains_key(principal) {
            return Err("processing caller is not approved".into());
        }
    }
    for principal in &selection.source_preflight_consumers {
        policy
            .principals
            .get_mut(principal)
            .unwrap()
            .call
            .insert(selection.frame_consumer.service.clone());
        let view = policy
            .processing_compositions
            .entry(principal.clone())
            .or_default();
        select(&mut view.frame_consumer, &selection.frame_consumer)?;
    }
    if selection.stream_owner.is_none()
        && selection.lifecycle.is_none()
        && selection.settings_registry.is_none()
    {
        if !selection.request_ancestors.is_empty() {
            return Err("frame-only processing does not use request ancestors".into());
        }
        policy
            .principals
            .get_mut(&selection.frame_consumer.principal)
            .unwrap()
            .native_use
            .insert(selection.processor.service.clone());
        if let Some(foreground) = &selection.foreground {
            policy
                .principals
                .get_mut(&selection.processor.principal)
                .unwrap()
                .call
                .insert(foreground.service.clone());
        }
        let view = policy
            .processing_compositions
            .entry(selection.processor.principal.clone())
            .or_default();
        select(&mut view.processor, &selection.processor)?;
        select(&mut view.frame_consumer, &selection.frame_consumer)?;
        select_optional(&mut view.foreground, &selection.foreground)?;
        let view = policy
            .processing_compositions
            .entry(selection.frame_consumer.principal.clone())
            .or_default();
        select(&mut view.processor, &selection.processor)?;
        select(&mut view.frame_consumer, &selection.frame_consumer)?;
        view.source_preflight_consumers
            .extend(selection.source_preflight_consumers.iter().cloned());
        if view
            .frame_settings
            .as_ref()
            .is_some_and(|old| Some(old) != selection.frame_settings.as_ref())
        {
            return Err("frame settings conflict with an existing selection".into());
        }
        view.frame_settings = selection.frame_settings.clone();
        for view in policy.processing_compositions.values() {
            sdk::validate(view)?;
        }
        return Ok(());
    }
    let metadata: Metadata = serde_json::from_str(metadata).map_err(|error| error.to_string())?;
    amaterasu_module_sdk::extension_api::validate_descriptor(&metadata.descriptor)?;
    if metadata.descriptor.provider != selection.processor.principal {
        return Err("processing metadata does not match the selected provider".into());
    }
    let intent = metadata
        .methods
        .iter()
        .find(|method| method.method == "processing_intent")
        .ok_or("processing metadata lacks intent method")?;
    let control = CapabilityBinding {
        principal: selection.processor.principal.clone(),
        service: intent.service.clone(),
        contract: sdk::PROCESSING_INTENT_CONTRACT.into(),
        interface_major: intent.interface_major,
        interface_minor: intent.interface_minor,
    };
    let source_preparation = selection
        .display_settings
        .as_ref()
        .map(|_| CapabilityBinding {
            principal: selection.processor.principal.clone(),
            service: control.service.clone(),
            contract: sdk::SOURCE_PREPARATION_CONTRACT.into(),
            interface_major: 1,
            interface_minor: 0,
        });
    check_provider(policy, &control, false)?;
    let fields = metadata
        .descriptor
        .options
        .iter()
        .map(|option| (option.id.clone(), option.definition.clone()))
        .collect();
    let initial_settings = serde_json::to_value(amaterasu_configuration_schema::defaults(&fields)?)
        .map_err(|error| error.to_string())?;

    policy
        .principals
        .get_mut(&selection.frame_consumer.principal)
        .unwrap()
        .native_use
        .insert(selection.processor.service.clone());
    if let Some(owner) = &selection.stream_owner {
        policy
            .principals
            .get_mut(&owner.principal)
            .ok_or("stream owner missing")?
            .call
            .insert(selection.frame_consumer.service.clone());
        for caller in selection
            .lifecycle_admitters
            .iter()
            .chain(&selection.history_owners)
        {
            policy
                .principals
                .get_mut(caller)
                .ok_or("stream caller missing")?
                .call
                .insert(owner.service.clone());
        }
    }
    for caller in &selection.request_ancestors {
        let grants = policy
            .principals
            .get_mut(caller)
            .ok_or("processing ancestor missing")?;
        for endpoint in [
            &selection.stream_owner,
            &selection.lifecycle,
            &selection.settings_registry,
        ]
        .into_iter()
        .flatten()
        {
            grants.call.insert(endpoint.service.clone());
        }
        if selection.stream_owner.is_some() {
            grants.call.insert(selection.frame_consumer.service.clone());
        }
    }
    let mut control_callers = selection.request_ancestors.clone();
    for binding in [
        &selection.stream_owner,
        &selection.lifecycle,
        &selection.settings_registry,
    ]
    .into_iter()
    .flatten()
    {
        control_callers.insert(binding.principal.clone());
    }
    for caller in &control_callers {
        policy
            .principals
            .get_mut(caller)
            .ok_or("processing caller missing")?
            .call
            .insert(control.service.clone());
    }
    if let Some(lifecycle) = &selection.lifecycle {
        for caller in &selection.prepared_context_consumers {
            policy
                .principals
                .get_mut(caller)
                .ok_or("prepared context consumer missing")?
                .call
                .insert(lifecycle.service.clone());
        }
        for caller in [
            Some(&selection.processor),
            Some(&selection.frame_consumer),
            selection.stream_owner.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            policy
                .principals
                .get_mut(&caller.principal)
                .ok_or("lifecycle caller missing")?
                .call
                .insert(lifecycle.service.clone());
        }
    }
    if let Some(registry) = &selection.settings_registry {
        if let Some(lifecycle) = &selection.lifecycle {
            policy
                .principals
                .get_mut(&registry.principal)
                .ok_or("registry missing")?
                .call
                .insert(lifecycle.service.clone());
        }
        for caller in selection
            .lifecycle
            .iter()
            .map(|binding| &binding.principal)
            .chain(&selection.settings_editors)
        {
            policy
                .principals
                .get_mut(caller)
                .ok_or("registry caller missing")?
                .call
                .insert(registry.service.clone());
        }
    }
    for binding in [&selection.display_settings, &selection.foreground]
        .into_iter()
        .flatten()
    {
        policy
            .principals
            .get_mut(&selection.processor.principal)
            .unwrap()
            .call
            .insert(binding.service.clone());
    }
    if let Some(display) = &selection.display_settings {
        let owner = selection
            .stream_owner
            .as_ref()
            .ok_or("display preparation needs a stream owner")?;
        let mut callers = selection.request_ancestors.clone();
        callers.insert(owner.principal.clone());
        callers.insert(selection.processor.principal.clone());
        callers.extend(
            selection
                .lifecycle
                .iter()
                .chain(&selection.settings_registry)
                .map(|binding| binding.principal.clone()),
        );
        for caller in callers {
            policy
                .principals
                .get_mut(&caller)
                .ok_or("display preparation caller missing")?
                .call
                .insert(display.service.clone());
        }
        for method in amaterasu_module_sdk::display_settings::methods(&display.service) {
            let mut callers = BTreeSet::from([owner.principal.clone()]);
            if method.method == "acquire" {
                callers.insert(selection.processor.principal.clone());
            }
            approve(
                policy,
                ApiMethodApproval {
                    provider: display.principal.clone(),
                    callers,
                    method,
                },
            )?;
        }
        approve(
            policy,
            ApiMethodApproval {
                provider: selection.processor.principal.clone(),
                callers: BTreeSet::from([owner.principal.clone()]),
                method: amaterasu_module_sdk::display_settings::preparation::method(
                    &control.service,
                ),
            },
        )?;
    }

    let mut lifecycle_grants: BTreeMap<String, ProcessingLifecycleGrant> = BTreeMap::new();
    if selection.lifecycle.is_some() {
        lifecycle_grant(&mut lifecycle_grants, &selection.processor.principal).read_context = true;
        if selection.settings_registry.is_some() {
            lifecycle_grant(&mut lifecycle_grants, &selection.frame_consumer.principal)
                .read_live_settings = true;
        }
        if let Some(owner) = &selection.stream_owner {
            lifecycle_grant(&mut lifecycle_grants, &owner.principal).read_frozen_settings = true;
        }
    }
    let mut settings_grants = Vec::new();
    if selection.settings_registry.is_some() {
        let scopes = [
            SettingsScopeGrant::Host,
            SettingsScopeGrant::AllApplications,
            SettingsScopeGrant::AllPairedClients,
            SettingsScopeGrant::AllSessions,
        ];
        for caller in selection
            .lifecycle
            .iter()
            .map(|binding| &binding.principal)
            .chain(&selection.settings_editors)
        {
            for scope in &scopes {
                settings_grants.push(SettingsAccessGrant {
                    caller: caller.clone(),
                    scope: scope.clone(),
                    read: true,
                    write: selection.settings_editors.contains(caller),
                });
            }
        }
    }
    let consumers = selection
        .stream_owner
        .iter()
        .chain(&selection.lifecycle)
        .map(|binding| binding.principal.clone())
        .collect();
    let registration = ProcessingRegistration {
        provider: selection.processor.principal.clone(),
        descriptor: serde_json::to_value(&metadata.descriptor)
            .map_err(|error| error.to_string())?,
        methods: metadata.methods,
        consumers,
        depends_on: Vec::new(),
        lifecycle_grants: lifecycle_grants.into_values().collect(),
        settings_revision: "composition-1".into(),
        initial_settings,
        settings_grants,
        migrations: metadata.migrations,
    };
    registration.validate().map_err(str::to_owned)?;
    for method in &registration.methods {
        let callers: BTreeSet<String> = match method.method.as_str() {
            "resource_plan" | "lifecycle_callback" | "lifecycle_status" => selection
                .lifecycle
                .iter()
                .map(|binding| binding.principal.clone())
                .collect(),
            "processing_intent" => selection
                .stream_owner
                .iter()
                .chain(&selection.lifecycle)
                .map(|binding| binding.principal.clone())
                .chain(selection.request_ancestors.iter().cloned())
                .collect(),
            "settings_validate" => selection
                .settings_registry
                .iter()
                .chain(&selection.lifecycle)
                .map(|binding| binding.principal.clone())
                .chain(selection.request_ancestors.iter().cloned())
                .collect(),
            _ => BTreeSet::new(),
        };
        if !callers.is_empty() {
            approve(
                policy,
                ApiMethodApproval {
                    provider: registration.provider.clone(),
                    callers,
                    method: method.clone(),
                },
            )?;
        }
    }
    let views = &mut policy.processing_compositions;
    {
        let view = views
            .entry(selection.processor.principal.clone())
            .or_default();
        select(&mut view.processor, &selection.processor)?;
        select(&mut view.processor_control, &control)?;
        select(&mut view.frame_consumer, &selection.frame_consumer)?;
        select_optional(&mut view.stream_owner, &selection.stream_owner)?;
        select_optional(&mut view.lifecycle, &selection.lifecycle)?;
        select_optional(&mut view.settings_registry, &selection.settings_registry)?;
        select_optional(&mut view.display_settings, &selection.display_settings)?;
        select_optional(&mut view.source_preparation, &source_preparation)?;
        select_optional(&mut view.foreground, &selection.foreground)?;
    }
    {
        let view = views
            .entry(selection.frame_consumer.principal.clone())
            .or_default();
        select(&mut view.processor, &selection.processor)?;
        select(&mut view.frame_consumer, &selection.frame_consumer)?;
        view.source_preflight_consumers
            .extend(selection.source_preflight_consumers.iter().cloned());
        select_optional(&mut view.stream_owner, &selection.stream_owner)?;
        select_optional(&mut view.lifecycle, &selection.lifecycle)?;
        if view
            .frame_settings
            .as_ref()
            .is_some_and(|old| Some(old) != selection.frame_settings.as_ref())
        {
            return Err("frame settings conflict with an existing selection".into());
        }
        view.frame_settings = selection.frame_settings.clone();
    }
    if let Some(owner) = &selection.stream_owner {
        let view = views.entry(owner.principal.clone()).or_default();
        select(&mut view.processor, &selection.processor)?;
        select(&mut view.processor_control, &control)?;
        select(&mut view.frame_consumer, &selection.frame_consumer)?;
        select(&mut view.stream_owner, owner)?;
        select_optional(&mut view.lifecycle, &selection.lifecycle)?;
        select_optional(&mut view.settings_registry, &selection.settings_registry)?;
        select_optional(&mut view.display_settings, &selection.display_settings)?;
        select_optional(&mut view.source_preparation, &source_preparation)?;
        view.request_ancestors
            .extend(selection.request_ancestors.iter().cloned());
        view.lifecycle_admitters
            .extend(selection.lifecycle_admitters.iter().cloned());
        view.history_owners
            .extend(selection.history_owners.iter().cloned());
    }
    if let Some(lifecycle) = &selection.lifecycle {
        let view = views.entry(lifecycle.principal.clone()).or_default();
        select(&mut view.processor, &selection.processor)?;
        select(&mut view.processor_control, &control)?;
        select(&mut view.lifecycle, lifecycle)?;
        select_optional(&mut view.settings_registry, &selection.settings_registry)?;
        view.prepared_context_consumers
            .extend(selection.prepared_context_consumers.iter().cloned());
        add_registration(view, &registration)?;
    }
    if let Some(registry) = &selection.settings_registry {
        let view = views.entry(registry.principal.clone()).or_default();
        select(&mut view.settings_registry, registry)?;
        select_optional(&mut view.lifecycle, &selection.lifecycle)?;
        add_registration(view, &registration)?;
    }
    for caller in &selection.prepared_context_consumers {
        let view = views.entry(caller.clone()).or_default();
        select_optional(&mut view.lifecycle, &selection.lifecycle)?;
    }
    for ancestor in &selection.request_ancestors {
        if ancestor == "amaterasu.console" {
            continue;
        }
        let view = views.entry(ancestor.clone()).or_default();
        select_optional(&mut view.stream_owner, &selection.stream_owner)?;
        select_optional(&mut view.lifecycle, &selection.lifecycle)?;
        select_optional(&mut view.settings_registry, &selection.settings_registry)?;
        if selection.stream_owner.is_some() {
            select(&mut view.frame_consumer, &selection.frame_consumer)?;
        }
    }
    for view in views.values() {
        sdk::validate(view)?;
    }
    Ok(())
}

fn lifecycle_grant<'a>(
    grants: &'a mut BTreeMap<String, ProcessingLifecycleGrant>,
    caller: &str,
) -> &'a mut ProcessingLifecycleGrant {
    grants
        .entry(caller.into())
        .or_insert_with(|| ProcessingLifecycleGrant {
            caller: caller.into(),
            read_context: false,
            read_frozen_settings: false,
            read_live_settings: false,
        })
}

fn check_provider(
    policy: &RuntimeConfig,
    binding: &CapabilityBinding,
    native: bool,
) -> PackageResult<()> {
    let grants = policy
        .principals
        .get(&binding.principal)
        .ok_or("selected processing provider is not approved")?;
    if !(if native {
        &grants.native_provide
    } else {
        &grants.provide
    })
    .contains(&binding.service)
    {
        return Err("selected processing service is not approved".into());
    }
    Ok(())
}

fn select(slot: &mut Option<CapabilityBinding>, selected: &CapabilityBinding) -> PackageResult<()> {
    if slot.as_ref().is_some_and(|old| old != selected) {
        return Err("processing capability conflicts with an existing selection".into());
    }
    *slot = Some(selected.clone());
    Ok(())
}

fn select_optional(
    slot: &mut Option<CapabilityBinding>,
    selected: &Option<CapabilityBinding>,
) -> PackageResult<()> {
    if let Some(selected) = selected {
        select(slot, selected)?;
    }
    Ok(())
}

fn add_registration(
    view: &mut ProcessingComposition,
    registration: &ProcessingRegistration,
) -> PackageResult<()> {
    if let Some(old) = view
        .registrations
        .iter()
        .find(|old| old.provider == registration.provider)
    {
        if old != registration {
            return Err("processing registration conflicts".into());
        }
    } else {
        view.registrations.push(registration.clone());
    }
    Ok(())
}

fn approve(policy: &mut RuntimeConfig, approval: ApiMethodApproval) -> PackageResult<()> {
    if let Some(old) = policy.api_methods.iter_mut().find(|old| {
        old.method.service == approval.method.service
            && old.method.method == approval.method.method
            && old.method.interface_major == approval.method.interface_major
    }) {
        if old.provider != approval.provider || old.method != approval.method {
            return Err("processing API approval conflicts".into());
        }
        old.callers.extend(approval.callers);
    } else {
        policy.api_methods.push(approval);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use amaterasu_contracts::Grants;
    use serde_json::json;

    fn endpoint(principal: &str, service: &str, contract: &str) -> CapabilityBinding {
        CapabilityBinding {
            principal: principal.into(),
            service: service.into(),
            contract: contract.into(),
            interface_major: 1,
            interface_minor: 0,
        }
    }

    fn metadata() -> String {
        let methods: Vec<_> = [
            "settings_validate",
            "resource_plan",
            "lifecycle_callback",
            "lifecycle_status",
            "processing_intent",
        ]
        .into_iter()
        .map(|method| {
            let mut api = amaterasu_module_sdk::operation_api::method(
                "other.effect-control/1",
                method,
                &[],
                &[],
            );
            if method == "processing_intent" {
                api.interface_minor = 1;
            }
            api
        })
        .collect();
        json!({"descriptor":{"provider":"other.effect","revision":1,"options":[],"actions":[],"capabilities":[],"settings_validate":{"service":"other.effect-control/1","method":"settings_validate","interface_major":1,"interface_minor":0,"schema_revision":1}},"methods":methods,"migrations":[]}).to_string()
    }

    fn fixture(managed: bool) -> (RuntimeConfig, ProcessingSelection) {
        let selection = ProcessingSelection {
            processor: endpoint(
                "other.effect",
                "other.effect/1",
                native_contracts::FRAME_PROCESSOR,
            ),
            frame_consumer: endpoint(
                "other.video",
                "other.video/1",
                amaterasu_module_sdk::video::CONTRACT,
            ),
            frame_settings: (!managed).then(|| json!({})),
            stream_owner: managed
                .then(|| endpoint("other.stream", "other.stream/1", STREAM_CONTRACT)),
            lifecycle: managed.then(|| {
                endpoint(
                    "other.lifecycle",
                    "other.lifecycle/1",
                    amaterasu_module_sdk::extension_api::SESSION_LIFECYCLE_SERVICE,
                )
            }),
            settings_registry: managed.then(|| {
                endpoint(
                    "other.registry",
                    "other.registry/1",
                    amaterasu_module_sdk::extension_api::EXTENSION_REGISTRY_SERVICE,
                )
            }),
            display_settings: None,
            foreground: None,
            source_preflight_consumers: BTreeSet::new(),
            request_ancestors: if managed {
                ["other.transport".into()].into()
            } else {
                BTreeSet::new()
            },
            settings_editors: if managed {
                ["other.editor".into()].into()
            } else {
                BTreeSet::new()
            },
            prepared_context_consumers: BTreeSet::new(),
            lifecycle_admitters: if managed {
                ["other.transport".into()].into()
            } else {
                BTreeSet::new()
            },
            history_owners: BTreeSet::new(),
        };
        let mut policy = RuntimeConfig::default();
        for principal in [
            "other.effect",
            "other.video",
            "other.stream",
            "other.lifecycle",
            "other.registry",
            "other.transport",
            "other.editor",
        ] {
            policy
                .principals
                .insert(principal.into(), Grants::default());
            policy
                .settings
                .insert(principal.into(), json!({"provider_private": principal}));
        }
        policy
            .principals
            .get_mut("other.effect")
            .unwrap()
            .native_provide
            .insert("other.effect/1".into());
        policy
            .principals
            .get_mut("other.effect")
            .unwrap()
            .provide
            .insert("other.effect-control/1".into());
        policy
            .principals
            .get_mut("other.video")
            .unwrap()
            .native_provide
            .insert("other.video/1".into());
        policy
            .principals
            .get_mut("other.video")
            .unwrap()
            .provide
            .insert("other.video/1".into());
        for binding in [
            &selection.stream_owner,
            &selection.lifecycle,
            &selection.settings_registry,
        ]
        .into_iter()
        .flatten()
        {
            policy
                .principals
                .get_mut(&binding.principal)
                .unwrap()
                .provide
                .insert(binding.service.clone());
        }
        (policy, selection)
    }

    #[test]
    fn frame_only_processing_needs_no_lifecycle_or_registry() {
        let (mut policy, selection) = fixture(false);
        let before = policy.settings.clone();
        policy
            .principals
            .get_mut("other.effect")
            .unwrap()
            .provide
            .clear();
        install(
            &mut policy,
            &selection,
            "no metadata is needed for native-only processing",
        )
        .unwrap();
        assert_eq!(policy.settings, before);
        assert_eq!(policy.processing_compositions.len(), 2);
        assert!(
            policy.processing_compositions["other.effect"]
                .processor_control
                .is_none()
        );
        assert!(
            policy.processing_compositions["other.effect"]
                .lifecycle
                .is_none()
        );
        assert!(
            policy.processing_compositions["other.effect"]
                .settings_registry
                .is_none()
        );
        assert!(
            policy.principals["other.video"]
                .native_use
                .contains("other.effect/1")
        );
    }

    #[test]
    fn selected_replacements_receive_contract_views_without_private_setting_mutation() {
        let (mut policy, selection) = fixture(true);
        let before = policy.settings.clone();
        install(&mut policy, &selection, &metadata()).unwrap();
        assert_eq!(policy.settings, before);
        let lifecycle = &policy.processing_compositions["other.lifecycle"];
        assert_eq!(
            lifecycle.processor.as_ref().unwrap().principal,
            "other.effect"
        );
        assert_eq!(
            lifecycle.settings_registry.as_ref().unwrap().service,
            "other.registry/1"
        );
        assert_eq!(lifecycle.registrations.len(), 1);
        assert_eq!(
            policy.processing_compositions["other.registry"]
                .registrations
                .len(),
            1
        );
        assert!(
            policy.processing_compositions["other.video"]
                .registrations
                .is_empty()
        );
        assert!(
            policy.principals["other.video"]
                .call
                .contains("other.lifecycle/1")
        );
        assert!(
            policy.principals["other.editor"]
                .call
                .contains("other.registry/1")
        );
        assert!(
            !policy.principals["other.editor"]
                .native_use
                .contains("other.effect/1")
        );
    }
}
