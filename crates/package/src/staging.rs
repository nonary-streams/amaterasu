use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use super::storage::sync_directory;
use super::verification::MAX_ARTIFACT_BYTES;
use crate::{
    ActivationState, BootstrapConfig, PackageResult, VerificationPolicy, VerifiedPackage,
    create_private_directory, decode_hex, random_identifier, read_bounded, read_json,
    unix_timestamp, verify_package, write_json_atomically,
};

pub fn package_directory(root: &Path, digest: &str) -> PackageResult<PathBuf> {
    decode_hex::<32>(digest)?;
    if digest != digest.to_ascii_lowercase() {
        return Err("noncanonical package identity".into());
    }
    Ok(root.join("sets").join(digest))
}

/// The caller must hold RootLock for the complete staging operation.
pub fn stage_package(
    source: &Path,
    root: &Path,
    bootstrap: &BootstrapConfig,
    allow_historical: bool,
) -> PackageResult<String> {
    let selection_path = root.join("selection.json");
    let mut selection = read_selection(&selection_path)?;
    let policy = VerificationPolicy {
        minimum_sequence: selection.highest_sequence,
        allow_historical,
        now: unix_timestamp(),
    };
    let package = verify_package(source, bootstrap, policy)?;

    install_immutable_set(source, root, bootstrap, policy, &package)?;

    selection.highest_sequence = selection.highest_sequence.max(package.manifest.sequence);
    selection.pending = Some(package.digest.clone());
    write_json_atomically(&selection_path, &selection)?;
    Ok(package.digest)
}

fn read_selection(path: &Path) -> PackageResult<ActivationState> {
    if path.try_exists().map_err(|error| error.to_string())? {
        read_json(path)
    } else {
        Ok(ActivationState::default())
    }
}

pub(crate) fn install_immutable_set(
    source: &Path,
    root: &Path,
    bootstrap: &BootstrapConfig,
    policy: VerificationPolicy,
    package: &VerifiedPackage,
) -> PackageResult<()> {
    let sets_directory = root.join("sets");
    create_private_directory(&sets_directory)?;
    let destination = package_directory(root, &package.digest)?;

    if destination
        .try_exists()
        .map_err(|error| error.to_string())?
    {
        verify_package(&destination, bootstrap, policy)?;
        return Ok(());
    }

    let temporary = sets_directory.join(format!(".stage-{}", random_identifier()?));
    create_private_directory(&temporary)?;
    let result = copy_and_publish(source, &temporary, &destination, bootstrap, policy, package);
    if result.is_err() {
        let _ = fs::remove_dir_all(&temporary);
    }
    result
}

fn copy_and_publish(
    source: &Path,
    temporary: &Path,
    destination: &Path,
    bootstrap: &BootstrapConfig,
    policy: VerificationPolicy,
    package: &VerifiedPackage,
) -> PackageResult<()> {
    copy_package_files(source, temporary, package)?;

    // Verify the copied bytes before publishing; the source may have changed.
    let copied_package = verify_package(temporary, bootstrap, policy)?;
    if copied_package.digest != package.digest {
        return Err("source manifest changed during staging".into());
    }

    sync_directory(temporary)?;
    fs::rename(temporary, destination).map_err(|error| error.to_string())?;
    sync_directory(
        destination
            .parent()
            .ok_or("package has no parent directory")?,
    )
}

fn copy_package_files(
    source: &Path,
    destination: &Path,
    package: &VerifiedPackage,
) -> PackageResult<()> {
    let metadata_files =
        std::iter::once("manifest.json").chain(package.signed.then_some("manifest.sig"));
    let artifacts = package
        .manifest
        .components
        .iter()
        .map(|component| component.file.as_str());

    let helpers = package
        .manifest
        .privileged_requests
        .iter()
        .map(|request| request.helper.file.as_str());
    for filename in metadata_files.chain(artifacts).chain(helpers) {
        let bytes = read_bounded(&source.join(filename), MAX_ARTIFACT_BYTES)?;
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(destination.join(filename))
            .map_err(|error| error.to_string())?;
        output
            .write_all(&bytes)
            .map_err(|error| error.to_string())?;
        output.sync_all().map_err(|error| error.to_string())?;
    }
    Ok(())
}
