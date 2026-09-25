//! Strict, one-way migration of a legacy web-login verifier.

use crate::protocol::{Credential, Document};
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
const PRINCIPAL: &str = "amaterasu.authentication";
const STATE_KEY: &str = "authentication/v1";

/// A validated predecessor verifier. It intentionally has no `Debug` impl so
/// callers cannot accidentally log the retained password verifier.
pub struct LegacyAuthenticationImport {
    document: Document,
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
    for path in candidates {
        let bytes = read_bounded(&path, 1024 * 1024).map_err(|_| "legacy_authentication_layout")?;
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|_| "legacy_authentication_invalid_credential")?;
        reject_unsupported_api_tokens(&value)?;
        if !["username", "password", "salt"]
            .iter()
            .any(|key| value.get(key).is_some())
        {
            continue;
        }
        let legacy: LegacyCredential = serde_json::from_value(value)
            .map_err(|_| "legacy_authentication_invalid_credential")?;
        if legacy.api_tokens_present() {
            return Err("legacy_authentication_tokens_unsupported");
        }
        let document = Document {
            credential: Some(Credential {
                username: legacy.username,
                password_hash: legacy.password,
                legacy_salt: Some(legacy.salt),
                case_insensitive_username: true,
            }),
            tokens: Vec::new(),
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
    create_private_directory(state_root).map_err(|_| "legacy_authentication_state_unavailable")?;
    let _lock = RootLock::acquire(state_root).map_err(|_| "legacy_authentication_state_busy")?;
    let path = namespace_path(state_root);
    let mut namespace = read_namespace(&path)?;
    if !namespace.values.is_empty() {
        return Ok(AuthenticationSeedOutcome::ExistingAmaterasuState);
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
    let directory = path
        .parent()
        .ok_or("legacy_authentication_state_unavailable")?;
    create_private_directory(directory).map_err(|_| "legacy_authentication_state_unavailable")?;
    write_json_atomically(&path, &namespace)
        .map_err(|_| "legacy_authentication_state_unavailable")?;
    Ok(AuthenticationSeedOutcome::Seeded)
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
    fn api_tokens_present(&self) -> bool {
        self.root
            .as_ref()
            .is_some_and(|root| !root.api_tokens.is_empty())
    }
}

fn credential_candidates(root: &Path) -> Result<Vec<PathBuf>, &'static str> {
    if let Some(path) = configured_credential_file(root)? {
        return Ok(vec![path]);
    }
    let configured_state = configured_state_paths(root)?;
    match &configured_state.primary_file_state {
        ConfiguredPrimaryState::Configured(path) => Ok(vec![path.clone()]),
        ConfiguredPrimaryState::Missing => Ok(Vec::new()),
        ConfiguredPrimaryState::NotConfigured => legacy_state_candidates(root, &configured_state),
    }
}

fn token_validation_candidates(root: &Path) -> Result<Vec<PathBuf>, &'static str> {
    if configured_credential_file(root)?.is_some() {
        return Ok(Vec::new());
    }
    let configured_state = configured_state_paths(root)?;
    legacy_state_candidates(root, &configured_state)
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

fn namespace_path(root: &Path) -> PathBuf {
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
    fn unconfigured_primary_state_keeps_default_and_auxiliary_discovery() {
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
        assert!(
            candidates.contains(
                &source
                    .join("private/auxiliary.json")
                    .canonicalize()
                    .unwrap()
            )
        );
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
    fn imports_a_vibeshine_file_state_path_with_a_quoted_hash_and_trailing_comment() {
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

        let imported = import_legacy_authentication(&source).unwrap();
        assert_eq!(
            imported.document.credential.as_ref().unwrap().username,
            "Admin"
        );
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
    fn imports_case_variant_vibeshine_file_state_spellings_as_one_windows_candidate() {
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
        let candidates = credential_candidates(&root).unwrap();
        assert_eq!(candidates.len(), 1);
        let imported = import_legacy_authentication(&source).unwrap();
        assert_eq!(
            imported.document.credential.as_ref().unwrap().username,
            "Admin"
        );
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
