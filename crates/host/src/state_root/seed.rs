//! User-token application of a staged migration snapshot.

use super::*;
use amaterasu_package::{RootLock, write_json_atomically};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// Runs only in the interactive user's token, before any module can load state.
pub(crate) fn apply_pending_seed(seed: &Path, destination: &Path) -> PackageResult<()> {
    let destination = canonical_real_directory(destination, "user state root")?;
    let _lock = RootLock::acquire(&destination)?;
    let marker = destination.join(".migration-seed-applied");
    if marker.try_exists().map_err(|error| error.to_string())? {
        let metadata = fs::symlink_metadata(&marker).map_err(|error| error.to_string())?;
        if !metadata.is_file() {
            return Err("migration completion marker is invalid".into());
        }
        reject_reparse(&marker)?;
        return Ok(());
    }
    let seed = canonical_real_directory(seed, "staged migration seed")?;
    let mut token = std::ptr::null_mut();
    // SAFETY: the current-process pseudo handle is valid and Windows writes one owned token.
    if unsafe { OpenProcessToken(GetCurrentProcess(), 8, &mut token) } == 0 {
        return Err("interactive user token is unavailable".into());
    }
    let sid = sid_string(token);
    // SAFETY: OpenProcessToken returned this owned token handle.
    unsafe { CloseHandle(token) };
    merge_missing_tree(&seed, &destination, &sid?, false, true)?;
    write_seed_owner(&marker, "S-1-5-18")
}

/// Replays an interrupted import into an already owned user root. Existing
/// Amaterasu data, especially a newer administrator verifier, always wins.
fn merge_missing_tree(
    source: &Path,
    destination: &Path,
    sid: &str,
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
            merge_missing_tree(&source_path, &destination_path, sid, namespace_tree, false)?;
        } else if source_metadata.is_file() {
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
