//! Shared library artwork routing. Only startup configuration chooses services;
//! provider-local references remain opaque to the facade and its consumers.
use super::provider::{
    ArtworkChunk, ArtworkReference, Capabilities, MAX_ARTWORK_BYTES, MAX_ARTWORK_CHUNK_BYTES,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
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
    pub fn route(
        &self,
        source: &str,
        reference: &ArtworkReference,
        initiator: &str,
        offset: u64,
    ) -> Result<(String, Value), &'static str> {
        if !valid_source(source)
            || !valid_reference(reference)
            || offset >= MAX_ARTWORK_BYTES
            || !offset.is_multiple_of(MAX_ARTWORK_CHUNK_BYTES as u64)
        {
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
            json!({"action":"artwork","reference":reference,"offset":offset}),
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

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Reply {
    ok: bool,
    reference: ArtworkReference,
    mime: String,
    offset: u64,
    total_bytes: u64,
    data_base64: String,
}
/// Decode exactly the SDK provider contract, validate its opaque identity and
/// canonical bytes, and normalize it to the client-facing chunk contract.
pub fn project_chunk(
    payload: Value,
    reference: &ArtworkReference,
    offset: u64,
) -> Result<Value, &'static str> {
    if serde_json::to_vec(&payload).map_or(true, |bytes| bytes.len() > MAX_RESPONSE_BYTES) {
        return Err("invalid_artwork_response");
    }
    let reply: Reply = serde_json::from_value(payload).map_err(|_| "invalid_artwork_response")?;
    if !reply.ok || reply.data_base64.len() > 4 * MAX_ARTWORK_CHUNK_BYTES.div_ceil(3) {
        return Err("invalid_artwork_response");
    }
    let bytes = STANDARD
        .decode(&reply.data_base64)
        .map_err(|_| "invalid_artwork_response")?;
    let chunk = ArtworkChunk {
        reference: reply.reference,
        mime: reply.mime,
        offset: reply.offset,
        total_bytes: reply.total_bytes,
        bytes,
    };
    chunk
        .validate_for(reference, &chunk.mime, offset, chunk.total_bytes)
        .map_err(|_| "invalid_artwork_response")?;
    let end = offset
        .checked_add(chunk.bytes.len() as u64)
        .ok_or("invalid_artwork_response")?;
    if end != (offset + MAX_ARTWORK_CHUNK_BYTES as u64).min(chunk.total_bytes) {
        return Err("invalid_artwork_response");
    }
    Ok(
        json!({"ok":true,"revision":reference.revision,"mime_type":chunk.mime,"offset":offset,
        "total_bytes":chunk.total_bytes,"next_offset":(end < chunk.total_bytes).then_some(end),"data":reply.data_base64}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn reference() -> ArtworkReference {
        ArtworkReference {
            id: "cover/opaque:?7".into(),
            revision: "revision:abc".into(),
        }
    }
    #[test]
    fn routing_uses_only_selected_services_and_attested_initiators() {
        let routes = ArtworkRoutes::from_module_settings(&json!({"artwork":{"providers":{"vendor":{"service":"vendor.library/1","allowed_initiators":["manager"]}}}})).unwrap();
        let (service, payload) = routes.route("vendor", &reference(), "manager", 0).unwrap();
        assert_eq!(service, "vendor.library/1");
        assert_eq!(
            payload,
            json!({"action":"artwork","reference":reference(),"offset":0})
        );
        assert_eq!(
            routes
                .route("unselected", &reference(), "manager", 0)
                .unwrap_err(),
            "artwork_provider_not_selected"
        );
        assert_eq!(
            routes
                .route("vendor", &reference(), "outsider", 0)
                .unwrap_err(),
            "forbidden"
        );
        assert!(ArtworkRoutes::from_module_settings(&json!({"artwork":{"providers":{"vendor":{"service":"amaterasu.library/1","allowed_initiators":["manager"]}}}})).is_err());
        assert!(ArtworkRoutes::from_module_settings(&json!({"artwork":{"providers":{"vendor":{"service":"vendor.library/2","allowed_initiators":["manager"]}}}})).is_err());
        assert!(routes.route("vendor", &reference(), "manager", 1).is_err());
    }
    #[test]
    fn projection_preserves_opaque_reference_and_rejects_changed_identity_or_private_fields() {
        let payload = json!({"ok":true,"reference":reference(),"mime":"image/png","offset":0,"total_bytes":3,"data_base64":"YWJj"});
        assert_eq!(
            project_chunk(payload.clone(), &reference(), 0).unwrap(),
            json!({"ok":true,"revision":"revision:abc","mime_type":"image/png","offset":0,"total_bytes":3,"next_offset":null,"data":"YWJj"})
        );
        let mut altered = payload.clone();
        altered["reference"]["id"] = json!("other-cover");
        assert!(project_chunk(altered, &reference(), 0).is_err());
        let mut private = payload.clone();
        private["path"] = json!("/secret");
        assert!(project_chunk(private, &reference(), 0).is_err());
        let mut wrong_mime = payload.clone();
        wrong_mime["mime"] = json!("text/html");
        assert!(project_chunk(wrong_mime, &reference(), 0).is_err());
        let mut short = payload.clone();
        short["total_bytes"] = json!(8192);
        assert!(project_chunk(short, &reference(), 0).is_err());
        assert!(supports_artwork(
            json!({"ok":true,"capabilities":{"discovery":true,"artwork":true,"lifecycle":false}})
        ));
        assert!(!supports_artwork(
            json!({"ok":true,"capabilities":{"discovery":true,"artwork":false,"lifecycle":false}})
        ));
    }
}
