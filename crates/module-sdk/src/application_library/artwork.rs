//! Shared library artwork routing. Only startup configuration chooses services;
//! provider-local references remain opaque to the facade and its consumers.
use super::provider::{ArtworkReference, Capabilities};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub const MAX_ROUTES: usize = 32;
const MAX_RESPONSE_BYTES: usize = 12_000;

pub fn valid_source(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}
pub fn valid_reference(reference: &ArtworkReference) -> bool {
    [&reference.id, &reference.revision]
        .into_iter()
        .all(|value| {
            !value.trim().is_empty() && value.len() <= 128 && !value.chars().any(char::is_control)
        })
}

/// Provider binding approved by the application composition. This grants no
/// broker authority: normal call grants and parent intersections still apply.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoutePolicy {
    pub service: String,
    pub allowed_initiators: BTreeSet<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtworkRoutes {
    #[serde(default)]
    pub providers: BTreeMap<String, RoutePolicy>,
}
impl ArtworkRoutes {
    pub fn from_module_settings(settings: &Value) -> Result<Self, String> {
        let routes: Self = settings
            .get("artwork")
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|error| format!("invalid artwork configuration: {error}"))?
            .unwrap_or_default();
        if routes.providers.len() > MAX_ROUTES
            || routes.providers.iter().any(|(alias, route)| {
                !valid_source(alias)
                    || route
                        .service
                        .rsplit_once('/')
                        .is_none_or(|(name, version)| {
                            !amaterasu_contracts::valid_name(name) || version != "1"
                        })
                    || route.service == "amaterasu.library/1"
                    || route.service == "amaterasu.catalog/1"
                    || route.allowed_initiators.is_empty()
                    || route.allowed_initiators.len() > 32
                    || route
                        .allowed_initiators
                        .iter()
                        .any(|id| !amaterasu_contracts::valid_name(id))
            })
        {
            return Err("invalid artwork provider configuration".into());
        }
        Ok(routes)
    }
    pub fn open(
        &self,
        source: &str,
        reference: &ArtworkReference,
        initiator: &str,
    ) -> Result<(String, Value), &'static str> {
        if !valid_source(source) || !valid_reference(reference) {
            return Err("invalid_artwork_reference");
        }
        let route = self
            .providers
            .get(source)
            .ok_or("artwork_provider_not_selected")?;
        if !route.allowed_initiators.contains(initiator) {
            return Err("forbidden");
        }
        Ok((
            route.service.clone(),
            json!({"action":"artwork_open","reference":reference}),
        ))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Description {
    ok: bool,
    capabilities: Capabilities,
}
pub fn supports_artwork(payload: Value) -> bool {
    serde_json::to_vec(&payload).is_ok_and(|bytes| bytes.len() <= MAX_RESPONSE_BYTES)
        && serde_json::from_value::<Description>(payload)
            .is_ok_and(|description| description.ok && description.capabilities.artwork)
}
