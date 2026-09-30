//! Strict import of the current Sunshine/Vibeshine pairing layout.
//!
//! The importer reads only a predecessor configuration root supplied by the
//! installer. It deliberately supports the current `root.named_devices`
//! schema and standard or contained configured identity files.
//! It does not follow configured absolute paths or accept a partial identity:
//! the caller must preserve the predecessor when this conversion fails.

use crate::protocol::{Client, Document, Identity, MAX_CLIENTS, MAX_DOCUMENT_BYTES};
use amaterasu_storage::{RootLock, create_private_directory, read_bounded, write_json_atomically};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_LEGACY_STATE_BYTES: u64 = 1024 * 1024;
const STATE_FORMAT_VERSION: u32 = 1;
const PAIRING_PRINCIPAL: &str = "amaterasu.pairing";
const PAIRING_STATE_KEY: &str = "pairing/v1";
const LEGACY_STATE_FILES: &[&str] = &[
    "sunshine_state.json",
    "vibeshine_state.json",
    "apollo_state.json",
    "vibepollo_state.json",
];

/// The result of a validated predecessor pairing conversion.
///
/// This type intentionally does not implement `Debug`: it contains a host
/// private key and callers must pass it directly to [`seed_pairing_state`].
pub struct LegacyPairingImport {
    document: Document,
}

/// The effect of attempting to seed the pairing namespace.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PairingSeedOutcome {
    /// A validated legacy identity and its paired clients were committed.
    Seeded,
    /// Amaterasu had already used this namespace, so no file was changed.
    ExistingAmaterasuState,
}

/// Converts the standard current Sunshine/Vibeshine pairing configuration.
///
/// `source_root` must be the copied predecessor configuration root, not an
/// arbitrary parent directory. Configured state paths are resolved only to
/// contained regular files; the importer returns fixed error codes so private
/// material is never surfaced through installer logs.
pub fn import_legacy_pairing(
    source_root: &Path,
    now: u64,
) -> Result<LegacyPairingImport, &'static str> {
    ensure_real_directory(source_root)?;
    let state = read_legacy_state(source_root)?;
    let (certificate_path, private_key_path) = legacy_identity_paths(source_root)?;
    let identity = Identity {
        host_id: state.host_id,
        certificate_pem: read_legacy_file(&certificate_path, 4096)?,
        private_key_pem: read_legacy_file(&private_key_path, 4096)?,
    };

    let mut clients = Vec::with_capacity(state.named_devices.len());
    let mut certificate_ids = BTreeSet::new();
    for device in state.named_devices {
        if !device.enabled {
            continue;
        }
        if clients.len() >= MAX_CLIENTS {
            return Err("legacy_pairing_client_limit");
        }
        let certificate = crate::crypto::parse_certificate(&device.certificate_pem, None)
            .map_err(|_| "legacy_pairing_invalid_client")?;
        if device.name.is_empty()
            || device.name.len() > 128
            || device.name.chars().any(char::is_control)
            || !valid_identifier(&device.client_id, 64)
            || !certificate_ids.insert(certificate.fingerprint.clone())
        {
            return Err("legacy_pairing_invalid_client");
        }
        clients.push(Client {
            certificate_id: certificate.fingerprint,
            client_id: device.client_id,
            name: device.name,
            certificate_pem: device.certificate_pem,
            paired_at: now,
            config_overrides: crate::protocol::ClientConfigOverrides::default(),
        });
    }

    let document = Document {
        identity: Some(identity),
        clients,
    };
    document
        .validate(now)
        .map_err(|_| "legacy_pairing_invalid_identity")?;
    if serde_json::to_vec(&document).map_or(true, |bytes| bytes.len() > MAX_DOCUMENT_BYTES) {
        return Err("legacy_pairing_resource_limit");
    }
    Ok(LegacyPairingImport { document })
}

/// Seeds Amaterasu's pairing namespace after [`import_legacy_pairing`] succeeds.
///
/// The state root is locked for the full read/check/write transaction. A
/// namespace with any prior value is left byte-for-byte untouched, preserving
/// an Amaterasu identity or pairing created by an earlier installation.
pub fn seed_pairing_state(
    state_root: &Path,
    import: &LegacyPairingImport,
) -> Result<PairingSeedOutcome, &'static str> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "legacy_pairing_clock_failure")?
        .as_secs();
    import
        .document
        .validate(now)
        .map_err(|_| "legacy_pairing_invalid_identity")?;

    create_private_directory(state_root).map_err(|_| "legacy_pairing_state_unavailable")?;
    let _lock = RootLock::acquire(state_root).map_err(|_| "legacy_pairing_state_busy")?;
    let namespace_path = pairing_namespace_path(state_root);
    let mut namespace = read_namespace(&namespace_path)?;
    if !namespace.values.is_empty() {
        return Ok(PairingSeedOutcome::ExistingAmaterasuState);
    }
    namespace
        .revision
        .checked_add(1)
        .ok_or("legacy_pairing_state_unavailable")
        .map(|revision| namespace.revision = revision)?;
    namespace.values.insert(
        PAIRING_STATE_KEY.into(),
        serde_json::to_value(&import.document).map_err(|_| "legacy_pairing_state_unavailable")?,
    );
    create_namespace_directories(state_root, &namespace_path)?;
    write_json_atomically(&namespace_path, &namespace)
        .map_err(|_| "legacy_pairing_state_unavailable")?;
    Ok(PairingSeedOutcome::Seeded)
}

#[derive(Deserialize)]
struct LegacyState {
    root: LegacyRoot,
}

#[derive(Deserialize)]
struct LegacyRoot {
    uniqueid: String,
    #[serde(default)]
    named_devices: Vec<LegacyDevice>,
    #[serde(default)]
    api_tokens: Vec<Value>,
}

#[derive(Deserialize)]
struct LegacyDevice {
    name: String,
    uuid: String,
    cert: String,
    #[serde(default = "enabled_by_default")]
    enabled: bool,
}

impl From<LegacyState> for ParsedLegacyState {
    fn from(value: LegacyState) -> Self {
        Self {
            host_id: value.root.uniqueid,
            api_tokens_present: !value.root.api_tokens.is_empty(),
            named_devices: value
                .root
                .named_devices
                .into_iter()
                .map(|device| ParsedLegacyDevice {
                    name: device.name,
                    client_id: device.uuid,
                    certificate_pem: device.cert,
                    enabled: device.enabled,
                })
                .collect(),
        }
    }
}

struct ParsedLegacyState {
    host_id: String,
    api_tokens_present: bool,
    named_devices: Vec<ParsedLegacyDevice>,
}

struct ParsedLegacyDevice {
    name: String,
    client_id: String,
    certificate_pem: String,
    enabled: bool,
}

fn enabled_by_default() -> bool {
    true
}

fn read_legacy_state(source_root: &Path) -> Result<ParsedLegacyState, &'static str> {
    let source_root = source_root
        .canonicalize()
        .map_err(|_| "legacy_pairing_invalid_layout")?;
    let mut candidates = Vec::new();
    for name in LEGACY_STATE_FILES {
        let relative = Path::new(name);
        if source_root.join(relative).try_exists().ok() == Some(true) {
            let path = contained_regular_file(&source_root, relative)?;
            if !candidates.contains(&path) {
                candidates.push(path);
            }
        }
    }
    for path in configured_state_paths(&source_root)? {
        if !candidates.contains(&path) {
            candidates.push(path);
        }
    }
    let parsed: Vec<_> = candidates
        .iter()
        .filter_map(|path| {
            let bytes = read_legacy_bytes(path, MAX_LEGACY_STATE_BYTES).ok()?;
            serde_json::from_slice::<LegacyState>(&bytes)
                .ok()
                .map(ParsedLegacyState::from)
        })
        .collect();
    if parsed.len() != 1 {
        if parsed.len() > 1 {
            return Err("legacy_pairing_state_ambiguous");
        }
        if candidates.is_empty() {
            return Err("legacy_pairing_state_ambiguous");
        }
        return Err("legacy_pairing_invalid_state");
    }
    let state = parsed.into_iter().next().expect("one parsed state");
    if state.api_tokens_present {
        return Err("legacy_pairing_tokens_unsupported");
    }
    if state.named_devices.len() > MAX_CLIENTS {
        return Err("legacy_pairing_client_limit");
    }
    Ok(state)
}

fn configured_state_paths(source_root: &Path) -> Result<Vec<PathBuf>, &'static str> {
    let mut configured = BTreeMap::<String, PathBuf>::new();
    let mut paths = Vec::new();
    for name in [
        "apollo.conf",
        "sunshine.conf",
        "vibeshine.conf",
        "vibepollo.conf",
    ] {
        let config_relative = Path::new(name);
        if source_root.join(config_relative).try_exists().ok() != Some(true) {
            continue;
        }
        let config_path = contained_regular_file(source_root, config_relative)?;
        let contents = String::from_utf8(read_legacy_bytes(&config_path, MAX_LEGACY_STATE_BYTES)?)
            .map_err(|_| "legacy_pairing_invalid_layout")?;
        for line in contents.lines() {
            let line = line.split('#').next().unwrap_or_default().trim();
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
            if !is_normal_relative_path(relative) {
                return Err("legacy_pairing_invalid_layout");
            }
            if configured
                .insert(key, relative.to_path_buf())
                .is_some_and(|previous| previous != relative)
            {
                return Err("legacy_pairing_state_ambiguous");
            }
            if let Some(path) = optional_contained_regular_file(source_root, relative)?
                && !paths.contains(&path)
            {
                paths.push(path);
            }
        }
    }

    Ok(paths)
}

fn legacy_identity_paths(source_root: &Path) -> Result<(PathBuf, PathBuf), &'static str> {
    let source_root = source_root
        .canonicalize()
        .map_err(|_| "legacy_pairing_invalid_layout")?;
    let mut private_key = None;
    let mut certificate = None;
    for name in [
        "apollo.conf",
        "sunshine.conf",
        "vibeshine.conf",
        "vibepollo.conf",
    ] {
        let path = source_root.join(name);
        if !path
            .try_exists()
            .map_err(|_| "legacy_pairing_invalid_layout")?
        {
            continue;
        }
        let contents = String::from_utf8(read_legacy_bytes(&path, MAX_LEGACY_STATE_BYTES)?)
            .map_err(|_| "legacy_pairing_invalid_layout")?;
        for line in contents.lines() {
            let line = strip_unquoted_comment(line).trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let value = value.trim().trim_matches('"');
            let destination = match key.trim().to_ascii_lowercase().as_str() {
                "pkey" => &mut private_key,
                "cert" => &mut certificate,
                _ => continue,
            };
            let candidate = contained_regular_file(&source_root, Path::new(value))?;
            if destination
                .replace(candidate.clone())
                .is_some_and(|previous| previous != candidate)
            {
                return Err("legacy_pairing_identity_path_ambiguous");
            }
        }
    }
    let private_key = match private_key {
        Some(path) => path,
        None => contained_regular_file(&source_root, Path::new("credentials/cakey.pem"))?,
    };
    let certificate = match certificate {
        Some(path) => path,
        None => contained_regular_file(&source_root, Path::new("credentials/cacert.pem"))?,
    };
    Ok((certificate, private_key))
}

fn strip_unquoted_comment(line: &str) -> &str {
    let mut inside_double_quotes = false;
    let mut escaped = false;
    for (index, character) in line.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if inside_double_quotes && character == '\\' {
            escaped = true;
            continue;
        }
        match character {
            '"' => inside_double_quotes = !inside_double_quotes,
            '#' if !inside_double_quotes => return &line[..index],
            _ => {}
        }
    }
    line
}

fn contained_regular_file(source_root: &Path, relative: &Path) -> Result<PathBuf, &'static str> {
    if !is_normal_relative_path(relative) {
        return Err("legacy_pairing_invalid_layout");
    }
    let canonical = source_root
        .join(relative)
        .canonicalize()
        .map_err(|_| "legacy_pairing_invalid_layout")?;
    if !canonical.starts_with(source_root) {
        return Err("legacy_pairing_invalid_layout");
    }
    let metadata = std::fs::metadata(&canonical).map_err(|_| "legacy_pairing_invalid_layout")?;
    if !metadata.is_file() {
        return Err("legacy_pairing_invalid_layout");
    }
    Ok(canonical)
}

fn optional_contained_regular_file(
    source_root: &Path,
    relative: &Path,
) -> Result<Option<PathBuf>, &'static str> {
    if !is_normal_relative_path(relative) {
        return Err("legacy_pairing_invalid_layout");
    }
    let path = source_root.join(relative);
    match std::fs::symlink_metadata(&path) {
        Ok(_) => contained_regular_file(source_root, relative).map(Some),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut ancestor = path.parent();
            while let Some(candidate) = ancestor {
                match std::fs::symlink_metadata(candidate) {
                    Ok(_) => {
                        let canonical = candidate
                            .canonicalize()
                            .map_err(|_| "legacy_pairing_invalid_layout")?;
                        if !canonical.starts_with(source_root)
                            || !std::fs::metadata(canonical)
                                .map_err(|_| "legacy_pairing_invalid_layout")?
                                .is_dir()
                        {
                            return Err("legacy_pairing_invalid_layout");
                        }
                        return Ok(None);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        ancestor = candidate.parent();
                    }
                    Err(_) => return Err("legacy_pairing_invalid_layout"),
                }
            }
            Err("legacy_pairing_invalid_layout")
        }
        Err(_) => Err("legacy_pairing_invalid_layout"),
    }
}

fn read_legacy_file(path: &Path, maximum_bytes: u64) -> Result<String, &'static str> {
    let bytes = read_legacy_bytes(path, maximum_bytes)?;
    String::from_utf8(bytes).map_err(|_| "legacy_pairing_invalid_identity")
}

fn read_legacy_bytes(path: &Path, maximum_bytes: u64) -> Result<Vec<u8>, &'static str> {
    read_bounded(path, maximum_bytes).map_err(|_| "legacy_pairing_invalid_layout")
}

fn ensure_real_directory(path: &Path) -> Result<(), &'static str> {
    let metadata = std::fs::symlink_metadata(path).map_err(|_| "legacy_pairing_invalid_layout")?;
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        Ok(())
    } else {
        Err("legacy_pairing_invalid_layout")
    }
}

fn is_normal_relative_path(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

fn valid_identifier(value: &str, maximum: usize) -> bool {
    !value.is_empty()
        && value.len() <= maximum
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-:/".contains(&byte))
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StateNamespace {
    format_version: u32,
    revision: u64,
    values: BTreeMap<String, Value>,
}

impl Default for StateNamespace {
    fn default() -> Self {
        Self {
            format_version: STATE_FORMAT_VERSION,
            revision: 0,
            values: BTreeMap::new(),
        }
    }
}

fn read_namespace(path: &Path) -> Result<StateNamespace, &'static str> {
    if !path
        .try_exists()
        .map_err(|_| "legacy_pairing_state_unavailable")?
    {
        return Ok(StateNamespace::default());
    }
    let namespace: StateNamespace = serde_json::from_slice(
        &read_bounded(path, 1024 * 1024).map_err(|_| "legacy_pairing_state_unavailable")?,
    )
    .map_err(|_| "legacy_pairing_state_unavailable")?;
    if namespace.format_version != STATE_FORMAT_VERSION {
        return Err("legacy_pairing_state_unavailable");
    }
    Ok(namespace)
}

fn pairing_namespace_path(state_root: &Path) -> PathBuf {
    let encoded: String = PAIRING_PRINCIPAL
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let mut path = state_root.join("namespaces");
    for component in encoded.as_bytes().chunks(64) {
        path.push(std::str::from_utf8(component).expect("hex is ASCII"));
    }
    path.join("state.json")
}

fn create_namespace_directories(state_root: &Path, path: &Path) -> Result<(), &'static str> {
    let directory = path.parent().ok_or("legacy_pairing_state_unavailable")?;
    let relative = directory
        .strip_prefix(state_root)
        .map_err(|_| "legacy_pairing_state_unavailable")?;
    let mut current = state_root.to_path_buf();
    for component in relative.components() {
        if !matches!(component, Component::Normal(_)) {
            return Err("legacy_pairing_state_unavailable");
        }
        current.push(component);
        create_private_directory(&current).map_err(|_| "legacy_pairing_state_unavailable")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::Document;
    use std::sync::atomic::{AtomicU64, Ordering};
    use x509_cert::der::DecodePem;

    const HOST_CERT: &str = include_str!("../tests/fixtures/host-cert.pem");
    const HOST_KEY: &str = include_str!("../tests/fixtures/host-key.pem");
    const CLIENT_CERT: &str = include_str!("../tests/fixtures/client-cert.pem");

    struct TemporaryDirectory(PathBuf);

    impl TemporaryDirectory {
        fn new() -> Self {
            static SEQUENCE: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "amaterasu-pairing-import-{}-{}",
                std::process::id(),
                SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TemporaryDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn fixture_now() -> u64 {
        x509_cert::Certificate::from_pem(HOST_CERT)
            .unwrap()
            .tbs_certificate
            .validity
            .not_before
            .to_unix_duration()
            .as_secs()
            + 1
    }

    fn write_legacy_source(root: &Path, enabled: bool) {
        std::fs::create_dir_all(root.join("credentials")).unwrap();
        std::fs::write(root.join("credentials/cakey.pem"), HOST_KEY).unwrap();
        std::fs::write(root.join("credentials/cacert.pem"), HOST_CERT).unwrap();
        let state = serde_json::json!({
            "root": {
                "uniqueid": "12345678-1234-4234-8234-123456789abc",
                "named_devices": [{
                    "name": "Migrated device",
                    "uuid": "87654321-1234-4234-8234-123456789abc",
                    "cert": CLIENT_CERT,
                    "enabled": enabled,
                }]
            }
        });
        std::fs::write(
            root.join("sunshine_state.json"),
            serde_json::to_vec(&state).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn converts_current_legacy_identity_and_enabled_clients_into_module_document() {
        let fixture = TemporaryDirectory::new();
        write_legacy_source(&fixture.0, true);
        let imported = import_legacy_pairing(&fixture.0, fixture_now()).unwrap();

        assert_eq!(
            imported.document.identity.as_ref().unwrap().host_id,
            "12345678-1234-4234-8234-123456789abc"
        );
        assert_eq!(imported.document.clients.len(), 1);
        imported.document.validate(fixture_now()).unwrap();
    }

    #[test]
    fn rejects_a_host_key_that_does_not_own_the_legacy_certificate() {
        let fixture = TemporaryDirectory::new();
        write_legacy_source(&fixture.0, true);
        std::fs::write(fixture.0.join("credentials/cakey.pem"), CLIENT_CERT).unwrap();

        assert!(matches!(
            import_legacy_pairing(&fixture.0, fixture_now()),
            Err("legacy_pairing_invalid_identity")
        ));
    }

    #[test]
    fn imports_configured_identity_files_only_when_their_canonical_paths_stay_contained() {
        let fixture = TemporaryDirectory::new();
        write_legacy_source(&fixture.0, true);
        std::fs::create_dir_all(fixture.0.join("private")).unwrap();
        std::fs::write(fixture.0.join("private/host-key.pem"), HOST_KEY).unwrap();
        std::fs::write(fixture.0.join("private/host-cert.pem"), HOST_CERT).unwrap();
        std::fs::write(
            fixture.0.join("sunshine.conf"),
            "pkey = private/host-key.pem\ncert = private/host-cert.pem\n",
        )
        .unwrap();

        let imported = import_legacy_pairing(&fixture.0, fixture_now()).unwrap();
        imported.document.validate(fixture_now()).unwrap();

        std::fs::write(
            fixture.0.join("sunshine.conf"),
            "pkey = ../outside.pem\ncert = private/host-cert.pem\n",
        )
        .unwrap();
        assert!(matches!(
            import_legacy_pairing(&fixture.0, fixture_now()),
            Err("legacy_pairing_invalid_layout")
        ));
    }

    #[test]
    fn configured_identity_paths_keep_quoted_hashes_and_strip_trailing_comments() {
        let fixture = TemporaryDirectory::new();
        write_legacy_source(&fixture.0, true);
        std::fs::create_dir_all(fixture.0.join("private")).unwrap();
        let private_key_path = fixture.0.join("private/host#key.pem");
        let certificate_path = fixture.0.join("private/host#cert.pem");
        std::fs::write(&private_key_path, HOST_KEY).unwrap();
        std::fs::write(&certificate_path, HOST_CERT).unwrap();
        std::fs::write(
            fixture.0.join("sunshine.conf"),
            "pkey = \"private/host#key.pem\" # private key\ncert = \"private/host#cert.pem\" # certificate\n",
        )
        .unwrap();

        let imported = import_legacy_pairing(&fixture.0, fixture_now()).unwrap();
        let identity = imported.document.identity.unwrap();
        assert_eq!(identity.private_key_pem, HOST_KEY);
        assert_eq!(identity.certificate_pem, HOST_CERT);
        assert_eq!(
            std::fs::read(private_key_path).unwrap(),
            HOST_KEY.as_bytes()
        );
        assert_eq!(
            std::fs::read(certificate_path).unwrap(),
            HOST_CERT.as_bytes()
        );
    }

    #[test]
    fn imports_configured_state_files_only_when_their_canonical_paths_stay_contained() {
        let fixture = TemporaryDirectory::new();
        write_legacy_source(&fixture.0, true);
        let configured_state = fixture.0.join("private/custom_state.json");
        std::fs::create_dir_all(configured_state.parent().unwrap()).unwrap();
        std::fs::rename(fixture.0.join("sunshine_state.json"), &configured_state).unwrap();
        std::fs::write(
            fixture.0.join("sunshine.conf"),
            "file_state = private/custom_state.json # custom primary\nvibeshine_file_state = private/not_created_yet.json\n",
        )
        .unwrap();

        let imported = import_legacy_pairing(&fixture.0, fixture_now()).unwrap();
        assert_eq!(imported.document.clients.len(), 1);

        std::fs::write(
            fixture.0.join("sunshine.conf"),
            "file_state = ../outside_state.json\n",
        )
        .unwrap();
        assert!(matches!(
            import_legacy_pairing(&fixture.0, fixture_now()),
            Err("legacy_pairing_invalid_layout")
        ));

        let outside = fixture.0.parent().unwrap().join("outside_state.json");
        std::fs::write(&outside, std::fs::read(&configured_state).unwrap()).unwrap();
        std::fs::write(
            fixture.0.join("sunshine.conf"),
            format!("file_state = {}\n", outside.display()),
        )
        .unwrap();
        assert!(matches!(
            import_legacy_pairing(&fixture.0, fixture_now()),
            Err("legacy_pairing_invalid_layout")
        ));
    }

    #[test]
    fn conflicting_configured_state_paths_are_rejected() {
        let fixture = TemporaryDirectory::new();
        write_legacy_source(&fixture.0, true);
        let state_bytes = std::fs::read(fixture.0.join("sunshine_state.json")).unwrap();
        std::fs::write(fixture.0.join("sunshine_state.json"), b"{}").unwrap();
        std::fs::create_dir_all(fixture.0.join("private")).unwrap();
        std::fs::write(fixture.0.join("private/custom_state.json"), state_bytes).unwrap();
        std::fs::write(
            fixture.0.join("sunshine.conf"),
            "file_state = private/custom_state.json\n",
        )
        .unwrap();
        std::fs::write(
            fixture.0.join("vibeshine.conf"),
            "file_state = sunshine_state.json\n",
        )
        .unwrap();

        assert!(matches!(
            import_legacy_pairing(&fixture.0, fixture_now()),
            Err("legacy_pairing_state_ambiguous")
        ));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_a_missing_state_path_beneath_an_escaping_symlink() {
        use std::os::unix::fs::symlink;

        let fixture = TemporaryDirectory::new();
        write_legacy_source(&fixture.0, true);
        let outside = fixture.0.parent().unwrap().join("outside-state-directory");
        std::fs::create_dir_all(&outside).unwrap();
        symlink(&outside, fixture.0.join("external")).unwrap();
        std::fs::write(
            fixture.0.join("sunshine.conf"),
            "vibeshine_file_state = external/not_created_yet.json\n",
        )
        .unwrap();

        assert!(matches!(
            import_legacy_pairing(&fixture.0, fixture_now()),
            Err("legacy_pairing_invalid_layout")
        ));
        let _ = std::fs::remove_dir_all(outside);
    }

    #[test]
    fn disabled_and_ambiguous_legacy_pairings_do_not_expand_trust() {
        let fixture = TemporaryDirectory::new();
        write_legacy_source(&fixture.0, false);
        let imported = import_legacy_pairing(&fixture.0, fixture_now()).unwrap();
        assert!(imported.document.clients.is_empty());

        let second_state = serde_json::json!({
            "root": {
                "uniqueid": "12345678-1234-4234-8234-123456789abc",
                "named_devices": []
            }
        });
        std::fs::write(
            fixture.0.join("vibeshine_state.json"),
            serde_json::to_vec(&second_state).unwrap(),
        )
        .unwrap();
        assert!(matches!(
            import_legacy_pairing(&fixture.0, fixture_now()),
            Err("legacy_pairing_state_ambiguous")
        ));
    }

    #[test]
    fn seeder_writes_the_state_module_namespace_once_and_preserves_existing_values() {
        let source = TemporaryDirectory::new();
        let state = TemporaryDirectory::new();
        write_legacy_source(&source.0, true);
        let imported = import_legacy_pairing(&source.0, fixture_now()).unwrap();

        assert_eq!(
            seed_pairing_state(&state.0, &imported).unwrap(),
            PairingSeedOutcome::Seeded
        );
        let path = pairing_namespace_path(&state.0);
        let namespace: StateNamespace =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(namespace.format_version, STATE_FORMAT_VERSION);
        assert_eq!(namespace.revision, 1);
        let document: Document =
            serde_json::from_value(namespace.values[PAIRING_STATE_KEY].clone()).unwrap();
        document.validate(fixture_now()).unwrap();
        assert_eq!(
            seed_pairing_state(&state.0, &imported).unwrap(),
            PairingSeedOutcome::ExistingAmaterasuState
        );
        let reimported = import_legacy_pairing(&source.0, fixture_now()).unwrap();
        assert_eq!(
            seed_pairing_state(&state.0, &reimported).unwrap(),
            PairingSeedOutcome::ExistingAmaterasuState
        );
        let unchanged: StateNamespace =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(unchanged.revision, 1);
    }
}
