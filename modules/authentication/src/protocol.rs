use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeSet;

pub const MAX_TOKENS: usize = 32;
pub const MAX_SCOPES: usize = 64;
pub const DEFAULT_SESSION_TOKEN_TTL_SECONDS: u64 = 7200;
pub const DEFAULT_REMEMBER_ME_REFRESH_TOKEN_TTL_SECONDS: u64 = 604800;
pub const DEFAULT_REFRESH_TOKEN_TTL_SECONDS: u64 = 86400;
pub const MAX_CONFIGURED_TTL_SECONDS: u64 = i32::MAX as u64;
pub const MAX_TOKEN_SECONDS: u64 = 30 * 24 * 60 * 60;

#[derive(Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub administrator_principals: BTreeSet<String>,
    pub allowed_scopes: BTreeSet<String>,
    pub session_token_ttl_seconds: u64,
    pub remember_me_refresh_token_ttl_seconds: u64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            administrator_principals: BTreeSet::new(),
            allowed_scopes: BTreeSet::new(),
            session_token_ttl_seconds: DEFAULT_SESSION_TOKEN_TTL_SECONDS,
            remember_me_refresh_token_ttl_seconds: DEFAULT_REMEMBER_ME_REFRESH_TOKEN_TTL_SECONDS,
        }
    }
}

impl Settings {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.administrator_principals.len() > 16
            || self
                .administrator_principals
                .iter()
                .any(|value| !identifier(value, 128))
            || self.allowed_scopes.len() > MAX_SCOPES
            || self
                .allowed_scopes
                .iter()
                .any(|value| !identifier(value, 64))
            || !(1..=MAX_CONFIGURED_TTL_SECONDS).contains(&self.session_token_ttl_seconds)
            || !(1..=MAX_CONFIGURED_TTL_SECONDS)
                .contains(&self.remember_me_refresh_token_ttl_seconds)
        {
            return Err("invalid authentication settings");
        }
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Operation {
    /// A non-secret readiness result for the trusted management principal.
    /// It deliberately reveals neither usernames nor credential material.
    Status {},
    Setup {
        username: String,
        password: String,
    },
    ChangePassword {
        password: String,
    },
    ListSessions {
        #[serde(default)]
        offset: usize,
        #[serde(default = "session_page_limit")]
        limit: usize,
    },
    SettingsGet {},
    SettingsUpdate {
        session_token_ttl_seconds: u64,
        remember_me_refresh_token_ttl_seconds: u64,
        expected_revision: u64,
    },
    RevokeSession {
        session_id: String,
    },
    Login {
        username: String,
        password: String,
        #[serde(default)]
        remember_me: bool,
    },
    Refresh {
        refresh_token: String,
    },
    Verify {
        bearer: String,
        required_scopes: BTreeSet<String>,
    },
    Logout {
        #[serde(default)]
        bearer: Option<String>,
        #[serde(default)]
        refresh_token: Option<String>,
    },
    IssueToken {
        scopes: BTreeSet<String>,
        ttl_seconds: u64,
    },
    RevokeToken {
        token_id: String,
    },
    ListTokens {},
}

impl Operation {
    pub fn validate(&self) -> Result<(), &'static str> {
        let valid = match self {
            Self::Status {} => true,
            Self::SettingsGet {} => true,
            Self::SettingsUpdate {
                session_token_ttl_seconds,
                remember_me_refresh_token_ttl_seconds,
                ..
            } => {
                (1..=MAX_CONFIGURED_TTL_SECONDS).contains(session_token_ttl_seconds)
                    && (1..=MAX_CONFIGURED_TTL_SECONDS)
                        .contains(remember_me_refresh_token_ttl_seconds)
            }
            Self::Setup { username, password } => {
                identifier(username, 64) && (12..=256).contains(&password.len())
            }
            Self::ChangePassword { password } => (12..=256).contains(&password.len()),
            Self::ListSessions { limit, .. } => (1..=4).contains(limit),
            Self::RevokeSession { session_id } => valid_digest(session_id),
            Self::Login {
                username, password, ..
            } => identifier(username, 64) && (1..=256).contains(&password.len()),
            Self::Refresh { refresh_token } => valid_bearer(refresh_token),
            Self::Verify {
                bearer,
                required_scopes,
            } => valid_bearer(bearer) && valid_scopes(required_scopes),
            Self::Logout {
                bearer,
                refresh_token,
            } => match (bearer.as_deref(), refresh_token.as_deref()) {
                (Some(token), None) | (None, Some(token)) => valid_bearer(token),
                _ => false,
            },
            Self::IssueToken {
                scopes,
                ttl_seconds,
            } => {
                !scopes.is_empty()
                    && valid_scopes(scopes)
                    && (1..=MAX_TOKEN_SECONDS).contains(ttl_seconds)
            }
            Self::RevokeToken { token_id } => valid_digest(token_id),
            Self::ListTokens {} => true,
        };
        if valid {
            Ok(())
        } else {
            Err("invalid_request")
        }
    }

    pub fn requires_administrator(&self) -> bool {
        matches!(
            self,
            Self::Setup { .. }
                | Self::ChangePassword { .. }
                | Self::SettingsGet { .. }
                | Self::SettingsUpdate { .. }
                | Self::ListSessions { .. }
                | Self::RevokeSession { .. }
                | Self::IssueToken { .. }
                | Self::RevokeToken { .. }
                | Self::ListTokens {}
        )
    }
}

#[derive(Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Document {
    pub credential: Option<Credential>,
    pub tokens: Vec<ApiToken>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Credential {
    pub username: String,
    pub password_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub legacy_salt: Option<String>,
    #[serde(default)]
    pub case_insensitive_username: bool,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ApiToken {
    pub token_id: String,
    pub owner: String,
    pub scopes: BTreeSet<String>,
    pub issued_at: u64,
    pub expires_at: u64,
}

impl Document {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.tokens.len() > MAX_TOKENS
            || serde_json::to_vec(self).map_or(true, |bytes| bytes.len() > 12_000)
        {
            return Err("invalid_state");
        }
        if let Some(credential) = &self.credential {
            if !identifier(&credential.username, 64)
                || !match &credential.legacy_salt {
                    Some(salt) => {
                        crate::crypto::valid_legacy_salt(salt)
                            && crate::crypto::valid_legacy_password_hash(&credential.password_hash)
                    }
                    None => crate::crypto::valid_password_hash(&credential.password_hash),
                }
            {
                return Err("invalid_state");
            }
        } else if !self.tokens.is_empty() {
            return Err("invalid_state");
        }
        let mut ids = BTreeSet::new();
        for token in &self.tokens {
            if !valid_digest(&token.token_id)
                || !ids.insert(&token.token_id)
                || !identifier(&token.owner, 128)
                || token.scopes.is_empty()
                || !valid_scopes(&token.scopes)
                || token.expires_at <= token.issued_at
                || token.expires_at - token.issued_at > MAX_TOKEN_SECONDS
            {
                return Err("invalid_state");
            }
        }
        Ok(())
    }
}

fn identifier(value: &str, max: usize) -> bool {
    !value.is_empty()
        && value.len() <= max
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-:/".contains(&byte))
}
fn valid_scopes(scopes: &BTreeSet<String>) -> bool {
    scopes.len() <= MAX_SCOPES && scopes.iter().all(|scope| identifier(scope, 64))
}
pub fn valid_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
fn valid_bearer(value: &str) -> bool {
    value.strip_prefix("ama_").is_some_and(valid_digest)
}
pub fn error(code: &str) -> Value {
    json!({"ok":false,"code":code})
}

fn session_page_limit() -> usize {
    4
}

#[cfg(test)]
mod scope_tests {
    use super::*;

    #[test]
    fn authentication_ttl_defaults_and_bounds_are_explicit() {
        let defaults: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(defaults.session_token_ttl_seconds, 7200);
        assert_eq!(defaults.remember_me_refresh_token_ttl_seconds, 604800);
        defaults.validate().unwrap();

        let custom: Settings = serde_json::from_value(json!({
            "session_token_ttl_seconds": 30,
            "remember_me_refresh_token_ttl_seconds": 60
        }))
        .unwrap();
        custom.validate().unwrap();

        for invalid in [
            json!({"session_token_ttl_seconds": 0}),
            json!({"session_token_ttl_seconds": i32::MAX as u64 + 1}),
            json!({"remember_me_refresh_token_ttl_seconds": 0}),
            json!({"remember_me_refresh_token_ttl_seconds": i32::MAX as u64 + 1}),
        ] {
            let mut settings = defaults.clone();
            if let Some(value) = invalid.get("session_token_ttl_seconds") {
                settings.session_token_ttl_seconds = value.as_u64().unwrap();
            }
            if let Some(value) = invalid.get("remember_me_refresh_token_ttl_seconds") {
                settings.remember_me_refresh_token_ttl_seconds = value.as_u64().unwrap();
            }
            assert!(settings.validate().is_err());
        }
    }

    #[test]
    fn editable_ttl_actions_require_admin_and_keep_strict_bounds() {
        let get: Operation = serde_json::from_value(json!({"action":"settings_get"})).unwrap();
        assert!(get.validate().is_ok());
        assert!(get.requires_administrator());

        let update: Operation = serde_json::from_value(json!({
            "action":"settings_update",
            "session_token_ttl_seconds":3600,
            "remember_me_refresh_token_ttl_seconds":259200,
            "expected_revision":0
        }))
        .unwrap();
        assert!(update.validate().is_ok());
        assert!(update.requires_administrator());

        for payload in [
            json!({"action":"settings_update","session_token_ttl_seconds":0,"remember_me_refresh_token_ttl_seconds":60,"expected_revision":0}),
            json!({"action":"settings_update","session_token_ttl_seconds":60,"remember_me_refresh_token_ttl_seconds":i32::MAX as u64 + 1,"expected_revision":0}),
            json!({"action":"settings_update","session_token_ttl_seconds":60,"remember_me_refresh_token_ttl_seconds":60}),
            json!({"action":"settings_update","session_token_ttl_seconds":60,"remember_me_refresh_token_ttl_seconds":60,"expected_revision":0,"owner":"amaterasu.other"}),
        ] {
            let operation = serde_json::from_value::<Operation>(payload);
            assert!(operation.is_err() || operation.unwrap().validate().is_err());
        }
    }

    #[test]
    fn login_and_logout_payloads_remain_backward_compatible_and_unambiguous() {
        let old_login: Operation = serde_json::from_value(json!({
            "action": "login",
            "username": "admin",
            "password": "correct horse battery staple"
        }))
        .unwrap();
        assert!(old_login.validate().is_ok());

        let token = format!("ama_{}", "a".repeat(64));
        for payload in [
            json!({"action":"logout","bearer":token.clone()}),
            json!({"action":"logout","refresh_token":token.clone()}),
        ] {
            let logout: Operation = serde_json::from_value(payload).unwrap();
            assert!(logout.validate().is_ok());
        }
        for payload in [
            json!({"action":"logout"}),
            json!({"action":"logout","bearer":token.clone(),"refresh_token":token}),
        ] {
            let logout: Operation = serde_json::from_value(payload).unwrap();
            assert!(logout.validate().is_err());
        }
    }

    #[test]
    fn scope_capacity_supports_new_domains_but_remains_bounded() {
        let scopes: BTreeSet<_> = (0..MAX_SCOPES)
            .map(|index| format!("domain{index}.read"))
            .collect();
        let mut settings = Settings {
            allowed_scopes: scopes.clone(),
            ..Settings::default()
        };
        assert!(settings.validate().is_ok());
        assert!(valid_scopes(&scopes));
        settings.allowed_scopes.insert("overflow.read".into());
        assert!(settings.validate().is_err());
        assert!(!valid_scopes(&settings.allowed_scopes));
        assert!(!valid_scopes(&BTreeSet::from(["native *".into()])));
    }
}
