use super::*;
use crate::{RootLock, create_private_directory, random_identifier, write_json_atomically};
use std::fs;

/// Builds an unsigned manifest and copies the listed bytes. The publisher signs
/// the final manifest; staging refuses unsigned bundles even in native developer mode.
pub fn prepare_bundle(
    source: &Path,
    output: &Path,
    sequence: u64,
    expires_unix: u64,
) -> PackageResult<Manifest> {
    if output.try_exists().map_err(|error| error.to_string())? {
        return Err("web output already exists".into());
    }
    let mut files = BTreeMap::new();
    collect(source, "", &mut files, &mut 0)?;
    let manifest = Manifest {
        schema: 1,
        project: "amaterasu.web".into(),
        sequence,
        expires_unix,
        files,
    };
    validate_manifest(&manifest, 0)?;
    create_private_directory(output)?;
    for (name, asset) in &manifest.files {
        let bytes = read_bounded(&safe_path(source, name)?, MAX_ASSET_BYTES)?;
        if sha256(&bytes) != asset.sha256 || bytes.len() as u64 != asset.size {
            return Err("web source changed while packaging".into());
        }
        write_asset(output, name, &bytes)?;
    }
    Ok(manifest)
}

fn collect(
    root: &Path,
    relative: &str,
    files: &mut BTreeMap<String, Asset>,
    entries: &mut usize,
) -> PackageResult<()> {
    let directory = safe_directory(root, relative)?;
    for entry in fs::read_dir(directory).map_err(|error| error.to_string())? {
        *entries += 1;
        if *entries > 256 {
            return Err("too many web directory entries".into());
        }
        let entry = entry.map_err(|error| error.to_string())?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| "web filename is not UTF-8")?;
        let name = if relative.is_empty() {
            name
        } else {
            format!("{relative}/{name}")
        };
        let kind = entry.file_type().map_err(|error| error.to_string())?;
        if kind.is_dir() {
            if name.len() > 150
                || name.split('/').count() > 8
                || name.split('/').any(|part| part.starts_with('.'))
            {
                return Err("web directory depth or path exceeds bounds".into());
            }
            collect(root, &name, files, entries)?;
        } else if kind.is_file() {
            validate_asset_name(&name)?;
            if files.len() >= MAX_FILES {
                return Err("too many web assets".into());
            }
            let bytes = read_bounded(&entry.path(), MAX_ASSET_BYTES)?;
            files.insert(
                name,
                Asset {
                    sha256: sha256(&bytes),
                    size: bytes.len() as u64,
                },
            );
        } else {
            return Err("web bundle cannot contain links or special files".into());
        }
    }
    Ok(())
}

fn write_asset(root: &Path, name: &str, bytes: &[u8]) -> PackageResult<()> {
    let destination = root.join(name);
    create_private_directory(destination.parent().ok_or("asset parent missing")?)?;
    amaterasu_storage::write_atomically(&destination, bytes)
}

pub fn stage_bundle(
    source: &Path,
    root: &Path,
    keys: &BTreeMap<String, String>,
    now: u64,
) -> PackageResult<String> {
    create_private_directory(root)?;
    let _lock = RootLock::acquire(root)?;
    let verified = verify_bundle(source, keys, now)?;
    check_progression(root, &verified)?;
    let bundles = root.join("bundles");
    create_private_directory(&bundles)?;
    require_directory(&bundles)?;
    let destination = bundles.join(&verified.digest);
    if destination.exists() {
        let existing = verify_bundle(&destination, keys, now)?;
        if existing.digest != verified.digest {
            return Err("existing web bundle mismatch".into());
        }
    } else {
        publish_copy(source, &bundles, &destination, &verified, keys, now)?;
    }
    let selection = Selection {
        digest: verified.digest.clone(),
        highest_sequence: verified.manifest.sequence,
    };
    write_json_atomically(&root.join("selection.json"), &selection)?;
    Ok(verified.digest)
}

fn check_progression(root: &Path, bundle: &VerifiedBundle) -> PackageResult<()> {
    if root
        .join("selection.json")
        .try_exists()
        .map_err(|error| error.to_string())?
    {
        let previous = read_selection(root)?;
        if bundle.manifest.sequence < previous.highest_sequence
            || (bundle.manifest.sequence == previous.highest_sequence
                && bundle.digest != previous.digest)
        {
            return Err(
                "web release must advance sequence; historical rollback is not enabled".into(),
            );
        }
    }
    Ok(())
}

fn publish_copy(
    source: &Path,
    bundles: &Path,
    destination: &Path,
    bundle: &VerifiedBundle,
    keys: &BTreeMap<String, String>,
    now: u64,
) -> PackageResult<()> {
    let temporary = bundles.join(format!(".stage-{}", random_identifier()?));
    create_private_directory(&temporary)?;
    let result = (|| {
        for (name, bytes) in &bundle.assets {
            write_asset(&temporary, name, bytes)?;
        }
        for name in ["manifest.json", "manifest.sig"] {
            let bytes = read_bounded(&safe_path(source, name)?, 65_536)?;
            amaterasu_storage::write_atomically(&temporary.join(name), &bytes)?;
        }
        let copied = verify_bundle(&temporary, keys, now)?;
        if copied.digest != bundle.digest {
            return Err("web manifest changed during staging".into());
        }
        fs::rename(&temporary, destination).map_err(|error| error.to_string())?;
        amaterasu_storage::sync_directory(bundles)
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&temporary);
    }
    result
}
