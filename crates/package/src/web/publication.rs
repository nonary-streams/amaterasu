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

pub(super) fn collect(
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

pub(super) fn write_asset(root: &Path, name: &str, bytes: &[u8]) -> PackageResult<()> {
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
    // The caller supplies only the locally trusted project bootstrap keys.
    stage_bundle_with_scope(source, root, keys, None, now)
}

/// Select a bundle from an approved repository. Its signed sequence is scoped
/// to the pinned publisher, so unrelated publishers may use independent
/// numbering while replay or equivocation by one publisher remains blocked.
pub fn stage_repository_bundle(
    source: &Path,
    root: &Path,
    keys: &BTreeMap<String, String>,
    publisher_fingerprint: &str,
    now: u64,
) -> PackageResult<String> {
    crate::decode_hex::<32>(publisher_fingerprint)?;
    if publisher_fingerprint != publisher_fingerprint.to_ascii_lowercase() || keys.len() != 1 {
        return Err("repository UI selection requires exactly its pinned publisher key".into());
    }
    let public_key = keys
        .get(publisher_fingerprint)
        .ok_or("repository UI publisher key is absent")?;
    if crate::sha256(&crate::decode_hex::<32>(public_key)?) != publisher_fingerprint {
        return Err("repository UI publisher fingerprint does not match its key".into());
    }
    stage_bundle_with_scope(source, root, keys, Some(publisher_fingerprint), now)
}

fn stage_bundle_with_scope(
    source: &Path,
    root: &Path,
    keys: &BTreeMap<String, String>,
    publisher_fingerprint: Option<&str>,
    now: u64,
) -> PackageResult<String> {
    create_private_directory(root)?;
    let _lock = RootLock::acquire(root)?;
    let verified = verify_bundle(source, keys, now)?;
    let previous = if root.join("selection.json").exists() {
        Some(read_selection(root)?)
    } else {
        None
    };
    check_progression(previous.as_ref(), &verified, publisher_fingerprint)?;
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
    let mut publisher_floors = previous
        .as_ref()
        .map_or_else(BTreeMap::new, |old| old.publisher_floors.clone());
    if let Some(fingerprint) = publisher_fingerprint {
        publisher_floors.insert(
            fingerprint.into(),
            PublisherFloor {
                sequence: verified.manifest.sequence,
                digest: verified.digest.clone(),
            },
        );
    }
    let mut project_floor = previous.as_ref().and_then(|old| old.project_floor.clone());
    if project_floor.is_none()
        && publisher_fingerprint.is_some()
        && let Some(old) = previous
            .as_ref()
            .filter(|old| old.publisher_floors.is_empty())
    {
        project_floor = Some(PublisherFloor {
            sequence: old.highest_sequence,
            digest: old.digest.clone(),
        });
    }
    if publisher_fingerprint.is_none() {
        project_floor = Some(PublisherFloor {
            sequence: verified.manifest.sequence,
            digest: verified.digest.clone(),
        });
    }
    let selection = Selection {
        digest: verified.digest.clone(),
        highest_sequence: verified
            .manifest
            .sequence
            .max(previous.as_ref().map_or(0, |old| old.highest_sequence)),
        active_sequence: Some(verified.manifest.sequence),
        previous_digest: previous.as_ref().and_then(|old| {
            if old.digest == verified.digest {
                old.previous_digest.clone()
            } else {
                Some(old.digest.clone())
            }
        }),
        publisher_floors,
        project_floor,
    };
    write_json_atomically(&root.join("selection.json"), &selection)?;
    Ok(verified.digest)
}

/// Administrator recovery to the last verified selection without lowering the
/// accepted release floor. The caller must supply the currently authorized keys.
pub fn restore_previous(
    root: &Path,
    keys: &BTreeMap<String, String>,
    now: u64,
) -> PackageResult<String> {
    let _lock = RootLock::acquire(root)?;
    let selection = read_selection(root)?;
    let previous = selection
        .previous_digest
        .clone()
        .ok_or("no previous web bundle available")?;
    restore_selected_version(root, selection, &previous, keys, now)
}

/// Explicit administrator recovery to any retained, currently trusted signed
/// bundle. The release floors remain unchanged even for a historical choice.
pub fn restore_version(
    root: &Path,
    digest: &str,
    keys: &BTreeMap<String, String>,
    now: u64,
) -> PackageResult<String> {
    let _lock = RootLock::acquire(root)?;
    let selection = read_selection(root)?;
    restore_selected_version(root, selection, digest, keys, now)
}

fn restore_selected_version(
    root: &Path,
    mut selection: Selection,
    digest: &str,
    keys: &BTreeMap<String, String>,
    now: u64,
) -> PackageResult<String> {
    let bundle = super::load_version(root, digest, keys, now)?;
    selection.previous_digest = Some(selection.digest);
    if selection.previous_digest.as_deref() == Some(digest) {
        return Err("requested UI is already selected".into());
    }
    selection.digest = digest.into();
    selection.active_sequence = Some(bundle.manifest.sequence);
    write_json_atomically(&root.join("selection.json"), &selection)?;
    Ok(digest.into())
}

fn check_progression(
    previous: Option<&Selection>,
    bundle: &VerifiedBundle,
    publisher_fingerprint: Option<&str>,
) -> PackageResult<()> {
    let Some(previous) = previous else {
        return Ok(());
    };
    if let Some(fingerprint) = publisher_fingerprint {
        if let Some(floor) = previous.publisher_floors.get(fingerprint)
            && (bundle.manifest.sequence < floor.sequence
                || (bundle.manifest.sequence == floor.sequence && bundle.digest != floor.digest))
        {
            return Err("publisher web release sequence replay or equivocation".into());
        }
    } else {
        let project_floor = previous.project_floor.clone().or_else(|| {
            previous
                .publisher_floors
                .is_empty()
                .then(|| PublisherFloor {
                    sequence: previous.highest_sequence,
                    digest: previous.digest.clone(),
                })
        });
        if let Some(floor) = project_floor
            && (bundle.manifest.sequence < floor.sequence
                || (bundle.manifest.sequence == floor.sequence && bundle.digest != floor.digest))
        {
            return Err("project web release sequence replay or equivocation".into());
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
