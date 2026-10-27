//! Strict, one-way migration of a legacy web-login verifier.

use crate::legacy_tokens::{
    LegacyApiToken, LegacyApiTokenScope, LegacyApiTokenStore, MAX_LEGACY_API_TOKEN_METHODS,
    MAX_LEGACY_API_TOKEN_SCOPES, MAX_LEGACY_API_TOKENS,
};
use crate::protocol::{Credential, Document, SessionDocument, valid_digest};
use amaterasu_storage::{RootLock, create_private_directory, read_bounded, write_json_atomically};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

const STATE_FILES: &[&str] = &[
    "apollo_state.json",
    "sunshine_state.json",
    "vibeshine_state.json",
    "vibepollo_state.json",
];
const PRIMARY_STATE_FILES: &[&str] = &["apollo_state.json", "sunshine_state.json"];
const PRINCIPAL: &str = "amaterasu.authentication";
const STATE_KEY: &str = "authentication/v1";
const SESSION_STATE_KEY: &str = "authentication/sessions/v1";
const LEGACY_API_TOKEN_STATE_KEY: &str = "authentication/api-tokens/v1";

/// A validated predecessor verifier. It intentionally has no `Debug` impl so
/// callers cannot accidentally log the retained password verifier.
pub struct LegacyAuthenticationImport {
    document: Document,
    legacy_api_tokens: LegacyApiTokenStore,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthenticationSeedOutcome {
    Seeded,
    ExistingAmaterasuState,
}

pub fn import_legacy_authentication(
    source_root: &Path,
) -> Result<LegacyAuthenticationImport, &'static str> {
    let root = canonical_directory(source_root)?;
    let candidates = credential_candidates(&root)?;
    for path in token_validation_candidates(&root)? {
        if !candidates.contains(&path) {
            reject_unsupported_api_tokens_in_file(&path)?;
        }
    }
    let mut documents = Vec::new();
    let mut legacy_api_tokens = None;
    for path in candidates {
        let bytes = read_bounded(&path, 1024 * 1024).map_err(|_| "legacy_authentication_layout")?;
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|_| "legacy_authentication_invalid_credential")?;
        if !["username", "password", "salt"]
            .iter()
            .any(|key| value.get(key).is_some())
        {
            reject_unpaired_api_tokens(&value)?;
            continue;
        }
        let legacy: LegacyCredential = serde_json::from_value(value)
            .map_err(|_| "legacy_authentication_invalid_credential")?;
        let tokens = parse_legacy_api_tokens(&legacy)?;
        if legacy_api_tokens.replace(tokens).is_some() {
            return Err("legacy_authentication_credential_ambiguous");
        }
        let document = Document {
            credential: Some(Credential {
                username: legacy.username,
                password_hash: legacy.password,
                legacy_salt: Some(legacy.salt),
                case_insensitive_username: true,
            }),
            tokens: Vec::new(),
            accounts: Vec::new(),
            enrollment: None,
        };
        document
            .validate()
            .map_err(|_| "legacy_authentication_invalid_credential")?;
        documents.push(document);
    }
    if documents.len() != 1 {
        return Err("legacy_authentication_credential_ambiguous");
    }
    Ok(LegacyAuthenticationImport {
        document: documents.pop().expect("one document"),
        legacy_api_tokens: legacy_api_tokens.unwrap_or_default(),
    })
}

pub fn seed_authentication_state(
    state_root: &Path,
    import: &LegacyAuthenticationImport,
) -> Result<AuthenticationSeedOutcome, &'static str> {
    import
        .document
        .validate()
        .map_err(|_| "legacy_authentication_invalid_credential")?;
    import
        .legacy_api_tokens
        .validate()
        .map_err(|_| "legacy_authentication_invalid_credential")?;
    create_private_directory(state_root).map_err(|_| "legacy_authentication_state_unavailable")?;
    let _lock = RootLock::acquire(state_root).map_err(|_| "legacy_authentication_state_busy")?;
    let path = namespace_path(state_root);
    let mut namespace = read_namespace(&path)?;
    validate_existing_session_sidecar(&namespace)?;
    validate_existing_legacy_api_token_sidecar(&namespace)?;
    if let Some(value) = namespace.values.get(STATE_KEY) {
        validate_existing_authentication(value, namespace.revision)?;
        return Ok(AuthenticationSeedOutcome::ExistingAmaterasuState);
    }
    if namespace.values.contains_key(LEGACY_API_TOKEN_STATE_KEY) {
        return Err("legacy_authentication_existing_state_invalid");
    }
    namespace.revision = namespace
        .revision
        .checked_add(1)
        .ok_or("legacy_authentication_state_unavailable")?;
    namespace.values.insert(
        STATE_KEY.into(),
        serde_json::to_value(&import.document)
            .map_err(|_| "legacy_authentication_state_unavailable")?,
    );
    namespace.values.insert(
        LEGACY_API_TOKEN_STATE_KEY.into(),
        serde_json::to_value(&import.legacy_api_tokens)
            .map_err(|_| "legacy_authentication_state_unavailable")?,
    );
    let directory = path
        .parent()
        .ok_or("legacy_authentication_state_unavailable")?;
    create_private_directory(directory).map_err(|_| "legacy_authentication_state_unavailable")?;
    write_json_atomically(&path, &namespace)
        .map_err(|_| "legacy_authentication_state_unavailable")?;
    Ok(AuthenticationSeedOutcome::Seeded)
}

fn validate_existing_authentication(value: &Value, revision: u64) -> Result<(), &'static str> {
    if revision == 0 {
        return Err("legacy_authentication_existing_state_invalid");
    }
    let document: Document = serde_json::from_value(value.clone())
        .map_err(|_| "legacy_authentication_existing_state_invalid")?;
    document
        .validate()
        .map_err(|_| "legacy_authentication_existing_state_invalid")?;
    if document.credential.is_none() && document.accounts.is_empty() {
        return Err("legacy_authentication_existing_state_invalid");
    }
    Ok(())
}

fn validate_existing_session_sidecar(namespace: &Namespace) -> Result<(), &'static str> {
    let Some(value) = namespace.values.get(SESSION_STATE_KEY) else {
        return Ok(());
    };
    if namespace.revision == 0 {
        return Err("legacy_authentication_existing_state_invalid");
    }
    let document: SessionDocument = serde_json::from_value(value.clone())
        .map_err(|_| "legacy_authentication_existing_state_invalid")?;
    document
        .validate()
        .map_err(|_| "legacy_authentication_existing_state_invalid")
}

fn validate_existing_legacy_api_token_sidecar(namespace: &Namespace) -> Result<(), &'static str> {
    let Some(value) = namespace.values.get(LEGACY_API_TOKEN_STATE_KEY) else {
        return Ok(());
    };
    if namespace.revision == 0 {
        return Err("legacy_authentication_existing_state_invalid");
    }
    let document: LegacyApiTokenStore = serde_json::from_value(value.clone())
        .map_err(|_| "legacy_authentication_existing_state_invalid")?;
    document
        .validate()
        .map_err(|_| "legacy_authentication_existing_state_invalid")
}

#[derive(Deserialize)]
struct LegacyCredential {
    username: String,
    password: String,
    salt: String,
    #[serde(default)]
    root: Option<LegacyRoot>,
}

#[derive(Deserialize)]
struct LegacyRoot {
    #[serde(default)]
    api_tokens: Vec<Value>,
}

impl LegacyCredential {
    fn api_tokens(&self) -> &[Value] {
        self.root
            .as_ref()
            .map_or(&[], |root| root.api_tokens.as_slice())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyApiTokenRecord {
    hash: String,
    username: String,
    created_at: i64,
    scopes: Vec<LegacyApiTokenScopeRecord>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyApiTokenScopeRecord {
    path: String,
    methods: Vec<String>,
}

fn parse_legacy_api_tokens(
    credential: &LegacyCredential,
) -> Result<LegacyApiTokenStore, &'static str> {
    let values = credential.api_tokens();
    if values.len() > MAX_LEGACY_API_TOKENS {
        return Err("legacy_authentication_tokens_unsupported");
    }
    let mut tokens = Vec::with_capacity(values.len());
    for value in values {
        let token: LegacyApiTokenRecord = serde_json::from_value(value.clone())
            .map_err(|_| "legacy_authentication_tokens_unsupported")?;
        if !valid_digest(&token.hash)
            || token.scopes.len() > MAX_LEGACY_API_TOKEN_SCOPES
            || token
                .scopes
                .iter()
                .any(|scope| scope.methods.len() > MAX_LEGACY_API_TOKEN_METHODS)
        {
            return Err("legacy_authentication_tokens_unsupported");
        }
        tokens.push(LegacyApiToken {
            token_id: token.hash,
            record_version: 1,
            owner: token.username,
            created_at: token.created_at,
            scopes: token
                .scopes
                .into_iter()
                .map(|scope| LegacyApiTokenScope {
                    path: scope.path,
                    methods: scope.methods.into_iter().collect(),
                })
                .collect(),
        });
    }
    let store = LegacyApiTokenStore::from_legacy_tokens(tokens);
    store
        .validate()
        .map_err(|_| "legacy_authentication_tokens_unsupported")?;
    Ok(store)
}

fn credential_candidates(root: &Path) -> Result<Vec<PathBuf>, &'static str> {
    if let Some(path) = configured_credential_file(root)? {
        return Ok(vec![path]);
    }
    let configured_state = configured_state_paths(root)?;
    match &configured_state.primary_file_state {
        ConfiguredPrimaryState::Configured(path) => Ok(vec![path.clone()]),
        ConfiguredPrimaryState::Missing => Ok(Vec::new()),
        ConfiguredPrimaryState::NotConfigured => default_primary_state_candidates(root),
    }
}

fn token_validation_candidates(root: &Path) -> Result<Vec<PathBuf>, &'static str> {
    let configured_state = configured_state_paths(root)?;
    legacy_state_candidates(root, &configured_state)
}

fn default_primary_state_candidates(root: &Path) -> Result<Vec<PathBuf>, &'static str> {
    let mut files = Vec::new();
    for name in PRIMARY_STATE_FILES {
        if root
            .join(name)
            .try_exists()
            .map_err(|_| "legacy_authentication_layout")?
        {
            files.push(regular_contained_file(root, Path::new(name))?);
        }
    }
    Ok(files)
}

fn legacy_state_candidates(
    root: &Path,
    configured_state: &ConfiguredStatePaths,
) -> Result<Vec<PathBuf>, &'static str> {
    let mut files = Vec::new();
    for name in STATE_FILES {
        if root
            .join(name)
            .try_exists()
            .map_err(|_| "legacy_authentication_layout")?
        {
            let path = regular_contained_file(root, Path::new(name))?;
            if !files.contains(&path) {
                files.push(path);
            }
        }
    }
    if let ConfiguredPrimaryState::Configured(path) = &configured_state.primary_file_state
        && !files.contains(path)
    {
        files.push(path.clone());
    }
    for path in &configured_state.auxiliary_paths {
        if !files.contains(path) {
            files.push(path.clone());
        }
    }
    Ok(files)
}

fn reject_unsupported_api_tokens(value: &Value) -> Result<(), &'static str> {
    if value
        .pointer("/root/api_tokens")
        .and_then(Value::as_array)
        .is_some_and(|tokens| !tokens.is_empty())
    {
        return Err("legacy_authentication_tokens_unsupported");
    }
    Ok(())
}

fn reject_unpaired_api_tokens(value: &Value) -> Result<(), &'static str> {
    match value.pointer("/root/api_tokens") {
        None => Ok(()),
        Some(Value::Array(tokens)) if tokens.is_empty() => Ok(()),
        Some(_) => Err("legacy_authentication_tokens_unsupported"),
    }
}

fn reject_unsupported_api_tokens_in_file(path: &Path) -> Result<(), &'static str> {
    let bytes = read_bounded(path, 1024 * 1024).map_err(|_| "legacy_authentication_layout")?;
    let value: Value =
        serde_json::from_slice(&bytes).map_err(|_| "legacy_authentication_invalid_credential")?;
    reject_unsupported_api_tokens(&value)
}

fn strip_inline_comment(line: &str) -> &str {
    let mut in_quotes = false;
    let mut escaped = false;
    for (index, character) in line.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match character {
            '\\' if in_quotes => escaped = true,
            '"' => in_quotes = !in_quotes,
            '#' if !in_quotes => return &line[..index],
            _ => {}
        }
    }
    line
}

enum ConfiguredPrimaryState {
    NotConfigured,
    Configured(PathBuf),
    Missing,
}

struct ConfiguredStatePaths {
    primary_file_state: ConfiguredPrimaryState,
    auxiliary_paths: Vec<PathBuf>,
}

fn configured_credential_file(root: &Path) -> Result<Option<PathBuf>, &'static str> {
    let mut value = None;
    for name in [
        "apollo.conf",
        "sunshine.conf",
        "vibeshine.conf",
        "vibepollo.conf",
    ] {
        let relative = Path::new(name);
        if !root
            .join(relative)
            .try_exists()
            .map_err(|_| "legacy_authentication_layout")?
        {
            continue;
        }
        let path = regular_contained_file(root, relative)?;
        let text = String::from_utf8(
            read_bounded(&path, 1024 * 1024).map_err(|_| "legacy_authentication_layout")?,
        )
        .map_err(|_| "legacy_authentication_layout")?;
        for line in text.lines() {
            let line = strip_inline_comment(line).trim();
            if line.is_empty() || line.starts_with(';') {
                continue;
            }
            let Some((key, candidate)) = line.split_once('=') else {
                continue;
            };
            if key.trim().eq_ignore_ascii_case("credentials_file") {
                let candidate = candidate.trim().trim_matches('"').trim();
                if candidate.is_empty() {
                    continue;
                }
                let candidate = regular_contained_file(root, Path::new(candidate))?;
                if value
                    .replace(candidate.clone())
                    .is_some_and(|previous| previous != candidate)
                {
                    return Err("legacy_authentication_credential_ambiguous");
                }
            }
        }
    }
    Ok(value)
}

fn configured_state_paths(root: &Path) -> Result<ConfiguredStatePaths, &'static str> {
    let mut configured_primary_file_state = None::<(PathBuf, Option<PathBuf>)>;
    let mut configured_vibeshine_file_state = None::<(PathBuf, Option<PathBuf>)>;
    for name in [
        "apollo.conf",
        "sunshine.conf",
        "vibeshine.conf",
        "vibepollo.conf",
    ] {
        let relative = Path::new(name);
        if !root
            .join(relative)
            .try_exists()
            .map_err(|_| "legacy_authentication_layout")?
        {
            continue;
        }
        let path = regular_contained_file(root, relative)?;
        let text = String::from_utf8(
            read_bounded(&path, 1024 * 1024).map_err(|_| "legacy_authentication_layout")?,
        )
        .map_err(|_| "legacy_authentication_layout")?;
        for line in text.lines() {
            let line = strip_inline_comment(line).trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let key = key.trim().to_ascii_lowercase();
            if !matches!(key.as_str(), "file_state" | "vibeshine_file_state") {
                continue;
            }
            let value = value.trim().trim_matches('"');
            if value.is_empty() {
                continue;
            }
            let relative = Path::new(value);
            if relative.as_os_str().is_empty()
                || !relative
                    .components()
                    .all(|part| matches!(part, Component::Normal(_)))
            {
                return Err("legacy_authentication_layout");
            }
            match key.as_str() {
                "file_state" => {
                    let path = optional_regular_contained_file(root, relative)?;
                    if let Some((previous_relative, previous_path)) = &configured_primary_file_state
                        && previous_relative != relative
                        && !matches!(
                            (previous_path.as_ref(), path.as_ref()),
                            (Some(previous), Some(current)) if previous == current
                        )
                    {
                        return Err("legacy_authentication_credential_ambiguous");
                    }
                    configured_primary_file_state = Some((relative.to_path_buf(), path));
                }
                "vibeshine_file_state" => {
                    let path = optional_regular_contained_file(root, relative)?;
                    if let Some((previous_relative, previous_path)) =
                        &configured_vibeshine_file_state
                        && previous_relative != relative
                        && !matches!(
                            (previous_path.as_ref(), path.as_ref()),
                            (Some(previous), Some(current)) if previous == current
                        )
                    {
                        return Err("legacy_authentication_credential_ambiguous");
                    }
                    configured_vibeshine_file_state = Some((relative.to_path_buf(), path));
                }
                _ => unreachable!("only supported state keys reach this point"),
            }
        }
    }
    let primary_file_state = match configured_primary_file_state {
        Some((_, Some(path))) => ConfiguredPrimaryState::Configured(path),
        Some((_, None)) => ConfiguredPrimaryState::Missing,
        None => ConfiguredPrimaryState::NotConfigured,
    };
    let auxiliary_paths = configured_vibeshine_file_state
        .and_then(|(_, path)| path)
        .into_iter()
        .collect();
    Ok(ConfiguredStatePaths {
        primary_file_state,
        auxiliary_paths,
    })
}

fn canonical_directory(path: &Path) -> Result<PathBuf, &'static str> {
    let metadata = std::fs::symlink_metadata(path).map_err(|_| "legacy_authentication_layout")?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err("legacy_authentication_layout");
    }
    path.canonicalize()
        .map_err(|_| "legacy_authentication_layout")
}

fn regular_contained_file(root: &Path, relative: &Path) -> Result<PathBuf, &'static str> {
    if relative.as_os_str().is_empty()
        || !relative
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
    {
        return Err("legacy_authentication_layout");
    }
    let path = root
        .join(relative)
        .canonicalize()
        .map_err(|_| "legacy_authentication_layout")?;
    if !path.starts_with(root)
        || !std::fs::metadata(&path)
            .map_err(|_| "legacy_authentication_layout")?
            .is_file()
    {
        return Err("legacy_authentication_layout");
    }
    Ok(path)
}

fn optional_regular_contained_file(
    root: &Path,
    relative: &Path,
) -> Result<Option<PathBuf>, &'static str> {
    if relative.as_os_str().is_empty()
        || !relative
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
    {
        return Err("legacy_authentication_layout");
    }
    let path = root.join(relative);
    match std::fs::symlink_metadata(&path) {
        Ok(_) => regular_contained_file(root, relative).map(Some),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut ancestor = path.parent();
            while let Some(candidate) = ancestor {
                match std::fs::symlink_metadata(candidate) {
                    Ok(_) => {
                        let canonical = candidate
                            .canonicalize()
                            .map_err(|_| "legacy_authentication_layout")?;
                        if !canonical.starts_with(root)
                            || !std::fs::metadata(canonical)
                                .map_err(|_| "legacy_authentication_layout")?
                                .is_dir()
                        {
                            return Err("legacy_authentication_layout");
                        }
                        return Ok(None);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        ancestor = candidate.parent();
                    }
                    Err(_) => return Err("legacy_authentication_layout"),
                }
            }
            Err("legacy_authentication_layout")
        }
        Err(_) => Err("legacy_authentication_layout"),
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Namespace {
    format_version: u32,
    revision: u64,
    values: BTreeMap<String, Value>,
}

fn read_namespace(path: &Path) -> Result<Namespace, &'static str> {
    if !path
        .try_exists()
        .map_err(|_| "legacy_authentication_state_unavailable")?
    {
        return Ok(Namespace {
            format_version: 1,
            revision: 0,
            values: BTreeMap::new(),
        });
    }
    let namespace: Namespace = serde_json::from_slice(
        &read_bounded(path, 1024 * 1024).map_err(|_| "legacy_authentication_state_unavailable")?,
    )
    .map_err(|_| "legacy_authentication_state_unavailable")?;
    if namespace.format_version != 1 {
        return Err("legacy_authentication_state_unavailable");
    }
    Ok(namespace)
}

pub(crate) fn namespace_path(root: &Path) -> PathBuf {
    let encoded: String = PRINCIPAL
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let mut path = root.join("namespaces");
    for part in encoded.as_bytes().chunks(64) {
        path.push(std::str::from_utf8(part).expect("hex is ascii"));
    }
    path.join("state.json")
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    fn temporary_directory(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "amaterasu-authentication-migration-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    fn credential_record(username: &str, password: &str, salt: &str) -> String {
        let hash = format!(
            "{:x}",
            Sha256::digest(format!("{password}{salt}").as_bytes())
        );
        serde_json::json!({"username":username,"password":hash,"salt":salt}).to_string()
    }

    #[test]
    fn imports_only_a_supported_verifier_and_preserves_existing_namespace() {
        let source = temporary_directory("source");
        let state = temporary_directory("state");
        let salt = "AbCdEfGhIjKlMnOp";
        let password = "correct horse battery staple";
        let hash = format!(
            "{:x}",
            Sha256::digest(format!("{password}{salt}").as_bytes())
        );
        std::fs::write(
            source.join("sunshine_state.json"),
            serde_json::json!({"username":"Admin","password":hash,"salt":salt,"root":{"uniqueid":"ignored"}}).to_string(),
        )
        .unwrap();
        let imported = import_legacy_authentication(&source).unwrap();
        assert_eq!(
            seed_authentication_state(&state, &imported).unwrap(),
            AuthenticationSeedOutcome::Seeded
        );
        let namespace: Namespace =
            serde_json::from_slice(&std::fs::read(namespace_path(&state)).unwrap()).unwrap();
        let document: Document =
            serde_json::from_value(namespace.values[STATE_KEY].clone()).unwrap();
        assert!(document.credential.unwrap().legacy_salt.is_some());
        assert_eq!(
            seed_authentication_state(&state, &imported).unwrap(),
            AuthenticationSeedOutcome::ExistingAmaterasuState
        );
        let _ = std::fs::remove_dir_all(source);
        let _ = std::fs::remove_dir_all(state);
    }

    #[test]
    fn imports_hash_only_api_tokens_to_a_separate_versioned_sidecar() {
        const LEGACY_BEARER: &str = "0123456789abcdefghijklmnopqrstuv";
        let source = temporary_directory("hash-only-api-token-source");
        let state = temporary_directory("hash-only-api-token-state");
        let salt = "AbCdEfGhIjKlMnOp";
        let password = "correct horse battery staple";
        let credential_hash = format!(
            "{:x}",
            Sha256::digest(format!("{password}{salt}").as_bytes())
        );
        let token_hash = crate::crypto::token_digest(LEGACY_BEARER);
        std::fs::write(
            source.join("sunshine_state.json"),
            serde_json::json!({
                "username":"Admin",
                "password":credential_hash,
                "salt":salt,
                "root":{"api_tokens":[{
                    "hash":token_hash,
                    "username":"ApiAdmin",
                    "created_at":1_700_000_000i64,
                    "scopes":[{"path":"/v1/apps/[0-9]+","methods":["POST","get"]}]
                }]}
            })
            .to_string(),
        )
        .unwrap();

        let imported = import_legacy_authentication(&source).unwrap();
        assert_eq!(
            imported.legacy_api_tokens.version,
            crate::legacy_tokens::LEGACY_API_TOKEN_STORE_VERSION
        );
        assert_eq!(imported.legacy_api_tokens.tokens.len(), 1);
        assert_eq!(imported.legacy_api_tokens.tokens[0].owner, "ApiAdmin");
        assert_eq!(
            imported.legacy_api_tokens.tokens[0].created_at,
            1_700_000_000
        );
        assert_eq!(
            imported.legacy_api_tokens.tokens[0].scopes[0].path,
            "/v1/apps/[0-9]+"
        );
        assert!(
            imported.legacy_api_tokens.tokens[0].scopes[0]
                .methods
                .contains("get")
        );

        assert_eq!(
            seed_authentication_state(&state, &imported).unwrap(),
            AuthenticationSeedOutcome::Seeded
        );
        let namespace: Namespace =
            serde_json::from_slice(&std::fs::read(namespace_path(&state)).unwrap()).unwrap();
        let stored = &namespace.values[LEGACY_API_TOKEN_STATE_KEY];
        assert_eq!(stored["version"], 1);
        assert_eq!(stored["tokens"][0]["record_version"], 1);
        assert!(stored["tokens"][0].get("expires_at").is_none());
        assert!(
            stored["tokens"][0]["token_id"]
                .as_str()
                .is_some_and(|digest| digest == crate::crypto::token_digest(LEGACY_BEARER))
        );
        assert!(!stored.to_string().contains(LEGACY_BEARER));

        std::fs::remove_dir_all(source).unwrap();
        std::fs::remove_dir_all(state).unwrap();
    }

    #[test]
    fn token_records_with_plaintext_or_unknown_fields_are_rejected() {
        let source = temporary_directory("plaintext-api-token-source");
        let salt = "AbCdEfGhIjKlMnOp";
        let password = "correct horse battery staple";
        let hash = format!(
            "{:x}",
            Sha256::digest(format!("{password}{salt}").as_bytes())
        );
        std::fs::write(
            source.join("sunshine_state.json"),
            serde_json::json!({
                "username":"Admin",
                "password":hash,
                "salt":salt,
                "root":{"api_tokens":[{
                    "token":"never-import-plaintext",
                    "username":"ApiAdmin",
                    "created_at":1,
                    "scopes":[]
                }]}
            })
            .to_string(),
        )
        .unwrap();
        assert!(matches!(
            import_legacy_authentication(&source),
            Err("legacy_authentication_tokens_unsupported")
        ));
        std::fs::remove_dir_all(source).unwrap();
    }

    #[test]
    fn token_records_without_a_credential_source_are_not_silently_ignored() {
        let source = temporary_directory("unpaired-api-token-source");
        std::fs::write(
            source.join("sunshine_state.json"),
            serde_json::json!({
                "root":{"api_tokens":[{
                    "hash":"a".repeat(64),
                    "username":"ApiAdmin",
                    "created_at":1,
                    "scopes":[]
                }]}
            })
            .to_string(),
        )
        .unwrap();
        assert!(matches!(
            import_legacy_authentication(&source),
            Err("legacy_authentication_tokens_unsupported")
        ));
        std::fs::remove_dir_all(source).unwrap();
    }

    #[test]
    fn orphan_legacy_token_sidecars_are_validated_and_never_overwritten() {
        let source = temporary_directory("orphan-token-sidecar-source");
        let state = temporary_directory("orphan-token-sidecar-state");
        std::fs::write(
            source.join("sunshine_state.json"),
            credential_record("Admin", "correct password", "CredentialSalt12"),
        )
        .unwrap();
        let imported = import_legacy_authentication(&source).unwrap();
        let path = namespace_path(&state);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();

        for sidecar in [
            serde_json::json!({"version":2,"tokens":[]}),
            serde_json::json!({"version":999,"tokens":[]}),
        ] {
            let namespace = Namespace {
                format_version: 1,
                revision: 1,
                values: BTreeMap::from([(LEGACY_API_TOKEN_STATE_KEY.into(), sidecar)]),
            };
            std::fs::write(&path, serde_json::to_vec(&namespace).unwrap()).unwrap();
            let before = std::fs::read(&path).unwrap();
            assert!(matches!(
                seed_authentication_state(&state, &imported),
                Err("legacy_authentication_existing_state_invalid")
            ));
            assert_eq!(std::fs::read(&path).unwrap(), before);
        }

        std::fs::remove_dir_all(source).unwrap();
        std::fs::remove_dir_all(state).unwrap();
    }

    #[test]
    fn existing_state_requires_a_usable_credential_and_positive_revision() {
        let source = temporary_directory("existing-state-source");
        let state = temporary_directory("existing-state-target");
        std::fs::write(
            source.join("sunshine_state.json"),
            credential_record("Imported", "imported password", "ImportedSalt1234"),
        )
        .unwrap();
        let imported = import_legacy_authentication(&source).unwrap();
        let path = namespace_path(&state);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();

        for (name, revision, existing) in [
            ("null", 1, Value::Null),
            (
                "uninitialized",
                1,
                serde_json::json!({"credential":null,"tokens":[]}),
            ),
            (
                "invalid-verifier",
                1,
                serde_json::json!({
                    "credential":{"username":"Admin","password_hash":"invalid","legacy_salt":null,"case_insensitive_username":true},
                    "tokens":[]
                }),
            ),
            (
                "zero-revision",
                0,
                serde_json::to_value(&imported.document).unwrap(),
            ),
        ] {
            let namespace = Namespace {
                format_version: 1,
                revision,
                values: BTreeMap::from([(STATE_KEY.into(), existing)]),
            };
            std::fs::write(&path, serde_json::to_vec(&namespace).unwrap()).unwrap();
            let before = std::fs::read(&path).unwrap();
            assert!(
                matches!(
                    seed_authentication_state(&state, &imported),
                    Err("legacy_authentication_existing_state_invalid")
                ),
                "{name}"
            );
            assert_eq!(std::fs::read(&path).unwrap(), before, "{name}");
        }

        for has_existing_credential in [false, true] {
            let mut values = BTreeMap::from([(
                SESSION_STATE_KEY.into(),
                serde_json::json!({"version":999,"sessions":[]}),
            )]);
            if has_existing_credential {
                values.insert(
                    STATE_KEY.into(),
                    serde_json::to_value(&imported.document).unwrap(),
                );
            }
            let namespace = Namespace {
                format_version: 1,
                revision: 1,
                values,
            };
            std::fs::write(&path, serde_json::to_vec(&namespace).unwrap()).unwrap();
            let before = std::fs::read(&path).unwrap();
            assert!(matches!(
                seed_authentication_state(&state, &imported),
                Err("legacy_authentication_existing_state_invalid")
            ));
            assert_eq!(std::fs::read(&path).unwrap(), before);
        }

        let namespace = Namespace {
            format_version: 1,
            revision: 1,
            values: BTreeMap::from([(
                SESSION_STATE_KEY.into(),
                serde_json::to_value(SessionDocument::default()).unwrap(),
            )]),
        };
        std::fs::write(&path, serde_json::to_vec(&namespace).unwrap()).unwrap();
        assert_eq!(
            seed_authentication_state(&state, &imported).unwrap(),
            AuthenticationSeedOutcome::Seeded
        );
        assert_eq!(
            seed_authentication_state(&state, &imported).unwrap(),
            AuthenticationSeedOutcome::ExistingAmaterasuState
        );

        std::fs::remove_file(&path).unwrap();
        assert_eq!(
            seed_authentication_state(&state, &imported).unwrap(),
            AuthenticationSeedOutcome::Seeded
        );
        let valid_existing = std::fs::read(&path).unwrap();
        assert_eq!(
            seed_authentication_state(&state, &imported).unwrap(),
            AuthenticationSeedOutcome::ExistingAmaterasuState
        );
        assert_eq!(std::fs::read(&path).unwrap(), valid_existing);
        let _ = std::fs::remove_dir_all(source);
        let _ = std::fs::remove_dir_all(state);
    }

    #[test]
    fn rejects_external_legacy_credential_sources() {
        let source = temporary_directory("reject");
        std::fs::write(
            source.join("sunshine.conf"),
            "credentials_file = ../secret.json\n",
        )
        .unwrap();
        assert!(matches!(
            import_legacy_authentication(&source),
            Err("legacy_authentication_layout")
        ));
        let _ = std::fs::remove_dir_all(source);
    }

    #[test]
    fn imports_a_configured_credential_file_with_a_quoted_hash_and_trailing_comment() {
        let source = temporary_directory("configured-credential-hash-source");
        std::fs::create_dir_all(source.join("private")).unwrap();
        let salt = "AbCdEfGhIjKlMnOp";
        let password = "correct horse battery staple";
        let hash = format!(
            "{:x}",
            Sha256::digest(format!("{password}{salt}").as_bytes())
        );
        std::fs::write(
            source.join("private/legacy#credentials.json"),
            serde_json::json!({"username":"Admin","password":hash,"salt":salt}).to_string(),
        )
        .unwrap();
        std::fs::write(
            source.join("sunshine.conf"),
            "credentials_file = \"private/legacy#credentials.json\" # selected credentials\n",
        )
        .unwrap();

        let imported = import_legacy_authentication(&source).unwrap();
        assert_eq!(
            imported.document.credential.as_ref().unwrap().username,
            "Admin"
        );
        let _ = std::fs::remove_dir_all(source);
    }

    #[test]
    fn inherits_primary_file_state_when_credentials_file_is_absent_or_empty() {
        for (name, credentials_line) in [
            ("omitted-credentials-file", ""),
            ("empty-credentials-file", "credentials_file = \"\"\n"),
        ] {
            let source = temporary_directory(name);
            std::fs::create_dir_all(source.join("private")).unwrap();
            std::fs::write(
                source.join("private/primary.json"),
                credential_record("Current", "current password", "PrimarySalt12345"),
            )
            .unwrap();
            std::fs::write(
                source.join("sunshine_state.json"),
                credential_record("Stale", "stale password", "StaleSalt1234567"),
            )
            .unwrap();
            std::fs::write(
                source.join("sunshine.conf"),
                format!("{credentials_line}file_state = private/primary.json\n"),
            )
            .unwrap();

            let imported = import_legacy_authentication(&source).unwrap();
            assert_eq!(
                imported.document.credential.as_ref().unwrap().username,
                "Current"
            );
            let _ = std::fs::remove_dir_all(source);
        }
    }

    #[test]
    fn missing_inherited_file_state_does_not_fall_back_to_a_default_state_file() {
        let source = temporary_directory("missing-inherited-file-state");
        std::fs::write(
            source.join("sunshine_state.json"),
            credential_record("Stale", "stale password", "StaleSalt1234567"),
        )
        .unwrap();
        std::fs::write(
            source.join("sunshine.conf"),
            "file_state = private/not-created.json\n",
        )
        .unwrap();

        assert!(matches!(
            import_legacy_authentication(&source),
            Err("legacy_authentication_credential_ambiguous")
        ));
        let _ = std::fs::remove_dir_all(source);
    }

    #[test]
    fn missing_configured_primary_does_not_promote_auxiliary_state() {
        let source = temporary_directory("missing-primary-with-auxiliary-state");
        std::fs::create_dir_all(source.join("private")).unwrap();
        std::fs::write(
            source.join("private/auxiliary.json"),
            credential_record("Auxiliary", "auxiliary password", "AuxiliarySalt123"),
        )
        .unwrap();
        std::fs::write(
            source.join("sunshine.conf"),
            "file_state = private/not-created.json\nvibeshine_file_state = private/auxiliary.json\n",
        )
        .unwrap();

        assert!(matches!(
            import_legacy_authentication(&source),
            Err("legacy_authentication_credential_ambiguous")
        ));
        let _ = std::fs::remove_dir_all(source);
    }

    #[test]
    fn nonempty_credentials_file_overrides_primary_file_state() {
        let source = temporary_directory("credentials-file-override");
        std::fs::create_dir_all(source.join("private")).unwrap();
        std::fs::write(
            source.join("private/primary.json"),
            credential_record("Primary", "primary password", "PrimarySalt12345"),
        )
        .unwrap();
        std::fs::write(
            source.join("private/override.json"),
            credential_record("Override", "override password", "OverrideSalt1234"),
        )
        .unwrap();
        std::fs::write(
            source.join("sunshine_state.json"),
            credential_record("Stale", "stale password", "StaleSalt1234567"),
        )
        .unwrap();
        std::fs::write(
            source.join("sunshine.conf"),
            "file_state = private/primary.json\ncredentials_file = private/override.json\n",
        )
        .unwrap();

        let imported = import_legacy_authentication(&source).unwrap();
        assert_eq!(
            imported.document.credential.as_ref().unwrap().username,
            "Override"
        );
        let _ = std::fs::remove_dir_all(source);
    }

    #[test]
    fn explicit_credentials_still_reject_tokens_in_auxiliary_state() {
        let source = temporary_directory("explicit-credentials-auxiliary-tokens");
        std::fs::create_dir_all(source.join("private")).unwrap();
        std::fs::write(
            source.join("private/credentials.json"),
            credential_record("Admin", "correct password", "CredentialSalt12"),
        )
        .unwrap();
        std::fs::write(
            source.join("private/auxiliary.json"),
            r#"{"root":{"api_tokens":[]}}"#,
        )
        .unwrap();
        std::fs::write(
            source.join("sunshine.conf"),
            "credentials_file = private/credentials.json\nvibeshine_file_state = private/auxiliary.json\n",
        )
        .unwrap();
        assert!(import_legacy_authentication(&source).is_ok());

        std::fs::write(
            source.join("private/auxiliary.json"),
            r#"{"root":{"api_tokens":[{"token":"legacy-token"}]}}"#,
        )
        .unwrap();
        assert!(matches!(
            import_legacy_authentication(&source),
            Err("legacy_authentication_tokens_unsupported")
        ));
        let _ = std::fs::remove_dir_all(source);
    }

    #[test]
    fn unconfigured_primary_state_uses_default_primary_and_checks_auxiliary() {
        let source = temporary_directory("unconfigured-primary-state-discovery");
        std::fs::create_dir_all(source.join("private")).unwrap();
        std::fs::write(
            source.join("sunshine_state.json"),
            credential_record("Legacy", "legacy password", "LegacySalt123456"),
        )
        .unwrap();
        std::fs::write(
            source.join("private/auxiliary.json"),
            credential_record("Auxiliary", "auxiliary password", "AuxiliarySalt12"),
        )
        .unwrap();
        std::fs::write(
            source.join("sunshine.conf"),
            "vibeshine_file_state = private/auxiliary.json\n",
        )
        .unwrap();
        let root = canonical_directory(&source).unwrap();

        let candidates = credential_candidates(&root).unwrap();
        assert!(candidates.contains(&source.join("sunshine_state.json").canonicalize().unwrap()));
        assert_eq!(candidates.len(), 1);
        let imported = import_legacy_authentication(&source).unwrap();
        assert_eq!(
            imported.document.credential.as_ref().unwrap().username,
            "Legacy"
        );
        std::fs::write(
            source.join("private/auxiliary.json"),
            r#"{"root":{"api_tokens":[{"token":"legacy-token"}]}}"#,
        )
        .unwrap();
        assert!(matches!(
            import_legacy_authentication(&source),
            Err("legacy_authentication_tokens_unsupported")
        ));
        let _ = std::fs::remove_dir_all(source);
    }

    #[test]
    fn imports_custom_primary_state_and_ignores_an_absent_auxiliary_state_file() {
        let source = temporary_directory("configured-state-source");
        let state = temporary_directory("configured-state-target");
        let salt = "AbCdEfGhIjKlMnOp";
        let password = "correct horse battery staple";
        let hash = format!(
            "{:x}",
            Sha256::digest(format!("{password}{salt}").as_bytes())
        );
        std::fs::create_dir_all(source.join("private")).unwrap();
        std::fs::write(
            source.join("private/primary.json"),
            serde_json::json!({"username":"Admin","password":hash,"salt":salt}).to_string(),
        )
        .unwrap();
        std::fs::write(
            source.join("sunshine.conf"),
            "file_state = private/primary.json # custom primary\nvibeshine_file_state = private/not_created_yet.json\n",
        )
        .unwrap();

        let imported = import_legacy_authentication(&source).unwrap();
        assert_eq!(
            seed_authentication_state(&state, &imported).unwrap(),
            AuthenticationSeedOutcome::Seeded
        );
        let namespace: Namespace =
            serde_json::from_slice(&std::fs::read(namespace_path(&state)).unwrap()).unwrap();
        let document: Document =
            serde_json::from_value(namespace.values[STATE_KEY].clone()).unwrap();
        assert!(document.credential.as_ref().unwrap().legacy_salt.is_some());
        assert!(namespace.values[STATE_KEY].get("sessions").is_none());
        let reimported = import_legacy_authentication(&source).unwrap();
        assert_eq!(
            seed_authentication_state(&state, &reimported).unwrap(),
            AuthenticationSeedOutcome::ExistingAmaterasuState
        );

        std::fs::write(
            source.join("private/not_created_yet.json"),
            r#"{"root":{"api_tokens":[{"token":"legacy-token"}]}}"#,
        )
        .unwrap();
        assert!(matches!(
            import_legacy_authentication(&source),
            Err("legacy_authentication_tokens_unsupported")
        ));
        let _ = std::fs::remove_dir_all(source);
        let _ = std::fs::remove_dir_all(state);
    }

    #[test]
    fn imports_a_file_state_path_with_a_quoted_hash_and_trailing_comment() {
        let source = temporary_directory("file-state-hash-source");
        std::fs::create_dir_all(source.join("private")).unwrap();
        let salt = "AbCdEfGhIjKlMnOp";
        let password = "correct horse battery staple";
        let hash = format!(
            "{:x}",
            Sha256::digest(format!("{password}{salt}").as_bytes())
        );
        std::fs::write(
            source.join("private/primary#state.json"),
            serde_json::json!({"username":"Admin","password":hash,"salt":salt}).to_string(),
        )
        .unwrap();
        std::fs::write(
            source.join("sunshine.conf"),
            "file_state = \"private/primary#state.json\" # primary state\n",
        )
        .unwrap();

        let imported = import_legacy_authentication(&source).unwrap();
        assert_eq!(
            imported.document.credential.as_ref().unwrap().username,
            "Admin"
        );
        let _ = std::fs::remove_dir_all(source);
    }

    #[test]
    fn does_not_promote_a_vibeshine_file_state_to_login_credentials() {
        let source = temporary_directory("vibeshine-file-state-hash-source");
        std::fs::create_dir_all(source.join("private")).unwrap();
        let salt = "AbCdEfGhIjKlMnOp";
        let password = "correct horse battery staple";
        let hash = format!(
            "{:x}",
            Sha256::digest(format!("{password}{salt}").as_bytes())
        );
        std::fs::write(
            source.join("private/primary#state.json"),
            serde_json::json!({"username":"Admin","password":hash,"salt":salt}).to_string(),
        )
        .unwrap();
        std::fs::write(
            source.join("sunshine.conf"),
            "vibeshine_file_state = \"private/primary#state.json\" # primary state\n",
        )
        .unwrap();

        assert!(matches!(
            import_legacy_authentication(&source),
            Err("legacy_authentication_credential_ambiguous")
        ));
        let _ = std::fs::remove_dir_all(source);
    }

    #[cfg(windows)]
    #[test]
    fn imports_case_variant_file_state_spellings_as_one_windows_candidate() {
        let source = temporary_directory("windows-file-state-case-alias");
        std::fs::create_dir_all(source.join("private")).unwrap();
        let salt = "AbCdEfGhIjKlMnOp";
        let password = "correct horse battery staple";
        let hash = format!(
            "{:x}",
            Sha256::digest(format!("{password}{salt}").as_bytes())
        );
        std::fs::write(
            source.join("private/primary.json"),
            serde_json::json!({"username":"Admin","password":hash,"salt":salt}).to_string(),
        )
        .unwrap();
        std::fs::write(
            source.join("sunshine.conf"),
            "file_state = private/primary.json\nfile_state = PRIVATE/PRIMARY.JSON\n",
        )
        .unwrap();

        let root = canonical_directory(&source).unwrap();
        let candidates = credential_candidates(&root).unwrap();
        assert_eq!(candidates.len(), 1);
        let imported = import_legacy_authentication(&source).unwrap();
        assert_eq!(
            imported.document.credential.as_ref().unwrap().username,
            "Admin"
        );
        let _ = std::fs::remove_dir_all(source);
    }

    #[cfg(windows)]
    #[test]
    fn checks_case_variant_vibeshine_file_state_spellings_without_promoting_them() {
        let source = temporary_directory("windows-vibeshine-file-state-case-alias");
        std::fs::create_dir_all(source.join("private")).unwrap();
        let salt = "AbCdEfGhIjKlMnOp";
        let password = "correct horse battery staple";
        let hash = format!(
            "{:x}",
            Sha256::digest(format!("{password}{salt}").as_bytes())
        );
        std::fs::write(
            source.join("private/primary.json"),
            serde_json::json!({"username":"Admin","password":hash,"salt":salt}).to_string(),
        )
        .unwrap();
        std::fs::write(
            source.join("sunshine.conf"),
            "vibeshine_file_state = private/primary.json\nvibeshine_file_state = PRIVATE/PRIMARY.JSON\n",
        )
        .unwrap();

        let root = canonical_directory(&source).unwrap();
        let candidates = token_validation_candidates(&root).unwrap();
        assert_eq!(candidates.len(), 1);
        assert!(matches!(
            import_legacy_authentication(&source),
            Err("legacy_authentication_credential_ambiguous")
        ));
        let _ = std::fs::remove_dir_all(source);
    }

    #[cfg(unix)]
    #[test]
    fn keeps_distinct_case_sensitive_file_state_paths_conflicting_on_unix() {
        let source = temporary_directory("unix-file-state-case-distinct");
        std::fs::create_dir_all(source.join("private")).unwrap();
        let lowercase_path = source.join("private/primary.json");
        let uppercase_path = source.join("private/PRIMARY.JSON");
        std::fs::write(&lowercase_path, b"{}").unwrap();
        std::fs::write(&uppercase_path, b"{}").unwrap();

        if lowercase_path.canonicalize().unwrap() == uppercase_path.canonicalize().unwrap() {
            let _ = std::fs::remove_dir_all(source);
            return;
        }

        std::fs::write(
            source.join("sunshine.conf"),
            "file_state = private/primary.json\nfile_state = private/PRIMARY.JSON\n",
        )
        .unwrap();
        let root = canonical_directory(&source).unwrap();

        assert!(matches!(
            configured_state_paths(&root),
            Err("legacy_authentication_credential_ambiguous")
        ));
        let _ = std::fs::remove_dir_all(source);
    }

    #[cfg(unix)]
    #[test]
    fn keeps_distinct_case_sensitive_vibeshine_paths_conflicting_on_unix() {
        let source = temporary_directory("unix-vibeshine-case-distinct");
        std::fs::create_dir_all(source.join("private")).unwrap();
        let lowercase_path = source.join("private/primary.json");
        let uppercase_path = source.join("private/PRIMARY.JSON");
        std::fs::write(&lowercase_path, b"{}").unwrap();
        std::fs::write(&uppercase_path, b"{}").unwrap();

        if lowercase_path.canonicalize().unwrap() == uppercase_path.canonicalize().unwrap() {
            let _ = std::fs::remove_dir_all(source);
            return;
        }

        std::fs::write(
            source.join("sunshine.conf"),
            "vibeshine_file_state = private/primary.json\nvibeshine_file_state = private/PRIMARY.JSON\n",
        )
        .unwrap();
        let root = canonical_directory(&source).unwrap();

        assert!(matches!(
            configured_state_paths(&root),
            Err("legacy_authentication_credential_ambiguous")
        ));
        let _ = std::fs::remove_dir_all(source);
    }

    #[cfg(unix)]
    #[test]
    fn rejects_missing_vibeshine_file_state_alias_on_unix() {
        let source = temporary_directory("unix-vibeshine-missing-alias");
        std::fs::create_dir_all(source.join("private")).unwrap();
        let missing_path = source.join("private/primary.json");
        let existing_path = source.join("private/PRIMARY.JSON");
        std::fs::write(&existing_path, b"{}").unwrap();

        if missing_path.try_exists().unwrap() {
            let _ = std::fs::remove_dir_all(source);
            return;
        }

        std::fs::write(
            source.join("sunshine.conf"),
            "vibeshine_file_state = private/primary.json\nvibeshine_file_state = private/PRIMARY.JSON\n",
        )
        .unwrap();
        let root = canonical_directory(&source).unwrap();

        assert!(matches!(
            configured_state_paths(&root),
            Err("legacy_authentication_credential_ambiguous")
        ));
        let _ = std::fs::remove_dir_all(source);
    }

    #[test]
    fn rejects_differently_spelled_missing_vibeshine_file_state_aliases() {
        let source = temporary_directory("missing-vibeshine-file-state-alias");
        std::fs::create_dir_all(source.join("private")).unwrap();
        std::fs::write(
            source.join("sunshine.conf"),
            "vibeshine_file_state = private/not-created.json\nvibeshine_file_state = PRIVATE/NOT-CREATED.JSON\n",
        )
        .unwrap();
        let root = canonical_directory(&source).unwrap();

        assert!(matches!(
            configured_state_paths(&root),
            Err("legacy_authentication_credential_ambiguous")
        ));
        let _ = std::fs::remove_dir_all(source);
    }

    #[test]
    fn rejects_differently_spelled_missing_file_state_aliases() {
        let source = temporary_directory("missing-file-state-alias");
        std::fs::create_dir_all(source.join("private")).unwrap();
        std::fs::write(
            source.join("sunshine.conf"),
            "file_state = private/not-created.json\nfile_state = private/NOT-CREATED.JSON\n",
        )
        .unwrap();
        let root = canonical_directory(&source).unwrap();

        assert!(matches!(
            configured_state_paths(&root),
            Err("legacy_authentication_credential_ambiguous")
        ));
        let _ = std::fs::remove_dir_all(source);
    }

    #[test]
    fn resolves_repeated_file_state_declarations_across_predecessor_configs() {
        const CONFIG_FILES: [&str; 4] = [
            "apollo.conf",
            "sunshine.conf",
            "vibeshine.conf",
            "vibepollo.conf",
        ];

        for (first_index, first_config_file) in CONFIG_FILES.iter().enumerate() {
            for second_config_file in CONFIG_FILES.iter().skip(first_index + 1) {
                let pair_name = format!(
                    "{}-{}",
                    first_config_file.trim_end_matches(".conf"),
                    second_config_file.trim_end_matches(".conf")
                );

                let same_source = temporary_directory(&format!("cross-config-same-{pair_name}"));
                std::fs::create_dir_all(same_source.join("private")).unwrap();
                std::fs::write(
                    same_source.join("private/primary.json"),
                    credential_record("Shared", "shared password", "SharedSalt123456"),
                )
                .unwrap();
                #[cfg(unix)]
                std::os::unix::fs::symlink(
                    "primary.json",
                    same_source.join("private/primary-alias.json"),
                )
                .unwrap();
                let second_state_path = if cfg!(unix) {
                    "private/primary-alias.json"
                } else {
                    "private/primary.json"
                };
                for (config_file, state_path) in [
                    (*first_config_file, "private/primary.json"),
                    (*second_config_file, second_state_path),
                ] {
                    std::fs::write(
                        same_source.join(config_file),
                        format!("file_state = {state_path}\n"),
                    )
                    .unwrap();
                }

                let root = canonical_directory(&same_source).unwrap();
                let candidates = credential_candidates(&root).unwrap();
                assert_eq!(
                    candidates,
                    vec![
                        same_source
                            .join("private/primary.json")
                            .canonicalize()
                            .unwrap()
                    ],
                    "the {pair_name} declarations should resolve to one candidate"
                );
                let imported = import_legacy_authentication(&same_source).unwrap();
                assert_eq!(
                    imported.document.credential.as_ref().unwrap().username,
                    "Shared",
                    "the {pair_name} declarations should import the shared credential once"
                );
                let _ = std::fs::remove_dir_all(same_source);

                let distinct_source =
                    temporary_directory(&format!("cross-config-distinct-{pair_name}"));
                std::fs::create_dir_all(distinct_source.join("private")).unwrap();
                std::fs::write(
                    distinct_source.join("private/first.json"),
                    credential_record("First", "first password", "FirstSalt1234567"),
                )
                .unwrap();
                std::fs::write(
                    distinct_source.join("private/second.json"),
                    credential_record("Second", "second password", "SecondSalt123456"),
                )
                .unwrap();
                for (config_file, state_path) in [
                    (*first_config_file, "private/first.json"),
                    (*second_config_file, "private/second.json"),
                ] {
                    std::fs::write(
                        distinct_source.join(config_file),
                        format!("file_state = {state_path}\n"),
                    )
                    .unwrap();
                }

                let root = canonical_directory(&distinct_source).unwrap();
                assert!(matches!(
                    configured_state_paths(&root),
                    Err("legacy_authentication_credential_ambiguous")
                ));
                assert!(matches!(
                    import_legacy_authentication(&distinct_source),
                    Err("legacy_authentication_credential_ambiguous")
                ));
                let _ = std::fs::remove_dir_all(distinct_source);

                let mixed_source = temporary_directory(&format!("cross-config-mixed-{pair_name}"));
                std::fs::create_dir_all(mixed_source.join("private")).unwrap();
                std::fs::write(
                    mixed_source.join("private/existing.json"),
                    credential_record("Existing", "existing password", "ExistingSalt1234"),
                )
                .unwrap();
                std::fs::write(
                    mixed_source.join(first_config_file),
                    "file_state = private/missing.json\n",
                )
                .unwrap();
                std::fs::write(
                    mixed_source.join(second_config_file),
                    "file_state = private/existing.json\n",
                )
                .unwrap();

                let root = canonical_directory(&mixed_source).unwrap();
                assert!(matches!(
                    configured_state_paths(&root),
                    Err("legacy_authentication_credential_ambiguous")
                ));
                assert!(matches!(
                    import_legacy_authentication(&mixed_source),
                    Err("legacy_authentication_credential_ambiguous")
                ));
                let _ = std::fs::remove_dir_all(mixed_source);
            }
        }
    }

    #[test]
    fn resolves_repeated_credentials_files_across_predecessor_config_pairs() {
        const CONFIG_FILES: [&str; 4] = [
            "apollo.conf",
            "sunshine.conf",
            "vibeshine.conf",
            "vibepollo.conf",
        ];

        for (first_index, &first_config) in CONFIG_FILES.iter().enumerate() {
            for &second_config in CONFIG_FILES.iter().skip(first_index + 1) {
                let pair_name = format!(
                    "{}-{}",
                    first_config.trim_end_matches(".conf"),
                    second_config.trim_end_matches(".conf")
                );
                let source = temporary_directory(&format!("credentials-cross-config-{pair_name}"));
                std::fs::create_dir_all(source.join("private")).unwrap();
                std::fs::write(
                    source.join("private/primary.json"),
                    credential_record("Shared", "shared password", "SharedSalt123456"),
                )
                .unwrap();
                #[cfg(unix)]
                std::os::unix::fs::symlink(
                    "primary.json",
                    source.join("private/primary-alias.json"),
                )
                .unwrap();
                let second_path = if cfg!(unix) {
                    "private/primary-alias.json"
                } else {
                    "private/primary.json"
                };
                for (config_file, credentials_path) in [
                    (first_config, "private/primary.json"),
                    (second_config, second_path),
                ] {
                    std::fs::write(
                        source.join(config_file),
                        format!("credentials_file = {credentials_path}\n"),
                    )
                    .unwrap();
                }

                let root = canonical_directory(&source).unwrap();
                let expected = source.join("private/primary.json").canonicalize().unwrap();
                assert_eq!(
                    credential_candidates(&root).unwrap(),
                    vec![expected],
                    "the {pair_name} declarations should resolve to one canonical candidate"
                );
                let imported = import_legacy_authentication(&source).unwrap();
                assert_eq!(
                    imported.document.credential.as_ref().unwrap().username,
                    "Shared",
                    "the {pair_name} declarations should import the shared credential once"
                );
                let _ = std::fs::remove_dir_all(source);

                let distinct =
                    temporary_directory(&format!("credentials-cross-config-distinct-{pair_name}"));
                std::fs::create_dir_all(distinct.join("private")).unwrap();
                std::fs::write(
                    distinct.join("private/first.json"),
                    credential_record("First", "first password", "FirstSalt1234567"),
                )
                .unwrap();
                std::fs::write(
                    distinct.join("private/second.json"),
                    credential_record("Second", "second password", "SecondSalt123456"),
                )
                .unwrap();
                for (config_file, credentials_path) in [
                    (first_config, "private/first.json"),
                    (second_config, "private/second.json"),
                ] {
                    std::fs::write(
                        distinct.join(config_file),
                        format!("credentials_file = {credentials_path}\n"),
                    )
                    .unwrap();
                }

                assert!(matches!(
                    import_legacy_authentication(&distinct),
                    Err("legacy_authentication_credential_ambiguous")
                ));
                std::fs::write(
                    distinct.join(second_config),
                    "credentials_file = ../outside.json\n",
                )
                .unwrap();
                assert!(matches!(
                    import_legacy_authentication(&distinct),
                    Err("legacy_authentication_layout")
                ));
                let _ = std::fs::remove_dir_all(distinct);
            }
        }
    }

    #[test]
    fn credentials_file_overrides_file_state_across_predecessor_config_pairs() {
        const CONFIG_FILES: [&str; 4] = [
            "apollo.conf",
            "sunshine.conf",
            "vibeshine.conf",
            "vibepollo.conf",
        ];

        for (first_index, &first_config) in CONFIG_FILES.iter().enumerate() {
            for &second_config in CONFIG_FILES.iter().skip(first_index + 1) {
                for (credentials_config, state_config) in
                    [(first_config, second_config), (second_config, first_config)]
                {
                    let pair_name = format!(
                        "{}-{}-credentials-in-{}",
                        first_config.trim_end_matches(".conf"),
                        second_config.trim_end_matches(".conf"),
                        credentials_config.trim_end_matches(".conf")
                    );
                    let source =
                        temporary_directory(&format!("credentials-overrides-state-{pair_name}"));
                    std::fs::create_dir_all(source.join("private")).unwrap();
                    std::fs::write(
                        source.join("private/explicit.json"),
                        credential_record("Explicit", "explicit password", "ExplicitSalt1234"),
                    )
                    .unwrap();
                    std::fs::write(
                        source.join("private/fallback.json"),
                        credential_record("Fallback", "fallback password", "FallbackSalt1234"),
                    )
                    .unwrap();
                    std::fs::write(
                        source.join(credentials_config),
                        "credentials_file = private/explicit.json\n",
                    )
                    .unwrap();
                    std::fs::write(
                        source.join(state_config),
                        "file_state = private/fallback.json\n",
                    )
                    .unwrap();

                    let imported = import_legacy_authentication(&source).unwrap();
                    assert_eq!(
                        imported.document.credential.as_ref().unwrap().username,
                        "Explicit",
                        "the {pair_name} file_state should defer to credentials_file"
                    );
                    let _ = std::fs::remove_dir_all(source);
                }
            }
        }
    }

    #[test]
    fn configured_primary_credentials_take_precedence_over_auxiliary_state() {
        let source = temporary_directory("ambiguous-configured-state");
        std::fs::write(
            source.join("primary.json"),
            credential_record("Primary", "primary password", "PrimarySalt12345"),
        )
        .unwrap();
        std::fs::write(
            source.join("auxiliary.json"),
            credential_record("Auxiliary", "auxiliary password", "AuxiliarySalt12"),
        )
        .unwrap();
        std::fs::write(
            source.join("sunshine.conf"),
            "file_state = primary.json\nvibeshine_file_state = auxiliary.json\n",
        )
        .unwrap();

        let imported = import_legacy_authentication(&source).unwrap();
        assert_eq!(
            imported.document.credential.as_ref().unwrap().username,
            "Primary"
        );
        let _ = std::fs::remove_dir_all(source);
    }

    #[test]
    fn rejects_unsafe_configured_state_paths() {
        let source = temporary_directory("unsafe-configured-state");
        std::fs::write(
            source.join("sunshine.conf"),
            "file_state = ../outside.json\n",
        )
        .unwrap();
        assert!(matches!(
            import_legacy_authentication(&source),
            Err("legacy_authentication_layout")
        ));

        let outside = source.parent().unwrap().join("outside-auth-state.json");
        std::fs::write(&outside, b"{}").unwrap();
        std::fs::write(
            source.join("sunshine.conf"),
            format!("file_state = {}\n", outside.display()),
        )
        .unwrap();
        assert!(matches!(
            import_legacy_authentication(&source),
            Err("legacy_authentication_layout")
        ));
        let _ = std::fs::remove_file(outside);
        let _ = std::fs::remove_dir_all(source);
    }

    #[cfg(unix)]
    #[test]
    fn rejects_a_missing_state_path_beneath_an_escaping_symlink() {
        use std::os::unix::fs::symlink;

        let source = temporary_directory("escaping-state-source");
        let outside = source
            .parent()
            .unwrap()
            .join("outside-auth-state-directory");
        std::fs::create_dir_all(&outside).unwrap();
        symlink(&outside, source.join("external")).unwrap();
        std::fs::write(
            source.join("sunshine.conf"),
            "vibeshine_file_state = external/not_created_yet.json\n",
        )
        .unwrap();

        assert!(matches!(
            import_legacy_authentication(&source),
            Err("legacy_authentication_layout")
        ));
        let _ = std::fs::remove_dir_all(outside);
        let _ = std::fs::remove_dir_all(source);
    }
}
