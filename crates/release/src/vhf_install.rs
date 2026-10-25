//! Optional Windows VHF input package. Repository approval never installs this
//! privileged component; an administrator must separately confirm its signer.
use amaterasu_package::PackageResult;
#[cfg(any(windows, test))]
use amaterasu_storage::{read_json, write_json_atomically};
#[cfg(any(windows, test))]
use serde::{Deserialize, Serialize};
#[cfg(any(windows, test))]
use sha2::{Digest, Sha256};
#[cfg(any(windows, test))]
use std::collections::BTreeMap;
#[cfg(any(windows, test))]
use std::fs::{self, File, OpenOptions};
use std::path::Path;
#[cfg(any(windows, test))]
use std::path::PathBuf;

#[cfg(any(windows, test))]
const HARDWARE_ID: &str = "ROOT\\AMATERASUVHFINPUT";
#[cfg(any(windows, test))]
const SERVICE_NAME: &str = "AmaterasuInputCompanion";
#[cfg(any(windows, test))]
const FILES: [&str; 4] = [
    "driver/AmaterasuVhfInput.inf",
    "driver/AmaterasuVhfInput.dll",
    "driver/AmaterasuVhfInput.cat",
    "amaterasu-input-companion.exe",
];

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
#[cfg(any(windows, test))]
struct Manifest {
    schema: u32,
    architecture: String,
    hardware_id: String,
    service_name: String,
    files: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
#[cfg(any(windows, test))]
struct InstallJournal {
    schema: u32,
    package_digest: String,
    staged_files: BTreeMap<String, String>,
    #[serde(default)]
    staging_pending: BTreeMap<String, String>,
    service_pending: bool,
    service_created: bool,
    driver_pending: bool,
    device_instance: Option<String>,
    published_inf: Option<String>,
}

#[cfg(any(windows, test))]
trait Signatures {
    fn verify_catalog_member(&self, catalog: &Path, member: &Path) -> PackageResult<()>;
    fn verified_signer(&self, file: &Path) -> PackageResult<String>;
}

#[cfg(any(windows, test))]
trait InstallActions {
    fn ensure_ready(&self, destination: &Path) -> PackageResult<()>;
    fn service_absent(&self) -> PackageResult<bool>;
    fn service_stopped(&self) -> PackageResult<()>;
    fn create_service(&self, executable: &Path, runtime: &Path) -> PackageResult<()>;
    fn service_matches(&self, executable: &Path, runtime: &Path) -> PackageResult<bool>;
    fn remove_service(&self) -> PackageResult<()>;
    fn install_device(&self, inf: &Path) -> PackageResult<(String, String)>;
    fn find_owned_device(&self) -> PackageResult<Option<(String, String)>>;
    fn find_matching_published_inf(&self, staged_inf: &Path) -> PackageResult<Option<String>>;
    fn published_inf_matches(&self, published_inf: &str, staged_inf: &Path) -> PackageResult<bool>;
    fn remove_device(&self, instance: &str) -> PackageResult<()>;
    fn remove_published_inf(&self, inf: &str) -> PackageResult<()>;
}

#[cfg(any(windows, test))]
fn normalized_fingerprint(value: &str) -> PackageResult<String> {
    let value = value.trim().replace(' ', "").to_ascii_uppercase();
    if value.len() != 40 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("confirmed signer fingerprint must be 40 hexadecimal characters".into());
    }
    Ok(value)
}

#[cfg(any(windows, test))]
fn safe_file(root: &Path, relative: &str) -> PackageResult<PathBuf> {
    let path = root.join(relative);
    let metadata = fs::symlink_metadata(&path).map_err(|error| error.to_string())?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > 64 * 1024 * 1024
    {
        return Err(format!("unsafe or oversized VHF package file: {relative}"));
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err(format!("VHF package reparse point: {relative}"));
        }
    }
    Ok(path)
}

#[cfg(any(windows, test))]
fn check_package_directories(root: &Path) -> PackageResult<()> {
    for directory in [root.to_path_buf(), root.join("driver")] {
        let metadata = fs::symlink_metadata(&directory).map_err(|error| error.to_string())?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err("VHF package directory is not a real directory".into());
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if metadata.file_attributes() & 0x400 != 0 {
                return Err("VHF package directory is a reparse point".into());
            }
        }
    }
    Ok(())
}

#[cfg(any(windows, test))]
fn file_hash(path: &Path) -> PackageResult<String> {
    let bytes = fs::read(path).map_err(|error| error.to_string())?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

#[cfg(any(windows, test))]
fn require_x64_pe(path: &Path) -> PackageResult<()> {
    let bytes = fs::read(path).map_err(|error| error.to_string())?;
    if bytes.len() < 0x40 || &bytes[..2] != b"MZ" {
        return Err("VHF native payload is not a PE image".into());
    }
    let pe_offset = u32::from_le_bytes(bytes[0x3c..0x40].try_into().unwrap()) as usize;
    if bytes
        .get(pe_offset..pe_offset.saturating_add(6))
        .is_none_or(|header| {
            &header[..4] != b"PE\0\0" || u16::from_le_bytes([header[4], header[5]]) != 0x8664
        })
    {
        return Err("VHF native payload is not an x64 PE image".into());
    }
    Ok(())
}

#[cfg(any(windows, test))]
struct OperationLock {
    file: File,
}

#[cfg(any(windows, test))]
impl Drop for OperationLock {
    fn drop(&mut self) {
        // Explicitly release the operation's lock before closing our handle.
        // On Unix a concurrent subprocess fork can briefly retain a duplicate
        // of this open-file description, so closing alone may leave it locked.
        let _ = self.file.unlock();
    }
}

#[cfg(any(windows, test))]
fn operation_lock(runtime: &Path) -> PackageResult<OperationLock> {
    let path = runtime.join("vhf-install.lock");
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .map_err(|error| error.to_string())?;
    let metadata = fs::symlink_metadata(&path).map_err(|error| error.to_string())?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("VHF operation lock is not a regular file".into());
    }
    file.try_lock()
        .map_err(|error| format!("another VHF installation operation is active: {error}"))?;
    Ok(OperationLock { file })
}

#[cfg(any(windows, test))]
fn verify_package(
    root: &Path,
    confirmed_fingerprint: &str,
    signatures: &impl Signatures,
) -> PackageResult<(Manifest, String)> {
    check_package_directories(root)?;
    let confirmed = normalized_fingerprint(confirmed_fingerprint)?;
    let manifest: Manifest = read_json(&root.join("manifest.json"))?;
    if manifest.schema != 1
        || manifest.architecture != "x64"
        || manifest.hardware_id != HARDWARE_ID
        || manifest.service_name != SERVICE_NAME
        || manifest.files.len() != FILES.len()
    {
        return Err("VHF package identity or schema is unsupported".into());
    }
    for relative in FILES {
        let expected = manifest
            .files
            .get(relative)
            .ok_or("VHF package file missing")?;
        if expected.len() != 64 || !expected.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err("invalid VHF package payload hash".into());
        }
        let actual = file_hash(&safe_file(root, relative)?)?;
        if !actual.eq_ignore_ascii_case(expected) {
            return Err(format!("VHF package payload hash mismatch: {relative}"));
        }
    }
    require_x64_pe(&safe_file(root, FILES[1])?)?;
    require_x64_pe(&safe_file(root, FILES[3])?)?;
    let catalog = safe_file(root, FILES[2])?;
    if normalized_fingerprint(&signatures.verified_signer(&catalog)?)? != confirmed {
        return Err("VHF catalog signer differs from confirmed fingerprint".into());
    }
    for member in &FILES[..2] {
        signatures.verify_catalog_member(&catalog, &safe_file(root, member)?)?;
    }
    if normalized_fingerprint(&signatures.verified_signer(&safe_file(root, FILES[3])?)?)?
        != confirmed
    {
        return Err("VHF companion signer differs from confirmed fingerprint".into());
    }
    // Manifest hashes detect mutation between verification and protected staging;
    // trust comes from the catalog and independently confirmed signing identity.
    let package_digest = file_hash(&root.join("manifest.json"))?;
    Ok((manifest, package_digest))
}

#[cfg(any(windows, test))]
fn seal_package(root: &Path, fingerprint: &str, signatures: &impl Signatures) -> PackageResult<()> {
    check_package_directories(root)?;
    if root.join("manifest.json").exists() {
        return Err("VHF manifest already exists; refusing to replace it".into());
    }
    let confirmed = normalized_fingerprint(fingerprint)?;
    require_x64_pe(&safe_file(root, FILES[1])?)?;
    require_x64_pe(&safe_file(root, FILES[3])?)?;
    let catalog = safe_file(root, FILES[2])?;
    if normalized_fingerprint(&signatures.verified_signer(&catalog)?)? != confirmed {
        return Err("VHF catalog signer differs from confirmed fingerprint".into());
    }
    for relative in &FILES[..2] {
        signatures.verify_catalog_member(&catalog, &safe_file(root, relative)?)?;
    }
    if normalized_fingerprint(&signatures.verified_signer(&safe_file(root, FILES[3])?)?)?
        != confirmed
    {
        return Err("VHF companion signer differs from confirmed fingerprint".into());
    }
    let mut files = BTreeMap::new();
    for relative in FILES {
        files.insert(relative.into(), file_hash(&safe_file(root, relative)?)?);
    }
    let path = root.join("manifest.json");
    write_json_atomically(
        &path,
        &Manifest {
            schema: 1,
            architecture: "x64".into(),
            hardware_id: HARDWARE_ID.into(),
            service_name: SERVICE_NAME.into(),
            files,
        },
    )?;
    if let Err(error) = verify_package(root, fingerprint, signatures) {
        fs::remove_file(path).map_err(|cleanup| format!("{error}; manifest cleanup: {cleanup}"))?;
        return Err(error);
    }
    Ok(())
}

#[cfg(any(windows, test))]
fn journal_path(destination: &Path) -> PathBuf {
    destination.join("install-journal.json")
}

#[cfg(any(windows, test))]
fn install(
    source: &Path,
    destination: &Path,
    runtime: &Path,
    fingerprint: &str,
    signatures: &impl Signatures,
    actions: &impl InstallActions,
) -> PackageResult<()> {
    let _operation_lock = operation_lock(runtime)?;
    let (manifest, package_digest) = verify_package(source, fingerprint, signatures)?;
    actions.ensure_ready(destination)?;
    if fs::read_dir(destination)
        .map_err(|error| error.to_string())?
        .next()
        .is_some()
    {
        return Err("VHF install directory is not empty; refusing replacement".into());
    }
    if actions.find_owned_device()?.is_some() {
        return Err("Amaterasu VHF device already exists; refusing replacement".into());
    }
    if actions
        .find_matching_published_inf(&source.join(FILES[0]))?
        .is_some()
    {
        return Err("matching VHF driver package already exists; refusing replacement".into());
    }
    if !actions.service_absent()? {
        return Err("Amaterasu input companion service already exists".into());
    }
    let mut journal = InstallJournal {
        schema: 1,
        package_digest,
        staged_files: BTreeMap::new(),
        staging_pending: BTreeMap::new(),
        service_pending: false,
        service_created: false,
        driver_pending: false,
        device_instance: None,
        published_inf: None,
    };
    write_json_atomically(&journal_path(destination), &journal)?;
    let result: PackageResult<()> = (|| {
        for relative in FILES.into_iter().chain(Some("manifest.json")) {
            let source_file = safe_file(source, relative)?;
            let target_file = destination.join(relative);
            fs::create_dir_all(target_file.parent().ok_or("missing stage parent")?)
                .map_err(|error| error.to_string())?;
            let expected = if relative == "manifest.json" {
                file_hash(&source_file)?
            } else {
                manifest.files[relative].to_ascii_lowercase()
            };
            journal
                .staging_pending
                .insert(relative.into(), expected.clone());
            write_json_atomically(&journal_path(destination), &journal)?;
            fs::copy(&source_file, &target_file).map_err(|error| error.to_string())?;
            if file_hash(&target_file)? != expected {
                return Err("VHF stage changed while copying".into());
            }
            journal.staged_files.insert(relative.into(), expected);
            journal.staging_pending.remove(relative);
            write_json_atomically(&journal_path(destination), &journal)?;
        }
        verify_package(destination, fingerprint, signatures)?;
        if !actions.service_absent()? {
            return Err("Amaterasu input companion service already exists".into());
        }
        journal.service_pending = true;
        write_json_atomically(&journal_path(destination), &journal)?;
        actions.create_service(&destination.join(FILES[3]), runtime)?;
        journal.service_created = true;
        journal.service_pending = false;
        write_json_atomically(&journal_path(destination), &journal)?;
        journal.driver_pending = true;
        write_json_atomically(&journal_path(destination), &journal)?;
        let (instance, published_inf) = actions.install_device(&destination.join(FILES[0]))?;
        validate_owned_identity(&instance, &published_inf)?;
        journal.device_instance = Some(instance);
        journal.published_inf = Some(published_inf);
        journal.driver_pending = false;
        write_json_atomically(&journal_path(destination), &journal)?;
        Ok(())
    })();
    if let Err(error) = result {
        // Preserve the journal after failure. Rollback is separately explicit;
        // a failed OS operation may have completed before its result was seen.
        return Err(format!(
            "VHF install incomplete; use exact journal recovery: {}",
            error
        ));
    }
    Ok(())
}

#[cfg(any(windows, test))]
fn validate_owned_instance(instance: &str) -> PackageResult<()> {
    // DevCon assigns ROOT\SYSTEM\NNNN as the instance ID for the System
    // class. The separately queried hardware-ID property establishes ownership.
    let prefix = "ROOT\\SYSTEM\\";
    if !instance.to_ascii_uppercase().starts_with(prefix)
        || instance[prefix.len()..].is_empty()
        || !instance[prefix.len()..]
            .bytes()
            .all(|byte| byte.is_ascii_digit())
    {
        return Err("OS returned an unexpected VHF device identity".into());
    }
    Ok(())
}

#[cfg(any(windows, test))]
fn validate_owned_identity(instance: &str, inf: &str) -> PackageResult<()> {
    validate_owned_instance(instance)?;
    if !inf.to_ascii_lowercase().starts_with("oem")
        || !inf.to_ascii_lowercase().ends_with(".inf")
        || inf.len() <= 7
        || !inf[3..inf.len() - 4]
            .bytes()
            .all(|byte| byte.is_ascii_digit())
    {
        return Err("OS returned an unexpected VHF device or driver identity".into());
    }
    Ok(())
}

#[cfg(any(windows, test))]
fn rollback(
    destination: &Path,
    runtime: &Path,
    actions: &impl InstallActions,
) -> PackageResult<()> {
    let _operation_lock = operation_lock(runtime)?;
    if runtime.join("input-companion-policy.json").exists() {
        return Err("revoke the VHF input grant before package rollback".into());
    }
    let path = journal_path(destination);
    let mut journal: InstallJournal = read_json(&path)?;
    if journal.schema != 1 {
        return Err("unknown VHF recovery journal".into());
    }
    // The companion must be stopped before removing either its device or files.
    actions.service_stopped()?;
    if journal.driver_pending {
        if let Some((instance, inf)) = actions.find_owned_device()? {
            validate_owned_instance(&instance)?;
            if !inf.is_empty() {
                validate_owned_identity(&instance, &inf)?;
                if !actions.published_inf_matches(&inf, &destination.join(FILES[0]))? {
                    return Err("uncertain VHF driver operation produced a different INF".into());
                }
                journal.published_inf = Some(inf);
            }
            journal.device_instance = Some(instance);
        }
        if let Some(inf) = actions.find_matching_published_inf(&destination.join(FILES[0]))? {
            validate_owned_identity("ROOT\\SYSTEM\\0", &inf)?;
            if journal
                .published_inf
                .as_ref()
                .is_some_and(|known| known != &inf)
            {
                return Err("VHF device and published INF identities disagree".into());
            }
            journal.published_inf = Some(inf);
        }
        journal.driver_pending = false;
        write_json_atomically(&path, &journal)?;
    }
    if let Some(instance) = &journal.device_instance {
        validate_owned_instance(instance)?;
        let current = actions.find_owned_device()?;
        if let Some((current_instance, current_inf)) = current.as_ref()
            && (current_instance != instance
                || (!current_inf.is_empty() && journal.published_inf.as_ref() != Some(current_inf)))
        {
            return Err("VHF device identity changed; refusing rollback".into());
        }
        if current.is_some() {
            actions.remove_device(instance)?;
        }
        journal.device_instance = None;
        write_json_atomically(&path, &journal)?;
    }
    if let Some(inf) = journal.published_inf.as_ref() {
        validate_owned_identity("ROOT\\SYSTEM\\0", inf)?;
        actions.remove_published_inf(inf)?;
        journal.published_inf = None;
        write_json_atomically(&path, &journal)?;
    }
    let executable = destination.join(FILES[3]);
    if journal.service_created || journal.service_pending {
        if actions.service_matches(&executable, runtime)? {
            actions.remove_service()?;
        }
        journal.service_created = false;
        journal.service_pending = false;
        write_json_atomically(&path, &journal)?;
    }
    for (relative, expected) in journal.staged_files.clone() {
        if !FILES.contains(&relative.as_str()) && relative != "manifest.json" {
            return Err("journal references an unowned VHF file".into());
        }
        let staged = destination.join(&relative);
        if staged.exists() {
            if file_hash(&staged)? != expected {
                return Err("staged VHF file changed; refusing deletion".into());
            }
            fs::remove_file(staged).map_err(|error| error.to_string())?;
        }
        journal.staged_files.remove(&relative);
        write_json_atomically(&path, &journal)?;
    }
    for relative in journal.staging_pending.clone().keys() {
        if !FILES.contains(&relative.as_str()) && relative != "manifest.json" {
            return Err("journal references an unowned VHF copy intent".into());
        }
        let staged = destination.join(relative);
        if staged.exists() {
            let metadata = fs::symlink_metadata(&staged).map_err(|error| error.to_string())?;
            if !metadata.is_file() || metadata.file_type().is_symlink() {
                return Err("interrupted VHF copy changed type; refusing deletion".into());
            }
            fs::remove_file(staged).map_err(|error| error.to_string())?;
        }
        journal.staging_pending.remove(relative);
        write_json_atomically(&path, &journal)?;
    }
    let driver_directory = destination.join("driver");
    if driver_directory.exists() {
        fs::remove_dir(&driver_directory).map_err(|error| error.to_string())?;
    }
    fs::remove_file(path).map_err(|error| error.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests;
#[cfg(windows)]
mod windows;

#[cfg(windows)]
pub(super) fn verify_command(
    package: &Path,
    fingerprint: &str,
    signtool: &Path,
) -> PackageResult<()> {
    windows::verify_command(package, fingerprint, signtool)
}
#[cfg(windows)]
pub(super) fn seal_command(
    package: &Path,
    fingerprint: &str,
    signtool: &Path,
) -> PackageResult<()> {
    windows::seal_command(package, fingerprint, signtool)
}
#[cfg(not(windows))]
pub(super) fn seal_command(_: &Path, _: &str, _: &Path) -> PackageResult<()> {
    Err("VHF installation is Windows-only".into())
}
#[cfg(not(windows))]
pub(super) fn verify_command(_: &Path, _: &str, _: &Path) -> PackageResult<()> {
    Err("VHF installation is Windows-only".into())
}

#[cfg(windows)]
pub(super) fn install_command(
    package: &Path,
    install_root: &Path,
    runtime: &Path,
    fingerprint: &str,
    signtool: &Path,
    devcon: &Path,
) -> PackageResult<()> {
    windows::install_command(
        package,
        install_root,
        runtime,
        fingerprint,
        signtool,
        devcon,
    )
}
#[cfg(not(windows))]
pub(super) fn install_command(
    _: &Path,
    _: &Path,
    _: &Path,
    _: &str,
    _: &Path,
    _: &Path,
) -> PackageResult<()> {
    Err("VHF installation is Windows-only".into())
}

#[cfg(windows)]
pub(super) fn rollback_command(
    install_root: &Path,
    runtime: &Path,
    devcon: &Path,
) -> PackageResult<()> {
    windows::rollback_command(install_root, runtime, devcon)
}
#[cfg(not(windows))]
pub(super) fn rollback_command(_: &Path, _: &Path, _: &Path) -> PackageResult<()> {
    Err("VHF installation is Windows-only".into())
}

#[cfg(windows)]
pub(super) fn grant_command(
    destination: &Path,
    runtime: &Path,
    digest: &str,
    keyboard: &str,
    mouse: &str,
) -> PackageResult<()> {
    windows::grant_command(destination, runtime, digest, keyboard, mouse)
}
#[cfg(not(windows))]
pub(super) fn grant_command(_: &Path, _: &Path, _: &str, _: &str, _: &str) -> PackageResult<()> {
    Err("VHF installation is Windows-only".into())
}

#[cfg(windows)]
pub(super) fn revoke_command(destination: &Path, runtime: &Path) -> PackageResult<()> {
    windows::revoke_command(destination, runtime)
}
#[cfg(not(windows))]
pub(super) fn revoke_command(_: &Path, _: &Path) -> PackageResult<()> {
    Err("VHF installation is Windows-only".into())
}
