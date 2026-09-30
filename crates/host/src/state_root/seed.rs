//! User-token application of a staged migration snapshot.

use super::*;
use amaterasu_package::{RootLock, sha256, sha256_reader, write_json_atomically};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    io::Write,
    path::{Component, PathBuf},
};

const PAIRING_PRINCIPAL: &str = "amaterasu.pairing";
const PAIRING_STATE_KEY: &str = "pairing/v1";
const HOST_CONFIGURATION_PRINCIPAL: &str = "amaterasu.host-configuration";
const HOST_CONFIGURATION_STATE_KEY: &str = "global/amaterasu.host-configuration";
const DEFAULT_CERTIFICATE_PATH: &str = "credentials/cacert.pem";
const DEFAULT_PRIVATE_KEY_PATH: &str = "credentials/cakey.pem";

/// Runs only in the interactive user's token, before any module can load state.
pub(crate) fn apply_pending_seed(seed: &Path, destination: &Path) -> PackageResult<()> {
    let destination = canonical_real_directory(destination, "user state root")?;
    let _lock = RootLock::acquire(&destination)?;
    let marker = destination.join(".migration-seed-applied");
    let previous = match fs::symlink_metadata(&marker) {
        Ok(_) => Some(read_applied_seed(&marker)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.to_string()),
    };
    let seed = canonical_real_directory(seed, "staged migration seed")?;
    let fingerprint = seed_fingerprint(&seed)?;
    let already_applied = previous.as_deref() == Some(fingerprint.as_str());
    let seeded_pairing_value = namespace_value(&seed, PAIRING_PRINCIPAL, PAIRING_STATE_KEY)?;
    let destination_pairing_value =
        namespace_value(&destination, PAIRING_PRINCIPAL, PAIRING_STATE_KEY)?;
    let pairing_seed_wins = seeded_pairing_value.as_ref().is_some_and(|seeded| {
        destination_pairing_value
            .as_ref()
            .is_none_or(|destination| destination == seeded)
    });
    let imported_identity = if pairing_seed_wins {
        seeded_pairing_value
            .as_ref()
            .map(imported_pairing_identity)
            .transpose()?
            .flatten()
    } else {
        None
    };
    let seed_identity_paths_to_skip = if !pairing_seed_wins
        && seeded_pairing_value
            .as_ref()
            .map(imported_pairing_identity)
            .transpose()?
            .flatten()
            .is_some()
    {
        let (certificate, private_key) = configured_identity_paths(&seed)?;
        vec![certificate, private_key]
    } else {
        Vec::new()
    };
    let mut token = std::ptr::null_mut();
    // SAFETY: the current-process pseudo handle is valid and Windows writes one owned token.
    if unsafe { OpenProcessToken(GetCurrentProcess(), 8, &mut token) } == 0 {
        return Err("interactive user token is unavailable".into());
    }
    let sid = sid_string(token);
    // SAFETY: OpenProcessToken returned this owned token handle.
    unsafe { CloseHandle(token) };
    let sid = sid?;
    if !already_applied || (pairing_seed_wins && destination_pairing_value.is_none()) {
        merge_missing_tree(
            &seed,
            &destination,
            &sid,
            &seed_identity_paths_to_skip,
            false,
            true,
        )?;
    }
    if let (Some(expected_pairing), Some(identity)) =
        (seeded_pairing_value.as_ref(), imported_identity)
    {
        if namespace_value(&destination, PAIRING_PRINCIPAL, PAIRING_STATE_KEY)?.as_ref()
            != Some(expected_pairing)
        {
            return Err("imported pairing state did not win the user-state merge".into());
        }
        materialize_imported_identity(&destination, &sid, &identity)?;
    }
    if already_applied {
        return Ok(());
    }
    write_json_atomically(
        &marker,
        &AppliedSeed {
            version: 1,
            fingerprint,
        },
    )
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct AppliedSeed {
    version: u32,
    fingerprint: String,
}

fn read_applied_seed(marker: &Path) -> PackageResult<String> {
    let metadata = fs::symlink_metadata(marker).map_err(|error| error.to_string())?;
    if !metadata.is_file() || metadata.len() > 256 {
        return Err("migration completion marker is invalid".into());
    }
    reject_reparse(marker)?;
    let bytes = fs::read(marker).map_err(|error| error.to_string())?;
    // Older installations wrote the SYSTEM SID after the first successful
    // merge. Replay that snapshot once to adopt the versioned marker.
    if bytes == b"S-1-5-18" {
        return Ok(String::new());
    }
    let applied: AppliedSeed =
        serde_json::from_slice(&bytes).map_err(|_| "migration completion marker is invalid")?;
    if applied.version != 1
        || applied.fingerprint.len() != 64
        || !applied
            .fingerprint
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err("migration completion marker is invalid".into());
    }
    Ok(applied.fingerprint)
}

fn seed_fingerprint(seed: &Path) -> PackageResult<String> {
    let mut entries = Vec::new();
    fingerprint_tree(seed, seed, &mut entries)?;
    let bytes = serde_json::to_vec(&entries).map_err(|error| error.to_string())?;
    Ok(sha256(&bytes))
}

fn fingerprint_tree(
    root: &Path,
    directory: &Path,
    entries: &mut Vec<(Vec<u16>, Option<String>)>,
) -> PackageResult<()> {
    let mut children = fs::read_dir(directory)
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    children.sort_by_key(|entry| entry.file_name());
    for entry in children {
        if directory == root && entry.file_name() == "host.lock" {
            continue;
        }
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path).map_err(|error| error.to_string())?;
        reject_reparse(&path)?;
        let relative: Vec<u16> = path
            .strip_prefix(root)
            .map_err(|error| error.to_string())?
            .as_os_str()
            .encode_wide()
            .collect();
        if metadata.is_dir() {
            entries.push((relative, None));
            fingerprint_tree(root, &path, entries)?;
        } else if metadata.is_file() {
            entries.push((
                relative,
                Some(sha256_reader(
                    fs::File::open(&path).map_err(|error| error.to_string())?,
                )?),
            ));
        } else {
            return Err("migration seed contains a non-regular entry".into());
        }
    }
    Ok(())
}

/// Replays an interrupted import into an already owned user root. Existing
/// Amaterasu data, especially a newer administrator verifier, always wins.
fn merge_missing_tree(
    source: &Path,
    destination: &Path,
    sid: &str,
    skipped_identity_files: &[PathBuf],
    namespace_tree: bool,
    root_level: bool,
) -> PackageResult<()> {
    set_user_dacl(destination, sid)?;
    for entry in fs::read_dir(source).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        // Each root owns its own live lock file; it is never imported state.
        if root_level && entry.file_name() == "host.lock" {
            continue;
        }
        let source_path = entry.path();
        let source_metadata =
            fs::symlink_metadata(&source_path).map_err(|error| error.to_string())?;
        reject_reparse(&source_path)?;
        let destination_path = destination.join(entry.file_name());
        if source_metadata.is_dir() {
            if destination_path
                .try_exists()
                .map_err(|error| error.to_string())?
            {
                canonical_real_directory(&destination_path, "existing user state directory")?;
            } else {
                create_real_directory(&destination_path)?;
            }
            let namespace_tree = namespace_tree || entry.file_name() == "namespaces";
            merge_missing_tree(
                &source_path,
                &destination_path,
                sid,
                skipped_identity_files,
                namespace_tree,
                false,
            )?;
        } else if source_metadata.is_file() {
            if skipped_identity_files
                .iter()
                .any(|identity_path| same_windows_path(identity_path, &source_path))
            {
                continue;
            }
            if destination_path
                .try_exists()
                .map_err(|error| error.to_string())?
            {
                let metadata =
                    fs::symlink_metadata(&destination_path).map_err(|error| error.to_string())?;
                reject_reparse(&destination_path)?;
                if !metadata.is_file() {
                    return Err("existing user state contains a non-regular entry".into());
                }
                if namespace_tree && entry.file_name() == "state.json" {
                    merge_namespace(&source_path, &destination_path)?;
                }
                set_user_dacl(&destination_path, sid)?;
                continue;
            }
            let temporary = destination.join(format!(".seed-{}", random_identifier()?));
            let result = fs::copy(&source_path, &temporary)
                .and_then(|_| fs::rename(&temporary, &destination_path))
                .map_err(|error| error.to_string());
            if result.is_err() {
                let _ = fs::remove_file(&temporary);
            }
            result?;
            set_user_dacl(&destination_path, sid)?;
        } else {
            return Err("migration seed contains a non-regular entry".into());
        }
    }
    Ok(())
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SeedNamespace {
    pub(super) format_version: u32,
    pub(super) revision: u64,
    pub(super) values: BTreeMap<String, Value>,
}

const MAX_NAMESPACE_BYTES: usize = 1_048_576;

fn validate_seed_namespace(namespace: &SeedNamespace) -> PackageResult<()> {
    if namespace.values.len() > 4096 {
        return Err("migration namespace exceeds the key limit".into());
    }
    for (key, value) in &namespace.values {
        if key.is_empty() || key.len() > 256 || key.chars().any(char::is_control) {
            return Err("migration namespace contains an invalid key".into());
        }
        if serde_json::to_vec(value)
            .map_err(|error| error.to_string())?
            .len()
            > 16_384
        {
            return Err("migration namespace contains an oversized value".into());
        }
    }
    if serde_json::to_vec_pretty(namespace)
        .map_err(|error| error.to_string())?
        .len()
        > MAX_NAMESPACE_BYTES
    {
        return Err("migration namespace exceeds the size limit".into());
    }
    Ok(())
}

pub(super) fn read_seed_namespace(path: &Path) -> PackageResult<SeedNamespace> {
    if fs::metadata(path).map_err(|error| error.to_string())?.len() > MAX_NAMESPACE_BYTES as u64 {
        return Err("migration namespace exceeds the size limit".into());
    }
    let namespace: SeedNamespace =
        serde_json::from_slice(&fs::read(path).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
    if namespace.format_version != 1 {
        return Err("migration namespace format is unsupported".into());
    }
    validate_seed_namespace(&namespace)?;
    Ok(namespace)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ImportedPairingDocument {
    identity: Option<ImportedPairingIdentity>,
    #[allow(dead_code)]
    clients: Vec<Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ImportedPairingIdentity {
    #[allow(dead_code)]
    host_id: String,
    certificate_pem: String,
    private_key_pem: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredHostConfiguration {
    version: u32,
    #[allow(dead_code)]
    revision: u64,
    #[allow(dead_code)]
    file_state: String,
    #[allow(dead_code)]
    vibeshine_file_state: String,
    #[serde(default)]
    #[allow(dead_code)]
    credentials_file: String,
    pkey: String,
    cert: String,
    #[allow(dead_code)]
    log_path: String,
    min_log_level: String,
}

fn imported_pairing_identity(value: &Value) -> PackageResult<Option<ImportedPairingIdentity>> {
    let document: ImportedPairingDocument = serde_json::from_value(value.clone())
        .map_err(|_| "migration pairing state is invalid".to_owned())?;
    let Some(identity) = document.identity else {
        return Ok(None);
    };
    if identity.host_id.is_empty()
        || identity.certificate_pem.is_empty()
        || identity.private_key_pem.is_empty()
        || identity.certificate_pem.len() > 4096
        || identity.private_key_pem.len() > 4096
    {
        return Err("migration pairing identity is invalid".into());
    }
    Ok(Some(identity))
}

fn namespace_value(state_root: &Path, principal: &str, key: &str) -> PackageResult<Option<Value>> {
    let path = namespace_path(state_root, principal);
    match safe_existing_file(&path)? {
        false => Ok(None),
        true => {
            let namespace = read_seed_namespace(&path)?;
            Ok(namespace.values.get(key).cloned())
        }
    }
}

fn safe_existing_file(path: &Path) -> PackageResult<bool> {
    if !path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        return Err("migration namespace path is unsafe".into());
    }
    let components: Vec<_> = path.components().collect();
    let mut current = PathBuf::new();
    for (index, component) in components.iter().enumerate() {
        match component {
            Component::Prefix(prefix) => current.push(prefix.as_os_str()),
            Component::RootDir => current.push(component.as_os_str()),
            Component::Normal(name) => current.push(name),
            Component::CurDir | Component::ParentDir => {
                return Err("migration namespace path is unsafe".into());
            }
        }
        if matches!(component, Component::Prefix(_)) {
            continue;
        }
        let is_target = index + 1 == components.len();
        let metadata = match fs::symlink_metadata(&current) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error.to_string()),
        };
        reject_reparse(&current)?;
        if is_target {
            if !metadata.is_file() {
                return Err("migration namespace is not a regular file".into());
            }
        } else if !metadata.is_dir() {
            return Err("migration namespace parent is not a directory".into());
        }
    }
    Ok(true)
}

fn namespace_path(state_root: &Path, principal: &str) -> PathBuf {
    let encoded: String = principal
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

fn materialize_imported_identity(
    state_root: &Path,
    sid: &str,
    identity: &ImportedPairingIdentity,
) -> PackageResult<()> {
    let (certificate_path, private_key_path) = configured_identity_paths(state_root)?;
    materialize_identity_files(
        sid,
        &certificate_path,
        identity.certificate_pem.as_bytes(),
        &private_key_path,
        identity.private_key_pem.as_bytes(),
    )
}

fn configured_identity_paths(state_root: &Path) -> PackageResult<(PathBuf, PathBuf)> {
    let host_settings = namespace_value(
        state_root,
        HOST_CONFIGURATION_PRINCIPAL,
        HOST_CONFIGURATION_STATE_KEY,
    )?;
    let (certificate_path, private_key_path) = if let Some(value) = host_settings {
        let settings: StoredHostConfiguration = serde_json::from_value(value)
            .map_err(|_| "host configuration state is invalid".to_owned())?;
        if settings.version != 1
            || !matches!(
                settings.min_log_level.as_str(),
                "product_default"
                    | "verbose"
                    | "debug"
                    | "info"
                    | "warning"
                    | "error"
                    | "fatal"
                    | "none"
            )
        {
            return Err("host configuration state is invalid".into());
        }
        (
            configured_identity_path(state_root, &settings.cert)?,
            configured_identity_path(state_root, &settings.pkey)?,
        )
    } else {
        (
            configured_identity_path(state_root, DEFAULT_CERTIFICATE_PATH)?,
            configured_identity_path(state_root, DEFAULT_PRIVATE_KEY_PATH)?,
        )
    };
    if same_windows_path(&certificate_path, &private_key_path) {
        return Err("configured TLS identity paths conflict".into());
    }
    Ok((certificate_path, private_key_path))
}

fn configured_identity_path(state_root: &Path, configured: &str) -> PackageResult<PathBuf> {
    let path = Path::new(configured);
    if configured.is_empty()
        || configured.len() > 4096
        || configured.chars().any(char::is_control)
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
        || (!path.is_absolute()
            && path
                .components()
                .all(|component| matches!(component, Component::CurDir)))
    {
        return Err("configured TLS identity path is invalid".into());
    }
    let mut resolved = if path.is_absolute() {
        PathBuf::new()
    } else {
        state_root.to_path_buf()
    };
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => resolved.push(prefix.as_os_str()),
            Component::RootDir => resolved.push(component.as_os_str()),
            Component::Normal(name) => resolved.push(name),
            Component::CurDir => (),
            Component::ParentDir => {
                return Err("configured TLS identity path is invalid".into());
            }
        }
    }
    if !resolved.is_absolute() {
        return Err("configured TLS identity path is invalid".into());
    }
    Ok(resolved)
}

fn materialize_identity_files(
    sid: &str,
    certificate_path: &Path,
    certificate_bytes: &[u8],
    private_key_path: &Path,
    private_key_bytes: &[u8],
) -> PackageResult<()> {
    let certificate_exists = inspect_identity_file(certificate_path)?;
    let private_key_exists = inspect_identity_file(private_key_path)?;
    match (certificate_exists, private_key_exists) {
        (true, true) => {
            let certificate = read_identity_file(certificate_path)?;
            let private_key = read_identity_file(private_key_path)?;
            if certificate != certificate_bytes || private_key != private_key_bytes {
                return Err("configured TLS identity conflicts with migrated pairing".into());
            }
            set_user_dacl(certificate_path, sid)?;
            set_user_dacl(private_key_path, sid)
        }
        (false, false) => {
            ensure_identity_parent(certificate_path, sid)?;
            ensure_identity_parent(private_key_path, sid)?;
            if inspect_identity_file(certificate_path)? || inspect_identity_file(private_key_path)?
            {
                return Err("configured TLS identity changed during migration".into());
            }
            let certificate = create_identity_file(certificate_path, certificate_bytes, sid)?;
            match create_identity_file(private_key_path, private_key_bytes, sid) {
                Ok(private_key) => {
                    drop(private_key);
                    drop(certificate);
                    Ok(())
                }
                Err(error) => {
                    remove_created_identity_file(certificate_path, certificate);
                    Err(error)
                }
            }
        }
        _ => Err("configured TLS identity is incomplete".into()),
    }
}

fn inspect_identity_file(path: &Path) -> PackageResult<bool> {
    if !path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        return Err("configured TLS identity path is unsafe".into());
    }
    let components: Vec<_> = path.components().collect();
    let mut current = PathBuf::new();
    for (index, component) in components.iter().enumerate() {
        match component {
            Component::Prefix(prefix) => current.push(prefix.as_os_str()),
            Component::RootDir => current.push(component.as_os_str()),
            Component::Normal(name) => current.push(name),
            Component::CurDir | Component::ParentDir => {
                return Err("configured TLS identity path is unsafe".into());
            }
        }
        if matches!(component, Component::Prefix(_)) {
            continue;
        }
        let is_target = index + 1 == components.len();
        let metadata = match fs::symlink_metadata(&current) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error.to_string()),
        };
        reject_reparse(&current)?;
        if is_target {
            if !metadata.is_file() {
                return Err("configured TLS identity target is not a regular file".into());
            }
        } else if !metadata.is_dir() {
            return Err("configured TLS identity parent is not a directory".into());
        }
    }
    Ok(true)
}

fn ensure_identity_parent(path: &Path, sid: &str) -> PackageResult<()> {
    let parent = path
        .parent()
        .ok_or_else(|| "configured TLS identity has no parent".to_owned())?;
    let components: Vec<_> = parent.components().collect();
    let mut current = PathBuf::new();
    for component in components {
        match component {
            Component::Prefix(prefix) => current.push(prefix.as_os_str()),
            Component::RootDir => current.push(component.as_os_str()),
            Component::Normal(name) => {
                current.push(name);
                match fs::symlink_metadata(&current) {
                    Ok(metadata) => {
                        reject_reparse(&current)?;
                        if !metadata.is_dir() {
                            return Err("configured TLS identity parent is not a directory".into());
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        create_real_directory(&current)?;
                        set_user_dacl(&current, sid)?;
                    }
                    Err(error) => return Err(error.to_string()),
                }
            }
            Component::CurDir | Component::ParentDir => {
                return Err("configured TLS identity path is unsafe".into());
            }
        }
    }
    Ok(())
}

fn read_identity_file(path: &Path) -> PackageResult<Vec<u8>> {
    reject_reparse(path)?;
    let bytes = amaterasu_package::read_bounded(path, 4096)?;
    if bytes.is_empty() {
        return Err("configured TLS identity file is empty".into());
    }
    Ok(bytes)
}

fn create_identity_file(path: &Path, bytes: &[u8], sid: &str) -> PackageResult<File> {
    use std::os::windows::fs::OpenOptionsExt;

    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    let mut options = OpenOptions::new();
    options
        .write(true)
        .create_new(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    let mut file = options.open(path).map_err(|error| error.to_string())?;
    if file
        .write_all(bytes)
        .and_then(|()| file.sync_all())
        .is_err()
    {
        remove_created_identity_file(path, file);
        return Err("migrated TLS identity could not be written".into());
    }
    if let Err(error) = set_user_dacl(path, sid) {
        remove_created_identity_file(path, file);
        return Err(error);
    }
    Ok(file)
}

fn remove_created_identity_file(path: &Path, file: File) {
    use std::os::windows::fs::MetadataExt;

    let Ok(created) = file.metadata() else {
        return;
    };
    drop(file);
    let Ok(current) = fs::symlink_metadata(path) else {
        return;
    };
    if reject_reparse(path).is_err()
        || created.volume_serial_number().is_none()
        || created.volume_serial_number() != current.volume_serial_number()
        || created.file_index().is_none()
        || created.file_index() != current.file_index()
    {
        return;
    }
    let _ = fs::remove_file(path);
}

fn merge_namespace(source: &Path, destination: &Path) -> PackageResult<()> {
    let seed = read_seed_namespace(source)?;
    let mut current = read_seed_namespace(destination)?;
    let mut changed = false;
    for (key, value) in seed.values {
        if let std::collections::btree_map::Entry::Vacant(entry) = current.values.entry(key) {
            entry.insert(value);
            changed = true;
        }
    }
    if changed {
        current.revision = current
            .revision
            .checked_add(1)
            .ok_or("migration namespace revision exhausted")?;
        validate_seed_namespace(&current)?;
        write_json_atomically(destination, &current)?;
    }
    Ok(())
}
