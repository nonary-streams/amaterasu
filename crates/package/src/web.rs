//! Independently signed static web bundles. A pointer replacement changes the UI
//! without changing the native component selection or requiring a host restart.
mod publication;
pub use publication::{
    prepare_bundle, restore_previous, restore_version, stage_bundle, stage_repository_bundle,
};

use crate::{
    PackageResult, ReleaseSignature, decode_hex, read_bounded, read_json, sha256,
    verify_detached_signature,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const MAX_ASSET_BYTES: u64 = 4 * 1024 * 1024;
pub const MAX_BUNDLE_BYTES: u64 = 16 * 1024 * 1024;
pub const MAX_FILES: usize = 128;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Asset {
    pub sha256: String,
    pub size: u64,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub schema: u32,
    pub project: String,
    pub sequence: u64,
    pub expires_unix: u64,
    pub files: BTreeMap<String, Asset>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Selection {
    pub digest: String,
    pub highest_sequence: u64,
    #[serde(default)]
    pub active_sequence: Option<u64>,
    #[serde(default)]
    pub previous_digest: Option<String>,
    #[serde(default)]
    pub publisher_floors: BTreeMap<String, PublisherFloor>,
    #[serde(default)]
    pub project_floor: Option<PublisherFloor>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublisherFloor {
    pub sequence: u64,
    pub digest: String,
}

pub struct VerifiedBundle {
    pub digest: String,
    pub manifest: Manifest,
    pub assets: BTreeMap<String, Vec<u8>>,
}

/// Read every verified asset into owned memory before exposing any of the bundle.
/// Later file replacement cannot substitute bytes in the already verified cache.
pub fn verify_bundle(
    directory: &Path,
    keys: &BTreeMap<String, String>,
    now: u64,
) -> PackageResult<VerifiedBundle> {
    let raw = read_bounded(&safe_path(directory, "manifest.json")?, 65_536)?;
    let signature: ReleaseSignature = read_json(&safe_path(directory, "manifest.sig")?)?;
    verify_detached_signature(keys, &raw, &signature)?;
    let manifest: Manifest = serde_json::from_slice(&raw).map_err(|error| error.to_string())?;
    validate_manifest(&manifest, now)?;
    let mut assets = BTreeMap::new();
    for (name, asset) in &manifest.files {
        let bytes = read_bounded(&safe_path(directory, name)?, MAX_ASSET_BYTES)?;
        if bytes.len() as u64 != asset.size || sha256(&bytes) != asset.sha256 {
            return Err("web asset integrity verification failed".into());
        }
        assets.insert(name.clone(), bytes);
    }
    Ok(VerifiedBundle {
        digest: sha256(&raw),
        manifest,
        assets,
    })
}

pub fn read_selection(root: &Path) -> PackageResult<Selection> {
    let bytes = read_bounded(&safe_path(root, "selection.json")?, 4096)?;
    let selection: Selection = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    decode_hex::<32>(&selection.digest)?;
    if selection.digest != selection.digest.to_ascii_lowercase() {
        return Err("invalid web selection digest".into());
    }
    if let Some(previous) = &selection.previous_digest {
        decode_hex::<32>(previous)?;
        if previous != &previous.to_ascii_lowercase() || previous == &selection.digest {
            return Err("invalid previous web selection digest".into());
        }
    }
    if selection
        .active_sequence
        .is_some_and(|active| active > selection.highest_sequence)
    {
        return Err("web active sequence exceeds accepted sequence".into());
    }
    for (fingerprint, floor) in &selection.publisher_floors {
        decode_hex::<32>(fingerprint)?;
        decode_hex::<32>(&floor.digest)?;
        if fingerprint != &fingerprint.to_ascii_lowercase()
            || floor.digest != floor.digest.to_ascii_lowercase()
        {
            return Err("invalid publisher web release floor".into());
        }
    }
    if let Some(floor) = &selection.project_floor {
        decode_hex::<32>(&floor.digest)?;
        if floor.digest != floor.digest.to_ascii_lowercase() {
            return Err("invalid project web release floor".into());
        }
    }
    Ok(selection)
}

pub fn load_selected(
    root: &Path,
    keys: &BTreeMap<String, String>,
    now: u64,
) -> PackageResult<VerifiedBundle> {
    let selection = read_selection(root)?;
    let directory = safe_directory(root, &format!("bundles/{}", selection.digest))?;
    let bundle = verify_bundle(&directory, keys, now)?;
    if bundle.digest != selection.digest
        || bundle.manifest.sequence
            != selection
                .active_sequence
                .unwrap_or(selection.highest_sequence)
    {
        return Err("web selection does not match signed bundle".into());
    }
    Ok(bundle)
}

pub fn load_version(
    root: &Path,
    digest: &str,
    keys: &BTreeMap<String, String>,
    now: u64,
) -> PackageResult<VerifiedBundle> {
    decode_hex::<32>(digest)?;
    if digest != digest.to_ascii_lowercase() {
        return Err("invalid web bundle digest".into());
    }
    let directory = safe_directory(root, &format!("bundles/{digest}"))?;
    let bundle = verify_bundle(&directory, keys, now)?;
    if bundle.digest != digest {
        return Err("web bundle digest mismatch".into());
    }
    Ok(bundle)
}

fn validate_manifest(manifest: &Manifest, now: u64) -> PackageResult<()> {
    if manifest.schema != 1 || manifest.project != "amaterasu.web" || manifest.expires_unix <= now {
        return Err("invalid or expired web release metadata".into());
    }
    if manifest.files.is_empty()
        || manifest.files.len() > MAX_FILES
        || !manifest.files.contains_key("index.html")
    {
        return Err("web bundle needs index.html and at most 128 assets".into());
    }
    let mut total = 0;
    for (name, asset) in &manifest.files {
        validate_asset_name(name)?;
        decode_hex::<32>(&asset.sha256)?;
        if asset.size > MAX_ASSET_BYTES {
            return Err("web asset exceeds 4 MiB".into());
        }
        total += asset.size;
    }
    if total > MAX_BUNDLE_BYTES {
        return Err("web bundle exceeds 16 MiB".into());
    }
    Ok(())
}

pub fn content_type(name: &str) -> Option<&'static str> {
    match name.rsplit_once('.')?.1 {
        "html" => Some("text/html; charset=utf-8"),
        "js" => Some("text/javascript; charset=utf-8"),
        "css" => Some("text/css; charset=utf-8"),
        "svg" => Some("image/svg+xml"),
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "webp" => Some("image/webp"),
        "ico" => Some("image/x-icon"),
        "woff2" => Some("font/woff2"),
        _ => None,
    }
}

pub fn validate_asset_name(name: &str) -> PackageResult<()> {
    if name.len() > 160
        || name.starts_with('/')
        || name.starts_with("api/")
        || name.starts_with("v1/")
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-._/".contains(&byte))
        || name
            .split('/')
            .any(|part| part.is_empty() || part.starts_with('.'))
        || content_type(name).is_none()
    {
        return Err("invalid static asset path or extension".into());
    }
    Ok(())
}

pub(super) fn safe_directory(root: &Path, relative: &str) -> PackageResult<PathBuf> {
    let mut path = root.to_path_buf();
    require_directory(&path)?;
    for part in relative.split('/').filter(|part| !part.is_empty()) {
        if part == "." || part == ".." || part.contains('\\') {
            return Err("invalid web directory".into());
        }
        path.push(part);
        require_directory(&path)?;
    }
    Ok(path)
}

fn require_directory(path: &Path) -> PackageResult<()> {
    let metadata = std::fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err("web directory must be a real directory".into());
    }
    Ok(())
}

fn safe_path(root: &Path, relative: &str) -> PackageResult<PathBuf> {
    let (directory, name) = relative.rsplit_once('/').unwrap_or(("", relative));
    Ok(safe_directory(root, directory)?.join(name))
}

#[cfg(test)]
mod tests;
