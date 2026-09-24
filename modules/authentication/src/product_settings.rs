//! Durable administrator-editable token lifetimes.
use crate::protocol::{MAX_CONFIGURED_TTL_SECONDS, Settings};
use serde_json::{Value, json};

pub const PRODUCT_SETTINGS_VERSION: u32 = 1;
pub const CONFIGURATION_SERVICE: &str = "amaterasu.configuration/1";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TokenLifetimes {
    pub session_token_ttl_seconds: u64,
    pub remember_me_refresh_token_ttl_seconds: u64,
}

impl TokenLifetimes {
    pub fn from_bootstrap(settings: &Settings) -> Self {
        Self {
            session_token_ttl_seconds: settings.session_token_ttl_seconds,
            remember_me_refresh_token_ttl_seconds: settings.remember_me_refresh_token_ttl_seconds,
        }
    }

    pub fn new(
        session_token_ttl_seconds: u64,
        remember_me_refresh_token_ttl_seconds: u64,
    ) -> Option<Self> {
        let lifetimes = Self {
            session_token_ttl_seconds,
            remember_me_refresh_token_ttl_seconds,
        };
        lifetimes.validate().then_some(lifetimes)
    }

    pub fn validate(self) -> bool {
        (1..=MAX_CONFIGURED_TTL_SECONDS).contains(&self.session_token_ttl_seconds)
            && (1..=MAX_CONFIGURED_TTL_SECONDS)
                .contains(&self.remember_me_refresh_token_ttl_seconds)
    }

    pub fn product_values(self) -> Value {
        json!({
            "session_token_ttl_seconds": self.session_token_ttl_seconds,
            "remember_me_refresh_token_ttl_seconds": self.remember_me_refresh_token_ttl_seconds,
        })
    }

    pub fn settings_response(self, revision: u64) -> Value {
        json!({"ok":true,"revision":revision,"values":self.product_values()})
    }

    pub fn update_payload(self, expected_revision: u64) -> Value {
        json!({
            "action": "product_update",
            "expected_revision": expected_revision,
            "version": PRODUCT_SETTINGS_VERSION,
            "values": self.product_values(),
        })
    }

    /// Reads the product config contract. Missing known fields inherit the
    /// packaged bootstrap values so v1 documents written before a field was
    /// introduced retain their configured behavior.
    pub fn from_product_response(
        payload: &Value,
        fallback: Self,
    ) -> Result<(Self, u64), &'static str> {
        if !has_exact_keys(payload, &["ok", "found", "version", "revision", "values"])
            || payload["ok"] != true
        {
            return Err("invalid configuration response");
        }
        let revision = payload["revision"]
            .as_u64()
            .ok_or("configuration revision is missing")?;
        let values = payload["values"]
            .as_object()
            .ok_or("configuration values are malformed")?;
        match payload["found"].as_bool() {
            Some(false) => {
                if revision != 0 || !payload["version"].is_null() || !values.is_empty() {
                    return Err("absent configuration document is malformed");
                }
                Ok((fallback, 0))
            }
            Some(true) => {
                if revision == 0
                    || payload["version"].as_u64() != Some(u64::from(PRODUCT_SETTINGS_VERSION))
                    || values.keys().any(|key| {
                        !matches!(
                            key.as_str(),
                            "session_token_ttl_seconds" | "remember_me_refresh_token_ttl_seconds"
                        )
                    })
                {
                    return Err("unsupported or malformed authentication settings document");
                }
                let session_ttl = optional_ttl(
                    values.get("session_token_ttl_seconds"),
                    fallback.session_token_ttl_seconds,
                )?;
                let refresh_ttl = optional_ttl(
                    values.get("remember_me_refresh_token_ttl_seconds"),
                    fallback.remember_me_refresh_token_ttl_seconds,
                )?;
                let lifetimes = Self::new(session_ttl, refresh_ttl)
                    .ok_or("authentication token lifetime is out of range")?;
                Ok((lifetimes, revision))
            }
            None => Err("configuration response omitted found"),
        }
    }

    /// Confirms an update response describes the committed full replacement.
    pub fn from_update_response(
        payload: &Value,
        expected_revision: u64,
        expected: Self,
    ) -> Result<u64, &'static str> {
        let expected_next_revision = expected_revision
            .checked_add(1)
            .ok_or("authentication settings revision is exhausted")?;
        let (committed, revision) = Self::from_product_response(
            payload,
            // Updates always submit both fields. The fallback is only used
            // for legacy reads; the equality check below rejects omissions.
            expected,
        )?;
        if revision != expected_next_revision
            || payload["found"] != true
            || payload["version"].as_u64() != Some(u64::from(PRODUCT_SETTINGS_VERSION))
            || payload["values"]["session_token_ttl_seconds"].as_u64()
                != Some(expected.session_token_ttl_seconds)
            || payload["values"]["remember_me_refresh_token_ttl_seconds"].as_u64()
                != Some(expected.remember_me_refresh_token_ttl_seconds)
            || committed != expected
        {
            return Err("configuration update response does not match the committed settings");
        }
        Ok(revision)
    }
}

fn optional_ttl(value: Option<&Value>, fallback: u64) -> Result<u64, &'static str> {
    match value {
        Some(value) => value
            .as_u64()
            .ok_or("authentication token lifetime is malformed"),
        None => Ok(fallback),
    }
}

fn has_exact_keys(value: &Value, keys: &[&str]) -> bool {
    value.as_object().is_some_and(|object| {
        object.len() == keys.len() && object.keys().all(|key| keys.contains(&key.as_str()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fallback() -> TokenLifetimes {
        TokenLifetimes {
            session_token_ttl_seconds: 7_200,
            remember_me_refresh_token_ttl_seconds: 604_800,
        }
    }

    #[test]
    fn absent_and_older_partial_documents_preserve_bootstrap_defaults() {
        let fallback = fallback();
        let absent = json!({
            "ok":true,"found":false,"version":null,"revision":0,"values":{}
        });
        assert_eq!(
            TokenLifetimes::from_product_response(&absent, fallback),
            Ok((fallback, 0))
        );

        let older = json!({
            "ok":true,"found":true,"version":1,"revision":3,
            "values":{"session_token_ttl_seconds":3600}
        });
        assert_eq!(
            TokenLifetimes::from_product_response(&older, fallback),
            Ok((
                TokenLifetimes {
                    session_token_ttl_seconds: 3600,
                    ..fallback
                },
                3
            ))
        );
    }

    #[test]
    fn product_document_rejects_unknown_fields_versions_and_bad_values() {
        let fallback = fallback();
        for malformed in [
            json!({"ok":true,"found":true,"version":2,"revision":1,"values":{}}),
            json!({"ok":true,"found":true,"version":1,"revision":1,"values":{"other":true}}),
            json!({"ok":true,"found":true,"version":1,"revision":1,"values":{"session_token_ttl_seconds":0}}),
            json!({"ok":true,"found":true,"version":1,"revision":1,"values":{"remember_me_refresh_token_ttl_seconds":i32::MAX as u64 + 1}}),
            json!({"ok":true,"found":false,"version":null,"revision":1,"values":{}}),
        ] {
            assert!(TokenLifetimes::from_product_response(&malformed, fallback).is_err());
        }
    }

    #[test]
    fn update_requires_the_full_expected_committed_document() {
        let expected = TokenLifetimes::new(3_600, 259_200).unwrap();
        let payload = json!({
            "ok":true,"found":true,"version":1,"revision":5,
            "values":expected.product_values()
        });
        assert_eq!(
            TokenLifetimes::from_update_response(&payload, 4, expected),
            Ok(5)
        );
        assert!(TokenLifetimes::from_update_response(&payload, 3, expected).is_err());

        let partial = json!({
            "ok":true,"found":true,"version":1,"revision":5,
            "values":{"session_token_ttl_seconds":3600}
        });
        assert!(TokenLifetimes::from_update_response(&partial, 4, expected).is_err());
    }

    #[test]
    fn non_remembered_refresh_lifetime_remains_fixed() {
        assert_eq!(crate::protocol::DEFAULT_REFRESH_TOKEN_TTL_SECONDS, 86_400);
    }
}
