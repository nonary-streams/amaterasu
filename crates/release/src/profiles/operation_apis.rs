//! Explicit composition of module-owned public operation contracts.
use amaterasu_contracts::{ApiMethod, ApiMethodApproval, RuntimeConfig};

pub(super) fn install(policy: &mut RuntimeConfig) -> Result<(), String> {
    for (provider, methods) in [
        (
            "amaterasu.display",
            include_str!("../../../../modules/display/metadata/api.json"),
        ),
        (
            "amaterasu.catalog",
            include_str!("../../../../modules/catalog/metadata/api.json"),
        ),
        (
            "amaterasu.nvhttp",
            include_str!("../../../../modules/nvhttp/metadata/api.json"),
        ),
        (
            "amaterasu.host-metrics",
            include_str!("../../../../modules/stats/metadata/api.json"),
        ),
        (
            "amaterasu.session-history",
            include_str!("../../../../modules/stats/history/metadata/api.json"),
        ),
        (
            "amaterasu.stream-session",
            include_str!("../../../../modules/stream-session/metadata/api.json"),
        ),
    ] {
        if !policy.principals.contains_key(provider) {
            continue;
        }
        let methods: Vec<ApiMethod> =
            serde_json::from_str(methods).map_err(|error| error.to_string())?;
        for method in methods {
            approve(policy, provider, method)?;
        }
    }
    Ok(())
}

fn approve(policy: &mut RuntimeConfig, provider: &str, method: ApiMethod) -> Result<(), String> {
    let caller = "amaterasu.management";
    if !policy.principals.contains_key(caller) {
        return Ok(());
    }
    if !policy
        .principals
        .get(provider)
        .is_some_and(|grants| grants.provide.contains(&method.service))
    {
        return Err("operation API provider does not declare its service".into());
    }
    if !policy.principals[caller].call.contains(&method.service) {
        return Err("operation API caller lacks the selected service grant".into());
    }
    if let Some(existing) = policy.api_methods.iter().find(|entry| {
        entry.method.service == method.service
            && entry.method.method == method.method
            && entry.method.interface_major == method.interface_major
    }) {
        if existing.provider != provider
            || serde_json::to_value(&existing.method).unwrap()
                != serde_json::to_value(&method).unwrap()
        {
            return Err("operation API composition conflict".into());
        }
        return Ok(());
    }
    policy.api_methods.push(ApiMethodApproval {
        provider: provider.into(),
        callers: [caller.into()].into(),
        method,
    });
    Ok(())
}
