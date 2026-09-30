//! Bounded storage and verification for hash-only predecessor API tokens.

use crate::crypto;
use crate::protocol::valid_digest;
use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const LEGACY_API_TOKEN_STORE_VERSION: u32 = 1;
pub const API_TOKEN_STORE_VERSION: u32 = 2;
pub const MAX_LEGACY_API_TOKENS: usize = 128;
pub const MAX_LEGACY_API_TOKEN_SCOPES: usize = 64;
pub const MAX_LEGACY_API_TOKEN_PATH_BYTES: usize = 2048;
pub const MAX_LEGACY_API_TOKEN_METHODS: usize = 32;
pub const MAX_LEGACY_API_TOKEN_METHOD_BYTES: usize = 32;
pub const MAX_LEGACY_API_TOKEN_STORE_BYTES: usize = 64 * 1024;

const MAX_REGEX_SIZE_BYTES: usize = 64 * 1024;

/// A separate, versioned store keeps path/method grants intact while current
/// API tokens continue to use exact named scopes and finite expiries.
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LegacyApiTokenStore {
    pub version: u32,
    pub tokens: Vec<LegacyApiToken>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LegacyApiToken {
    /// SHA-256 hex of the bearer. The bearer itself is never stored.
    pub token_id: String,
    /// 1 is the pinned record shape; 2 marks the current persisted record form.
    pub record_version: u32,
    pub owner: String,
    /// Vibeshine API tokens do not expire, so this timestamp remains metadata.
    pub created_at: i64,
    pub scopes: Vec<LegacyApiTokenScope>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LegacyApiTokenScope {
    /// Kept verbatim, including syntax the safe Rust regex engine cannot run.
    pub path: String,
    pub methods: BTreeSet<String>,
}

pub struct LegacyApiTokenVerification {
    pub token_id: String,
    pub owner: String,
    pub upgraded_store: Option<LegacyApiTokenStore>,
}

impl Default for LegacyApiTokenStore {
    fn default() -> Self {
        Self {
            version: API_TOKEN_STORE_VERSION,
            tokens: Vec::new(),
        }
    }
}

impl LegacyApiTokenStore {
    pub fn from_legacy_tokens(tokens: Vec<LegacyApiToken>) -> Self {
        Self {
            version: LEGACY_API_TOKEN_STORE_VERSION,
            tokens,
        }
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        if !matches!(
            self.version,
            LEGACY_API_TOKEN_STORE_VERSION | API_TOKEN_STORE_VERSION
        ) || self.tokens.len() > MAX_LEGACY_API_TOKENS
            || serde_json::to_vec(self)
                .map_or(true, |bytes| bytes.len() > MAX_LEGACY_API_TOKEN_STORE_BYTES)
        {
            return Err("invalid_state");
        }

        let mut token_ids = BTreeSet::new();
        for token in &self.tokens {
            let valid_record_version = match self.version {
                LEGACY_API_TOKEN_STORE_VERSION => {
                    token.record_version == LEGACY_API_TOKEN_STORE_VERSION
                }
                API_TOKEN_STORE_VERSION => matches!(
                    token.record_version,
                    LEGACY_API_TOKEN_STORE_VERSION | API_TOKEN_STORE_VERSION
                ),
                _ => false,
            };
            if !valid_record_version
                || !valid_digest(&token.token_id)
                || !token_ids.insert(&token.token_id)
                || token.owner.is_empty()
                || token.owner.len() > 256
                || token.owner.chars().any(char::is_control)
                || token.scopes.len() > MAX_LEGACY_API_TOKEN_SCOPES
                || token.scopes.iter().any(|scope| {
                    scope.path.len() > MAX_LEGACY_API_TOKEN_PATH_BYTES
                        || scope.methods.len() > MAX_LEGACY_API_TOKEN_METHODS
                        || scope.methods.iter().any(|method| {
                            method.is_empty()
                                || method.len() > MAX_LEGACY_API_TOKEN_METHOD_BYTES
                                || method.bytes().any(|byte| byte.is_ascii_control())
                        })
                })
            {
                return Err("invalid_state");
            }
        }
        Ok(())
    }

    pub fn verify_route(
        &mut self,
        bearer: &str,
        request_path: &str,
        request_method: &str,
    ) -> Result<Option<LegacyApiTokenVerification>, &'static str> {
        self.validate()?;
        if !crypto::valid_legacy_api_token(bearer) {
            return Ok(None);
        }

        let Some(index) = self
            .tokens
            .iter()
            .position(|token| crypto::verify_legacy_api_token(bearer, &token.token_id))
        else {
            return Ok(None);
        };
        let token = &self.tokens[index];
        if !scope_allows_route(&token.scopes, request_path, request_method) {
            return Ok(None);
        }

        let upgraded_store = if token.record_version == LEGACY_API_TOKEN_STORE_VERSION {
            let mut upgraded = self.clone();
            upgraded.version = API_TOKEN_STORE_VERSION;
            upgraded.tokens[index].token_id = crypto::token_digest(bearer);
            upgraded.tokens[index].record_version = API_TOKEN_STORE_VERSION;
            upgraded.validate()?;
            Some(upgraded)
        } else {
            None
        };

        Ok(Some(LegacyApiTokenVerification {
            token_id: crypto::token_digest(bearer),
            owner: token.owner.clone(),
            upgraded_store,
        }))
    }

    pub fn revoke(&mut self, token_id: &str) -> bool {
        let previous_len = self.tokens.len();
        self.tokens.retain(|token| token.token_id != token_id);
        self.tokens.len() != previous_len
    }
}

/// Matches Vibeshine's `std::regex_match` whole-input behavior for expressions
/// supported by Rust's linear-time regex dialect. This intentionally does not
/// translate the full C++ `std::regex` grammar; Unicode mode is disabled, the
/// stored expression is wrapped in whole-path anchors, and unsupported or
/// invalid expressions remain stored but cannot authorize a request.
fn scope_allows_route(
    scopes: &[LegacyApiTokenScope],
    request_path: &str,
    request_method: &str,
) -> bool {
    let mut scopes_by_path = BTreeMap::new();
    for scope in scopes {
        scopes_by_path.entry(scope.path.as_str()).or_insert(scope);
    }

    for (path_pattern, scope) in scopes_by_path {
        let Some(regex) = compile_legacy_path_regex(path_pattern) else {
            return false;
        };
        if regex.is_match(request_path)
            && scope
                .methods
                .iter()
                .any(|method| method.eq_ignore_ascii_case(request_method))
        {
            return true;
        }
    }
    false
}

fn compile_legacy_path_regex(path_pattern: &str) -> Option<Regex> {
    if path_pattern.is_empty() {
        return None;
    }
    let mut anchored_pattern = String::with_capacity(path_pattern.len() + 10);
    if !path_pattern.starts_with('^') {
        anchored_pattern.push('^');
    }
    anchored_pattern.push_str("(?:");
    anchored_pattern.push_str(path_pattern);
    anchored_pattern.push(')');
    if !path_pattern.ends_with('$') {
        anchored_pattern.push('$');
    }
    // `$` can also match immediately before a final newline. `\z` requires
    // the exact end of the request path, as `std::regex_match` does.
    anchored_pattern.push_str(r"\z");

    let mut builder = RegexBuilder::new(&anchored_pattern);
    builder.unicode(false).size_limit(MAX_REGEX_SIZE_BYTES);
    builder.build().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const LEGACY_BEARER: &str = "0123456789abcdefghijklmnopqrstuv";

    fn token(scopes: Vec<LegacyApiTokenScope>) -> LegacyApiToken {
        LegacyApiToken {
            token_id: crypto::token_digest(LEGACY_BEARER),
            record_version: LEGACY_API_TOKEN_STORE_VERSION,
            owner: "legacy-admin".into(),
            created_at: 1_700_000_000,
            scopes,
        }
    }

    fn scope(path: &str, methods: &[&str]) -> LegacyApiTokenScope {
        LegacyApiTokenScope {
            path: path.into(),
            methods: methods.iter().map(|method| (*method).into()).collect(),
        }
    }

    #[test]
    fn legacy_scope_matching_anchors_regex_and_ignores_method_case() {
        let mut store = LegacyApiTokenStore::from_legacy_tokens(vec![token(vec![scope(
            "/api/apps/[0-9]+",
            &["GET"],
        )])]);

        let accepted = store
            .verify_route(LEGACY_BEARER, "/api/apps/42", "get")
            .unwrap();
        assert!(accepted.is_some());
        assert!(
            store
                .verify_route(LEGACY_BEARER, "/api/apps/42/extra", "GET")
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .verify_route(LEGACY_BEARER, "/api/apps/42\n", "GET")
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .verify_route(LEGACY_BEARER, "/api/apps/42", "POST")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn duplicate_legacy_paths_keep_the_first_loaded_method_set() {
        let mut store = LegacyApiTokenStore::from_legacy_tokens(vec![token(vec![
            scope("/api/apps", &["GET"]),
            scope("/api/apps", &["POST"]),
        ])]);

        assert!(
            store
                .verify_route(LEGACY_BEARER, "/api/apps", "GET")
                .unwrap()
                .is_some()
        );
        assert!(
            store
                .verify_route(LEGACY_BEARER, "/api/apps", "POST")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn unsupported_regex_syntax_stays_stored_and_denies_without_panicking() {
        let mut store = LegacyApiTokenStore::from_legacy_tokens(vec![token(vec![scope(
            r"/api/(apps)\1",
            &["GET"],
        )])]);

        assert!(store.validate().is_ok());
        assert!(
            store
                .verify_route(LEGACY_BEARER, "/api/appsapps", "GET")
                .unwrap()
                .is_none()
        );
        assert_eq!(store.version, LEGACY_API_TOKEN_STORE_VERSION);
        assert_eq!(
            store.tokens[0].record_version,
            LEGACY_API_TOKEN_STORE_VERSION
        );
    }

    #[test]
    fn malformed_regex_is_retained_but_never_authorizes() {
        let mut store = LegacyApiTokenStore::from_legacy_tokens(vec![token(vec![scope(
            "/api/apps/[",
            &["GET"],
        )])]);

        assert!(store.validate().is_ok());
        assert!(
            store
                .verify_route(LEGACY_BEARER, "/api/apps/", "GET")
                .unwrap()
                .is_none()
        );
        assert_eq!(store.tokens.len(), 1);
    }

    #[test]
    fn only_a_successful_legacy_route_proof_upgrades_the_versioned_digest_record() {
        let mut store = LegacyApiTokenStore::from_legacy_tokens(vec![token(vec![scope(
            "/api/apps",
            &["GET"],
        )])]);

        assert!(
            store
                .verify_route(LEGACY_BEARER, "/api/other", "GET")
                .unwrap()
                .is_none()
        );
        assert_eq!(
            store.tokens[0].record_version,
            LEGACY_API_TOKEN_STORE_VERSION
        );

        let accepted = store
            .verify_route(LEGACY_BEARER, "/api/apps", "GET")
            .unwrap();
        assert!(accepted.as_ref().unwrap().upgraded_store.is_some());
        let upgraded = accepted.unwrap().upgraded_store.unwrap();
        assert_eq!(upgraded.version, API_TOKEN_STORE_VERSION);
        assert_eq!(upgraded.tokens[0].record_version, API_TOKEN_STORE_VERSION);
        assert!(upgraded.validate().is_ok());
    }
}
