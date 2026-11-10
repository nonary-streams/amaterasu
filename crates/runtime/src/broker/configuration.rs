use amaterasu_contracts::{
    Grants, HOST_LIFECYCLE_CONTROL_PRINCIPAL, HookSpec, RuntimeConfig, valid_name,
};
use std::collections::BTreeSet;

const MAX_PRINCIPALS: usize = 128;
const MAX_GRANTS_PER_KIND: usize = 256;
const MAX_HOOK_PARTICIPANTS: usize = 32;

pub(super) fn validate(configuration: &RuntimeConfig) -> Result<(), String> {
    if configuration.host_package_activation_control && !configuration.host_lifecycle_control {
        return Err("package activation requires the host control endpoint".into());
    }
    if configuration.principals.len() > MAX_PRINCIPALS {
        return Err("too many principals".into());
    }

    for (name, grants) in &configuration.principals {
        if name == HOST_LIFECYCLE_CONTROL_PRINCIPAL {
            if !configuration.host_lifecycle_control || !grants_are_empty(grants) {
                return Err("reserved host lifecycle identity is not host-authorized".into());
            }
            continue;
        }
        if !valid_name(name) {
            return Err(format!("invalid principal name: {name}"));
        }
        validate_grants(grants)?;
    }

    if configuration.host_lifecycle_control
        != configuration
            .principals
            .contains_key(HOST_LIFECYCLE_CONTROL_PRINCIPAL)
    {
        return Err("host lifecycle endpoint identity is missing or untrusted".into());
    }

    if configuration.legacy_native_contracts.len() > MAX_PRINCIPALS {
        return Err("too many legacy native contract principals".into());
    }
    for (principal, services) in &configuration.legacy_native_contracts {
        let grants = configuration
            .principals
            .get(principal)
            .ok_or("legacy native contract references unknown principal")?;
        if services.len() > MAX_GRANTS_PER_KIND
            || services.iter().any(|(service, contract)| {
                !valid_name(service)
                    || !valid_name(contract)
                    || !grants.native_provide.contains(service)
            })
        {
            return Err("invalid or ungranted legacy native contract".into());
        }
    }

    for (principal, settings) in &configuration.settings {
        if !configuration.principals.contains_key(principal) {
            return Err(format!("settings refer to unknown principal: {principal}"));
        }
        if !super::errors::payload_fits(settings) {
            return Err(format!(
                "settings payload is too large for principal: {principal}"
            ));
        }
    }

    if configuration.processing_compositions.len() > MAX_PRINCIPALS {
        return Err("too many processing compositions".into());
    }
    for (principal, composition) in &configuration.processing_compositions {
        if !configuration.principals.contains_key(principal) {
            return Err("processing composition refers to unknown principal".into());
        }
        composition.validate().map_err(str::to_owned)?;
        let payload = serde_json::to_value(composition).map_err(|error| error.to_string())?;
        if !super::errors::payload_fits(&payload) {
            return Err("processing composition exceeds payload limit".into());
        }
        for caller in &composition.prepared_context_consumers {
            let lifecycle = composition
                .lifecycle
                .as_ref()
                .ok_or("prepared context has no lifecycle")?;
            if !configuration
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
            let grants = configuration
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
            let grants = configuration
                .principals
                .get(caller)
                .ok_or("processing caller is not selected")?;
            let owner = composition
                .stream_owner
                .as_ref()
                .ok_or("processing caller has no stream owner")?;
            if !grants.call.contains(&owner.service) {
                return Err("processing caller lacks stream owner grant".into());
            }
        }
        for binding in composition.bindings() {
            let grants = configuration
                .principals
                .get(&binding.principal)
                .ok_or("processing binding refers to unknown provider")?;
            if !grants.provide.contains(&binding.service)
                && !grants.native_provide.contains(&binding.service)
            {
                return Err("processing binding refers to an unapproved service".into());
            }
        }
        for registration in &composition.registrations {
            let grants = configuration
                .principals
                .get(&registration.provider)
                .ok_or("processing registration refers to unknown provider")?;
            if registration
                .methods
                .iter()
                .any(|method| !grants.provide.contains(&method.service))
                || registration
                    .consumers
                    .iter()
                    .any(|consumer| !configuration.principals.contains_key(consumer))
                || registration
                    .settings_grants
                    .iter()
                    .any(|grant| !configuration.principals.contains_key(&grant.caller))
                || registration
                    .lifecycle_grants
                    .iter()
                    .any(|grant| !configuration.principals.contains_key(&grant.caller))
                || registration
                    .depends_on
                    .iter()
                    .any(|provider| !configuration.principals.contains_key(provider))
            {
                return Err(
                    "processing registration refers to an unapproved service or consumer".into(),
                );
            }
        }
    }

    for (name, hook) in &configuration.hooks {
        validate_hook(name, hook, configuration)?;
    }
    configuration.validate_service_layers()?;
    super::api::validate_approvals(configuration)?;
    Ok(())
}

fn grants_are_empty(grants: &Grants) -> bool {
    grants.native_provide.is_empty()
        && grants.native_use.is_empty()
        && grants.provide.is_empty()
        && grants.call.is_empty()
        && grants.publish.is_empty()
        && grants.subscribe.is_empty()
        && grants.hook.is_empty()
}

fn validate_grants(grants: &Grants) -> Result<(), String> {
    let grant_sets = [
        &grants.native_provide,
        &grants.native_use,
        &grants.call,
        &grants.provide,
        &grants.publish,
        &grants.subscribe,
        &grants.hook,
    ];

    for names in grant_sets {
        if names.len() > MAX_GRANTS_PER_KIND {
            return Err("too many grants in a permission category".into());
        }
        if names.iter().any(|name| !valid_name(name)) {
            return Err("invalid interface name in permission grant".into());
        }
    }
    Ok(())
}

fn validate_hook(name: &str, hook: &HookSpec, configuration: &RuntimeConfig) -> Result<(), String> {
    if !valid_name(name) || !configuration.principals.contains_key(&hook.owner) {
        return Err(format!("invalid hook name or owner: {name}"));
    }
    if !(1..=30_000).contains(&hook.timeout_ms) {
        return Err(format!("invalid hook deadline: {name}"));
    }

    let unique_participants: BTreeSet<_> = hook.participants.iter().collect();
    if hook.participants.len() > MAX_HOOK_PARTICIPANTS
        || unique_participants.len() != hook.participants.len()
    {
        return Err(format!("invalid or duplicate hook participants: {name}"));
    }
    for participant in &hook.participants {
        if !configuration.principals.contains_key(participant) {
            return Err(format!("unknown participant {participant} in hook {name}"));
        }
    }
    Ok(())
}
