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
    let mut documents = Vec::new();
    for path in candidates {
        let bytes = read_bounded(&path, 1024 * 1024).map_err(|_| "legacy_authentication_layout")?;
        let legacy: LegacyCredential = serde_json::from_slice(&bytes)
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
    let configured = configured_credential_file(root)?;
    if let Some(path) = configured {
        return Ok(vec![path]);
    }
    let mut files = Vec::new();
    for name in STATE_FILES {
        let path = root.join(name);
        if path
            .try_exists()
            .map_err(|_| "legacy_authentication_layout")?
        {
            files.push(regular_contained_file(root, Path::new(name))?);
        }
    }
    Ok(files)
}

fn configured_credential_file(root: &Path) -> Result<Option<PathBuf>, &'static str> {
    let mut value = None;
    for name in [
        "apollo.conf",
        "sunshine.conf",
        "vibeshine.conf",
        "vibepollo.conf",
    ] {
        let path = root.join(name);
        if !path
            .try_exists()
            .map_err(|_| "legacy_authentication_layout")?
        {
            continue;
        }
        let text = String::from_utf8(
            read_bounded(&path, 1024 * 1024).map_err(|_| "legacy_authentication_layout")?,
        )
        .map_err(|_| "legacy_authentication_layout")?;
        for line in text.lines() {
            let Some((key, candidate)) = line.split_once('=') else {
                continue;
            };
            if key.trim().eq_ignore_ascii_case("credentials_file") {
                let candidate =
                    regular_contained_file(root, Path::new(candidate.trim().trim_matches('"')))?;
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
}
