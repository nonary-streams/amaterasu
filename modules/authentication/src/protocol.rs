use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeSet;

pub const MAX_TOKENS: usize = 32;
pub const MAX_SCOPES: usize = 64;
pub const SESSION_SECONDS: u64 = 900;
pub const MAX_TOKEN_SECONDS: u64 = 30 * 24 * 60 * 60;

#[derive(Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub administrator_principals: BTreeSet<String>,
    pub allowed_scopes: BTreeSet<String>,
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
    RevokeSession {
        session_id: String,
    },
    Login {
        username: String,
        password: String,
    },
    Verify {
        bearer: String,
        required_scopes: BTreeSet<String>,
    },
    Logout {
        bearer: String,
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
            Self::Setup { username, password } => {
                identifier(username, 64) && (12..=256).contains(&password.len())
            }
            Self::ChangePassword { password } => (12..=256).contains(&password.len()),
            Self::ListSessions { limit, .. } => (1..=4).contains(limit),
            Self::RevokeSession { session_id } => valid_digest(session_id),
            Self::Login { username, password } => {
                identifier(username, 64) && (1..=256).contains(&password.len())
            }
            Self::Verify {
                bearer,
                required_scopes,
            } => valid_bearer(bearer) && valid_scopes(required_scopes),
            Self::Logout { bearer } => valid_bearer(bearer),
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
