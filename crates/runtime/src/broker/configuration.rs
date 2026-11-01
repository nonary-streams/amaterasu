use amaterasu_contracts::{
    Grants, HOST_LIFECYCLE_CONTROL_PRINCIPAL, HookSpec, RuntimeConfig, valid_name,
};
use std::collections::BTreeSet;

const MAX_PRINCIPALS: usize = 128;
const MAX_GRANTS_PER_KIND: usize = 256;
const MAX_HOOK_PARTICIPANTS: usize = 32;

pub(super) fn validate(configuration: &RuntimeConfig) -> Result<(), String> {
    if configuration.principals.len() > MAX_PRINCIPALS {
        return Err("too many principals".into());
    }

    let mut instance_ids = BTreeSet::new();
    for (principal, instance) in &configuration.host_context.provider_incarnations {
        if !configuration.principals.contains_key(principal)
            || instance.version != 1
            || instance.id.len() != 64
            || !instance
                .id
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            || !instance_ids.insert(&instance.id)
        {
            return Err("invalid host provider incarnation".into());
        }
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

    for (name, hook) in &configuration.hooks {
        validate_hook(name, hook, configuration)?;
    }
    super::api::validate_approvals(configuration)?;
    validate_selections(configuration)?;
    Ok(())
}

fn validate_selections(configuration: &RuntimeConfig) -> Result<(), String> {
    for (native, entries) in [
        (false, &configuration.service_selections.broker),
        (true, &configuration.service_selections.native),
    ] {
        if entries.len() > MAX_GRANTS_PER_KIND {
            return Err("too many service selections".into());
        }
        for (service, selection) in entries {
            if !valid_name(service) || service == amaterasu_contracts::HOST_LOG_SERVICE {
                return Err(format!("invalid selected service: {service}"));
            }
            if let Some(selection) = selection {
                let grants = configuration
                    .principals
                    .get(&selection.provider)
                    .ok_or_else(|| format!("unknown selected provider for {service}"))?;
                if !valid_name(&selection.contract)
                    || !(if native {
                        &grants.native_provide
                    } else {
                        &grants.provide
                    })
                    .contains(service)
                {
                    return Err(format!("ungranted selected provider for {service}"));
                }
            }
        }
    }
    for approval in &configuration.api_methods {
        if let Some(Some(selection)) = configuration
            .service_selections
            .broker
            .get(&approval.method.service)
            && selection.provider != approval.provider
        {
            return Err(format!(
                "service selection conflicts with API approval for {}",
                approval.method.service
            ));
        }
    }
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
