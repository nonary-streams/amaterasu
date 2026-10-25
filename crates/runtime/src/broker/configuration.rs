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
