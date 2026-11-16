//! Shared declaration validation used before preparation and at runtime.
use crate::{ApiSchema, RuntimeConfig, valid_name};
use std::collections::{BTreeMap, BTreeSet};
pub fn validate_approvals(configuration: &RuntimeConfig) -> Result<(), String> {
    if configuration.api_methods.len() > 256 {
        return Err("too many approved API methods".into());
    }
    let mut keys = BTreeSet::new();
    let mut service_providers = BTreeMap::new();
    for approval in &configuration.api_methods {
        let method = &approval.method;
        let service_version = method
            .service
            .rsplit_once('/')
            .and_then(|(namespace, version)| {
                (namespace.contains('.')
                    && !namespace.starts_with('.')
                    && !namespace.ends_with('.'))
                .then(|| version.parse::<u32>().ok())
                .flatten()
            });
        if !valid_name(&method.service)
            || method.service.starts_with("amaterasu.")
            || method.service.starts_with("hooks/")
            || !valid_name(&method.method)
            || method.method.contains('/')
            || method.interface_major == 0
            || service_version != Some(method.interface_major)
            || method.request.required.len() + method.request.optional.len() > 64
            || method.response.required.len() + method.response.optional.len() > 64
            || !valid_schema(&method.request)
            || !valid_schema(&method.response)
            || !configuration.principals.contains_key(&approval.provider)
            || !configuration.principals[&approval.provider]
                .provide
                .contains(&method.service)
            || approval.callers.is_empty()
            || approval.callers.iter().any(|caller| {
                configuration
                    .principals
                    .get(caller)
                    .is_none_or(|grants| !grants.call.contains(&method.service))
            })
            || !keys.insert((&method.service, &method.method, method.interface_major))
            || service_providers
                .insert(&method.service, &approval.provider)
                .is_some_and(|old| old != &approval.provider)
        {
            return Err("invalid or duplicate installed API approval".into());
        }
    }
    Ok(())
}

fn valid_schema(schema: &ApiSchema) -> bool {
    schema
        .required
        .keys()
        .chain(schema.optional.keys())
        .all(|name| valid_name(name) && !name.contains('/'))
        && schema
            .required
            .keys()
            .all(|name| !schema.optional.contains_key(name))
}
