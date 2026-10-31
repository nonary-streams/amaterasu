//! Opt-in routing to startup-selected discovery adapters.
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

const MAX_RESPONSE_BYTES: usize = 12_000;
const MAX_PROVIDERS: usize = 32;

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Settings {
    #[serde(default)]
    providers: BTreeMap<String, Provider>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Provider {
    service: String,
    allowed_initiators: BTreeSet<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Request {
    provider: String,
    cursor: Option<String>,
    limit: usize,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ProviderError {
    code: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Failure {
    ok: bool,
    error: ProviderError,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Artwork {
    id: String,
    revision: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    id: String,
    name: String,
    artwork: Option<Artwork>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Page {
    ok: bool,
    revision: String,
    entries: Vec<Entry>,
    next_cursor: Option<String>,
}

fn bounded(value: &str) -> bool {
    !value.is_empty() && value.len() <= 128 && !value.chars().any(char::is_control)
}

impl Request {
    pub(crate) fn new(provider: String, cursor: Option<String>, limit: usize) -> Self {
        Self {
            provider,
            cursor,
            limit,
        }
    }
}

impl Settings {
    pub(crate) fn from_module_settings(settings: &Value) -> Result<Self, String> {
        let Some(value) = settings.get("discovery") else {
            return Ok(Self::default());
        };
        let config: Self = serde_json::from_value(value.clone()).map_err(|e| e.to_string())?;
        if config.providers.len() > MAX_PROVIDERS
            || config.providers.iter().any(|(key, provider)| {
                !bounded(key)
                    || !bounded(&provider.service)
                    || provider.service == super::SERVICE
                    || provider.service == super::CATALOG
                    || provider.allowed_initiators.is_empty()
                    || provider.allowed_initiators.len() > 32
                    || provider.allowed_initiators.iter().any(|id| !bounded(id))
            })
        {
            return Err("invalid discovery configuration".into());
        }
        Ok(config)
    }

    pub(crate) fn route(
        &self,
        request: Request,
        initiator: &str,
    ) -> Result<(String, Value, usize), &'static str> {
        if !bounded(&request.provider)
            || !(1..=32).contains(&request.limit)
            || request
                .cursor
                .as_ref()
                .is_some_and(|cursor| !bounded(cursor))
        {
            return Err("invalid_request");
        }
        let provider = self.providers.get(&request.provider).ok_or("forbidden")?;
        if !provider.allowed_initiators.contains(initiator) {
            return Err("forbidden");
        }
        Ok((
            provider.service.clone(),
            json!({"action":"discover", "cursor":request.cursor, "limit":request.limit}),
            request.limit,
        ))
    }
}

pub(crate) fn project(payload: Value, limit: usize) -> Value {
    let valid_size =
        serde_json::to_vec(&payload).is_ok_and(|bytes| bytes.len() <= MAX_RESPONSE_BYTES);
    let valid_failure = serde_json::from_value::<Failure>(payload.clone())
        .is_ok_and(|failure| !failure.ok && bounded(&failure.error.code));
    let has_required_optional_fields = payload.get("next_cursor").is_some()
        && payload
            .get("entries")
            .and_then(Value::as_array)
            .is_some_and(|entries| entries.iter().all(|entry| entry.get("artwork").is_some()));
    let valid_page = has_required_optional_fields
        && serde_json::from_value::<Page>(payload.clone()).is_ok_and(|page| {
            let mut ids = BTreeSet::new();
            page.ok
                && bounded(&page.revision)
                && page.entries.len() <= limit
                && page
                    .next_cursor
                    .as_ref()
                    .is_none_or(|cursor| bounded(cursor))
                && page.entries.iter().all(|entry| {
                    bounded(&entry.id)
                        && bounded(&entry.name)
                        && ids.insert(&entry.id)
                        && entry
                            .artwork
                            .as_ref()
                            .is_none_or(|art| bounded(&art.id) && bounded(&art.revision))
                })
        });
    if valid_size && (valid_page || valid_failure) {
        payload
    } else {
        super::error(
            "invalid_provider_response",
            "discovery provider returned an invalid response",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn settings() -> Settings {
        Settings::from_module_settings(&json!({"discovery":{"providers":{"games":{"service":"example.games/1","allowed_initiators":["manager"]}}}})).unwrap()
    }
    #[test]
    fn discovery_is_opt_in_and_routes_only_selected_authorized_provider() {
        let request = || {
            serde_json::from_value(json!({"provider":"games","cursor":null,"limit":20})).unwrap()
        };
        assert!(
            Settings::from_module_settings(&json!({}))
                .unwrap()
                .route(request(), "manager")
                .is_err()
        );
        assert!(settings().route(request(), "outsider").is_err());
        let (service, payload, limit) = settings().route(request(), "manager").unwrap();
        assert_eq!(service, "example.games/1");
        assert_eq!(
            payload,
            json!({"action":"discover","cursor":null,"limit":20})
        );
        assert_eq!(limit, 20);
        assert!(
            serde_json::from_value::<Request>(
                json!({"provider":"games","cursor":null,"limit":20,"service":"evil/1"})
            )
            .is_err()
        );
        assert!(
            settings()
                .route(
                    serde_json::from_value(json!({"provider":"games","cursor":null,"limit":33}))
                        .unwrap(),
                    "manager"
                )
                .is_err()
        );
        assert!(Settings::from_module_settings(&json!({"discovery":{"providers":{"games":{"service":"amaterasu.library/1","allowed_initiators":["manager"]}}}})).is_err());
    }
    #[test]
    fn provider_pages_are_strict_bounded_and_unique() {
        let page = json!({"ok":true,"revision":"rev-1","entries":[{"id":"game","name":"Game","artwork":{"id":"cover","revision":"one"}}],"next_cursor":null});
        assert_eq!(project(page.clone(), 1), page);
        let mut extra = page.clone();
        extra["entries"][0]["private"] = json!("secret");
        assert_eq!(
            project(extra, 1)["error"]["code"],
            "invalid_provider_response"
        );
        let mut duplicate = page.clone();
        duplicate["entries"]
            .as_array_mut()
            .unwrap()
            .push(page["entries"][0].clone());
        assert_eq!(
            project(duplicate, 2)["error"]["code"],
            "invalid_provider_response"
        );
        assert_eq!(
            project(page.clone(), 0)["error"]["code"],
            "invalid_provider_response"
        );
        let mut missing = page.clone();
        missing["entries"][0]
            .as_object_mut()
            .unwrap()
            .remove("artwork");
        assert_eq!(
            project(missing, 1)["error"]["code"],
            "invalid_provider_response"
        );
        let mut oversized = page;
        oversized["entries"][0]["name"] = json!("x".repeat(12_001));
        assert_eq!(
            project(oversized, 1)["error"]["code"],
            "invalid_provider_response"
        );
        let failure = json!({"ok":false,"error":{"code":"unavailable"}});
        assert_eq!(project(failure.clone(), 1), failure);
        assert_eq!(
            project(
                json!({"ok":false,"error":{"code":"unavailable","secret":"x"}}),
                1
            )["error"]["code"],
            "invalid_provider_response"
        );
    }
}
