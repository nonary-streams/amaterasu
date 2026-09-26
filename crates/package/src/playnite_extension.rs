//! Bounded installation of the approved Playnite script extension.

use crate::{
    PackageResult, create_machine_private_directory, random_identifier, read_bounded, sha256,
    write_json_atomically,
};
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Component, Path, PathBuf};

const ACTIVE_DIRECTORY: &str = "Amaterasu.Playnite.Connector";
const TRANSACTION_DIRECTORY: &str = ".amaterasu-playnite-installer";
const VERSION: &str = "0.1.0";
const FILES: [(&str, &str, u64); 2] = [
    (
        "extension.yaml",
        "0e3507a22ac5c24f1e0573caaff52f572bc1af9c1c18270905b2694ebfeba72d",
        4096,
    ),
    (
        "AmaterasuPlaynite.psm1",
        "264d97199dcc36e772e739da9b2e4311a27c9350d9436ae2bb647d1572ee1715",
        65536,
    ),
];

/// Persisted before replacing an existing extension, so an installer restart
/// can load this receipt and restore the previous directory.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PlayniteExtensionReceipt {
    pub extensions_directory: PathBuf,
    pub transaction: String,
    pub had_previous: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PlayniteExtensionInstall {
    AlreadyCurrent,
    Installed(PlayniteExtensionReceipt),
}

/// Installs exactly the approved two-file extension into the supplied Playnite
/// Extensions directory. The caller supplies an authenticated user profile and
/// SID; this function verifies that the target is inside that profile and owned
/// by that SID. A separately configured portable location needs its own policy.
pub fn install_playnite_extension(
    source: &Path,
    extensions: &Path,
    authorized_profile: &Path,
    authorized_sid: &str,
) -> PackageResult<PlayniteExtensionInstall> {
    install_with_hashes(
        source,
        extensions,
        authorized_profile,
        authorized_sid,
        &FILES,
    )
}

fn install_with_hashes(
    source: &Path,
    extensions: &Path,
    authorized_profile: &Path,
    authorized_sid: &str,
    expected: &[(&str, &str, u64); 2],
) -> PackageResult<PlayniteExtensionInstall> {
    validate_target(extensions, authorized_profile, authorized_sid)?;
    let assets = read_approved_source(source, expected)?;
    let transactions = extensions.join(TRANSACTION_DIRECTORY);
    if transactions.try_exists().map_err(io_error)? {
        require_real_directory(&transactions)?;
        require_transaction_owner(&transactions, authorized_sid)?;
        if fs::read_dir(&transactions)
            .map_err(io_error)?
            .next()
            .is_some()
        {
            return Err(
                "a Playnite extension transaction requires rollback or finalization".into(),
            );
        }
    } else {
        create_machine_private_directory(&transactions)?;
    }
    require_transaction_owner(&transactions, authorized_sid)?;
    let active = extensions.join(ACTIVE_DIRECTORY);
    if active.try_exists().map_err(io_error)? && verify_pair(&active, expected).is_ok() {
        require_active_owner(&active, authorized_sid)?;
        return Ok(PlayniteExtensionInstall::AlreadyCurrent);
    }
    let transaction = random_identifier()?;
    let stage = transactions.join(format!("version-{VERSION}-{transaction}"));
    let backup = transactions.join(format!("backup-{transaction}"));
    let receipt_path = transactions.join(format!("receipt-{transaction}.json"));
    let had_previous = active.try_exists().map_err(io_error)?;
    if had_previous {
        require_exact_pair(&active)?;
        require_active_owner(&active, authorized_sid)?;
    }
    let receipt = PlayniteExtensionReceipt {
        extensions_directory: extensions.to_path_buf(),
        transaction,
        had_previous,
    };
    write_json_atomically(&receipt_path, &receipt)?;
    fs::create_dir(&stage).map_err(io_error)?;
    for ((name, _, _), bytes) in expected.iter().zip(assets.iter()) {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(stage.join(name))
            .map_err(io_error)?;
        file.write_all(bytes).map_err(io_error)?;
        file.sync_all().map_err(io_error)?;
    }
    verify_pair(&stage, expected)?;
    grant_user_read_access(&stage, authorized_sid)?;
    for (name, _, _) in expected {
        grant_user_read_access(&stage.join(name), authorized_sid)?;
    }
    let activation = (|| {
        if had_previous {
            fs::rename(&active, &backup).map_err(io_error)?;
        }
        fs::rename(&stage, &active).map_err(io_error)
    })();
    if let Err(error) = activation {
        if had_previous && backup.is_dir() && !active.exists() {
            let _ = fs::rename(&backup, &active);
        }
        return Err(error);
    }
    Ok(PlayniteExtensionInstall::Installed(receipt))
}

/// Reads only a receipt at the fixed transaction location, never a path from
/// the receipt itself. The caller can retain this across installer restarts.
pub fn read_playnite_extension_receipt(
    extensions: &Path,
    transaction: &str,
    authorized_profile: &Path,
    authorized_sid: &str,
) -> PackageResult<PlayniteExtensionReceipt> {
    validate_target(extensions, authorized_profile, authorized_sid)?;
    validate_transaction(transaction)?;
    let transactions = extensions.join(TRANSACTION_DIRECTORY);
    require_real_directory(&transactions)?;
    require_transaction_owner(&transactions, authorized_sid)?;
    let path = transactions.join(format!("receipt-{transaction}.json"));
    require_regular_file(&path)?;
    let receipt: PlayniteExtensionReceipt = crate::read_json(&path)?;
    if receipt.extensions_directory != extensions || receipt.transaction != transaction {
        return Err("Playnite extension receipt does not match target".into());
    }
    Ok(receipt)
}

/// Finds the single durable pending transaction after an installer restart.
/// Unknown entries are rejected so a caller cannot silently skip recovery.
pub fn pending_playnite_extension_receipt(
    extensions: &Path,
    authorized_profile: &Path,
    authorized_sid: &str,
) -> PackageResult<Option<PlayniteExtensionReceipt>> {
    validate_target(extensions, authorized_profile, authorized_sid)?;
    let transactions = extensions.join(TRANSACTION_DIRECTORY);
    if !transactions.try_exists().map_err(io_error)? {
        return Ok(None);
    }
    require_real_directory(&transactions)?;
    require_transaction_owner(&transactions, authorized_sid)?;
    let mut transaction = None;
    let mut auxiliary_ids = Vec::new();
    for (count, entry) in fs::read_dir(&transactions).map_err(io_error)?.enumerate() {
        if count >= 3 {
            return Err("too many Playnite transaction entries".into());
        }
        let entry = entry.map_err(io_error)?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| "invalid Playnite transaction entry")?;
        if let Some(id) = name
            .strip_prefix("receipt-")
            .and_then(|part| part.strip_suffix(".json"))
        {
            validate_transaction(id)?;
            if transaction.replace(id.to_owned()).is_some() {
                return Err("multiple Playnite extension transactions".into());
            }
        } else if let Some(id) = name
            .strip_prefix(&format!("version-{VERSION}-"))
            .or_else(|| name.strip_prefix("backup-"))
        {
            validate_transaction(id)?;
            require_real_directory(&entry.path())?;
            auxiliary_ids.push(id.to_owned());
        } else {
            return Err("unknown Playnite transaction entry".into());
        }
    }
    if auxiliary_ids
        .iter()
        .any(|id| Some(id.as_str()) != transaction.as_deref())
    {
        return Err("orphaned Playnite extension transaction stage".into());
    }
    transaction
        .map(|id| {
            read_playnite_extension_receipt(extensions, &id, authorized_profile, authorized_sid)
        })
        .transpose()
}

pub fn rollback_playnite_extension(
    receipt: &PlayniteExtensionReceipt,
    authorized_profile: &Path,
    authorized_sid: &str,
) -> PackageResult<()> {
    rollback_with_hashes(receipt, authorized_profile, authorized_sid, &FILES)
}

fn rollback_with_hashes(
    receipt: &PlayniteExtensionReceipt,
    authorized_profile: &Path,
    authorized_sid: &str,
    expected: &[(&str, &str, u64); 2],
) -> PackageResult<()> {
    let receipt = read_playnite_extension_receipt(
        &receipt.extensions_directory,
        &receipt.transaction,
        authorized_profile,
        authorized_sid,
    )?;
    let transactions = receipt.extensions_directory.join(TRANSACTION_DIRECTORY);
    let active = receipt.extensions_directory.join(ACTIVE_DIRECTORY);
    let backup = transactions.join(format!("backup-{}", receipt.transaction));
    let has_backup = backup.try_exists().map_err(io_error)?;
    if has_backup {
        require_exact_pair(&backup)?;
        if active.try_exists().map_err(io_error)? {
            verify_pair(&active, expected)?;
            fs::remove_dir_all(&active).map_err(io_error)?;
        }
        fs::rename(&backup, &active).map_err(io_error)?;
    } else if active.try_exists().map_err(io_error)? {
        if receipt.had_previous {
            require_exact_pair(&active)?;
        } else {
            verify_pair(&active, expected)?;
            fs::remove_dir_all(&active).map_err(io_error)?;
        }
    } else if receipt.had_previous {
        return Err("previous Playnite extension is missing".into());
    }
    remove_transaction(&transactions, &receipt.transaction)
}

pub fn finalize_playnite_extension(
    receipt: &PlayniteExtensionReceipt,
    authorized_profile: &Path,
    authorized_sid: &str,
) -> PackageResult<()> {
    finalize_with_hashes(receipt, authorized_profile, authorized_sid, &FILES)
}

fn finalize_with_hashes(
    receipt: &PlayniteExtensionReceipt,
    authorized_profile: &Path,
    authorized_sid: &str,
    expected: &[(&str, &str, u64); 2],
) -> PackageResult<()> {
    let receipt = read_playnite_extension_receipt(
        &receipt.extensions_directory,
        &receipt.transaction,
        authorized_profile,
        authorized_sid,
    )?;
    verify_pair(
        &receipt.extensions_directory.join(ACTIVE_DIRECTORY),
        expected,
    )?;
    let transactions = receipt.extensions_directory.join(TRANSACTION_DIRECTORY);
    let backup = transactions.join(format!("backup-{}", receipt.transaction));
    if receipt.had_previous {
        require_exact_pair(&backup)?;
        fs::remove_dir_all(&backup).map_err(io_error)?;
    }
    remove_transaction(&transactions, &receipt.transaction)
}

fn remove_transaction(transactions: &Path, transaction: &str) -> PackageResult<()> {
    let stage = transactions.join(format!("version-{VERSION}-{transaction}"));
    if stage.exists() {
        require_partial_pair(&stage)?;
        fs::remove_dir_all(stage).map_err(io_error)?;
    }
    fs::remove_file(transactions.join(format!("receipt-{transaction}.json"))).map_err(io_error)
}

fn require_partial_pair(directory: &Path) -> PackageResult<()> {
    require_real_directory(directory)?;
    for (count, entry) in fs::read_dir(directory).map_err(io_error)?.enumerate() {
        if count >= 2 {
            return Err("Playnite stage contains extra entries".into());
        }
        let entry = entry.map_err(io_error)?;
        if !FILES.iter().any(|(name, _, _)| entry.file_name() == *name) {
            return Err("Playnite stage contains an unexpected file".into());
        }
        require_regular_file(&entry.path())?;
    }
    Ok(())
}

fn validate_transaction(transaction: &str) -> PackageResult<()> {
    if transaction.len() != 32 || !transaction.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("invalid Playnite extension transaction".into());
    }
    Ok(())
}

fn read_approved_source(
    source: &Path,
    expected: &[(&str, &str, u64); 2],
) -> PackageResult<[Vec<u8>; 2]> {
    require_real_directory(source)?;
    require_exact_pair(source)?;
    let mut assets = [Vec::new(), Vec::new()];
    for (index, (name, hash, maximum)) in expected.iter().enumerate() {
        let bytes = read_bounded(&source.join(name), *maximum)?;
        let canonical = normalize_crlf(&bytes);
        if sha256(&canonical) != *hash {
            return Err("Playnite extension source does not match the approved artifact".into());
        }
        assets[index] = canonical;
    }
    Ok(assets)
}

fn normalize_crlf(bytes: &[u8]) -> Vec<u8> {
    let mut output = Vec::with_capacity(bytes.len());
    for (index, byte) in bytes.iter().enumerate() {
        if *byte == b'\n' && (index == 0 || bytes[index - 1] != b'\r') {
            output.push(b'\r');
        }
        output.push(*byte);
    }
    output
}

fn verify_pair(directory: &Path, expected: &[(&str, &str, u64); 2]) -> PackageResult<()> {
    require_exact_pair(directory)?;
    for (name, hash, maximum) in expected {
        if sha256(&read_bounded(&directory.join(name), *maximum)?) != *hash {
            return Err("Playnite extension files changed after staging".into());
        }
    }
    Ok(())
}

fn require_exact_pair(directory: &Path) -> PackageResult<()> {
    require_real_directory(directory)?;
    let mut names = Vec::new();
    for entry in fs::read_dir(directory).map_err(io_error)? {
        let entry = entry.map_err(io_error)?;
        require_regular_file(&entry.path())?;
        names.push(entry.file_name());
        if names.len() > 2 {
            return Err("Playnite extension directory contains extra files".into());
        }
    }
    names.sort();
    let mut expected: [std::ffi::OsString; 2] = FILES.map(|(name, _, _)| name.into());
    expected.sort();
    if names != expected {
        return Err("Playnite extension directory is incomplete".into());
    }
    Ok(())
}

fn require_regular_file(path: &Path) -> PackageResult<()> {
    let metadata = fs::symlink_metadata(path).map_err(io_error)?;
    if !metadata.is_file() || is_reparse(&metadata) {
        return Err("Playnite extension file is unsafe".into());
    }
    Ok(())
}

fn require_real_directory(path: &Path) -> PackageResult<()> {
    let metadata = fs::symlink_metadata(path).map_err(io_error)?;
    if !metadata.is_dir() || is_reparse(&metadata) {
        return Err("Playnite extension directory is unsafe".into());
    }
    Ok(())
}

fn is_reparse(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x0000_0400 != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

fn validate_target(extensions: &Path, profile: &Path, sid: &str) -> PackageResult<()> {
    if extensions
        .file_name()
        .is_none_or(|name| name != "Extensions")
    {
        return Err("Playnite target is not an Extensions directory".into());
    }
    require_real_directory(profile)?;
    require_target_owner(profile, sid)?;
    let relative = extensions
        .strip_prefix(profile)
        .map_err(|_| "Playnite target escapes authorized profile")?;
    if relative.components().count() > 8
        || relative
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err("Playnite target path is unsafe".into());
    }
    let mut path = profile.to_path_buf();
    for part in relative.components() {
        path.push(part.as_os_str());
        require_real_directory(&path)?;
        require_target_owner(&path, sid)?;
    }
    Ok(())
}

#[cfg(windows)]
fn require_target_owner(path: &Path, sid: &str) -> PackageResult<()> {
    if owner_matches(path, sid)? {
        Ok(())
    } else {
        Err("Playnite target has an untrusted owner".into())
    }
}

#[cfg(not(windows))]
fn require_target_owner(_: &Path, _: &str) -> PackageResult<()> {
    Err("Playnite installation requires Windows owner validation".into())
}

#[cfg(windows)]
fn require_transaction_owner(path: &Path, sid: &str) -> PackageResult<()> {
    let expected = if running_as_system()? {
        "S-1-5-18"
    } else {
        sid
    };
    if owner_matches(path, expected)? {
        Ok(())
    } else {
        Err("Playnite transaction has an untrusted owner".into())
    }
}

#[cfg(windows)]
fn require_active_owner(path: &Path, sid: &str) -> PackageResult<()> {
    if owner_matches(path, sid)? || owner_matches(path, "S-1-5-18")? {
        Ok(())
    } else {
        Err("Playnite active extension has an untrusted owner".into())
    }
}

#[cfg(not(windows))]
fn require_active_owner(_: &Path, _: &str) -> PackageResult<()> {
    Err("Playnite installation requires Windows owner validation".into())
}

#[cfg(windows)]
fn running_as_system() -> PackageResult<bool> {
    use std::ffi::c_void;
    #[repr(C)]
    struct SidAndAttributes {
        sid: *mut c_void,
        attributes: u32,
    }
    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn OpenProcessToken(process: *mut c_void, access: u32, token: *mut *mut c_void) -> i32;
        fn GetTokenInformation(
            token: *mut c_void,
            kind: i32,
            value: *mut c_void,
            length: u32,
            needed: *mut u32,
        ) -> i32;
        fn ConvertStringSidToSidW(text: *const u16, sid: *mut *mut c_void) -> i32;
        fn EqualSid(left: *mut c_void, right: *mut c_void) -> i32;
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentProcess() -> *mut c_void;
        fn CloseHandle(handle: *mut c_void) -> i32;
        fn LocalFree(memory: *mut c_void) -> *mut c_void;
    }
    let mut token = std::ptr::null_mut();
    // SAFETY: GetCurrentProcess returns a pseudo-handle; token receives an owned handle.
    if unsafe { OpenProcessToken(GetCurrentProcess(), 8, &mut token) } == 0 {
        return Err("installer account could not be determined".into());
    }
    let result = (|| {
        let mut needed = 0u32;
        // SAFETY: the first call only requests the required TokenUser buffer size.
        unsafe { GetTokenInformation(token, 1, std::ptr::null_mut(), 0, &mut needed) };
        if needed < std::mem::size_of::<SidAndAttributes>() as u32 || needed > 4096 {
            return Err("installer token has an invalid user SID".into());
        }
        let mut buffer = vec![0u64; (needed as usize).div_ceil(8)];
        // SAFETY: buffer is aligned for TOKEN_USER and has at least needed bytes.
        if unsafe { GetTokenInformation(token, 1, buffer.as_mut_ptr().cast(), needed, &mut needed) }
            == 0
        {
            return Err("installer account could not be determined".into());
        }
        // SAFETY: a successful TokenUser query starts with SID_AND_ATTRIBUTES;
        // its SID pointer remains valid while buffer is live.
        let user = unsafe { &*(buffer.as_ptr().cast::<SidAndAttributes>()) };
        let system_text: Vec<u16> = "S-1-5-18".encode_utf16().chain(Some(0)).collect();
        let mut system_sid = std::ptr::null_mut();
        // SAFETY: system_text is NUL-terminated and system_sid is LocalFree-owned.
        if unsafe { ConvertStringSidToSidW(system_text.as_ptr(), &mut system_sid) } == 0 {
            return Err("LocalSystem SID could not be constructed".into());
        }
        // SAFETY: both SID pointers remain live for the synchronous comparison.
        let matches = unsafe { EqualSid(user.sid, system_sid) } != 0;
        // SAFETY: system_sid was allocated by ConvertStringSidToSidW.
        unsafe { LocalFree(system_sid) };
        Ok(matches)
    })();
    // SAFETY: token was opened by OpenProcessToken and is closed once.
    unsafe { CloseHandle(token) };
    result
}

#[cfg(not(windows))]
fn require_transaction_owner(_: &Path, _: &str) -> PackageResult<()> {
    Err("Playnite installation requires Windows owner validation".into())
}

#[cfg(windows)]
fn grant_user_read_access(path: &Path, sid: &str) -> PackageResult<()> {
    apply_user_access(path, sid, "GRGX")
}

#[cfg(all(test, windows))]
fn grant_user_full_access_for_test(path: &Path, sid: &str) -> PackageResult<()> {
    apply_user_access(path, sid, "FA")
}

#[cfg(windows)]
fn apply_user_access(path: &Path, sid: &str, rights: &str) -> PackageResult<()> {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn ConvertStringSecurityDescriptorToSecurityDescriptorW(
            text: *const u16,
            revision: u32,
            descriptor: *mut *mut c_void,
            length: *mut u32,
        ) -> i32;
        fn SetFileSecurityW(path: *const u16, information: u32, descriptor: *const c_void) -> i32;
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn LocalFree(memory: *mut c_void) -> *mut c_void;
    }
    // The stage remains inside a SYSTEM-private parent until the verified pair
    // is renamed into Playnite's directory. Grant the target user read/execute
    // there and retain installer recovery access for SYSTEM and Administrators.
    let sddl = format!("D:P(A;OICI;{rights};;;{sid})(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)");
    let descriptor_text: Vec<u16> = sddl.encode_utf16().chain(Some(0)).collect();
    let target: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut descriptor = std::ptr::null_mut();
    let mut length = 0;
    // SAFETY: descriptor_text is NUL-terminated and descriptor receives a
    // LocalFree-owned allocation kept live through SetFileSecurityW.
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            descriptor_text.as_ptr(),
            1,
            &mut descriptor,
            &mut length,
        )
    } == 0
    {
        return Err("Playnite user access descriptor is invalid".into());
    }
    // SAFETY: target is NUL-terminated and descriptor remains live for this call.
    let applied = unsafe { SetFileSecurityW(target.as_ptr(), 4, descriptor) };
    // SAFETY: descriptor was allocated by the preceding Windows API call.
    unsafe { LocalFree(descriptor) };
    if applied == 0 {
        return Err("Playnite user read access could not be applied".into());
    }
    Ok(())
}

#[cfg(not(windows))]
fn grant_user_read_access(_: &Path, _: &str) -> PackageResult<()> {
    Err("Playnite installation requires Windows access control".into())
}

#[cfg(windows)]
fn owner_matches(path: &Path, sid: &str) -> PackageResult<bool> {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn ConvertStringSidToSidW(value: *const u16, sid: *mut *mut c_void) -> i32;
        fn GetNamedSecurityInfoW(
            path: *const u16,
            object_type: i32,
            security_information: u32,
            owner: *mut *mut c_void,
            group: *mut *mut c_void,
            dacl: *mut *mut c_void,
            sacl: *mut *mut c_void,
            descriptor: *mut *mut c_void,
        ) -> u32;
        fn EqualSid(left: *mut c_void, right: *mut c_void) -> i32;
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn LocalFree(memory: *mut c_void) -> *mut c_void;
    }
    let path_wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let sid_wide: Vec<u16> = sid.encode_utf16().chain(Some(0)).collect();
    let mut expected_sid: *mut c_void = std::ptr::null_mut();
    // SAFETY: sid_wide is NUL-terminated and expected_sid receives a LocalFree-owned allocation.
    if unsafe { ConvertStringSidToSidW(sid_wide.as_ptr(), &mut expected_sid) } == 0 {
        return Err("authorized Playnite SID is invalid".into());
    }
    let mut owner: *mut c_void = std::ptr::null_mut();
    let mut descriptor: *mut c_void = std::ptr::null_mut();
    // SAFETY: path_wide remains live; owner points inside descriptor until LocalFree.
    let status = unsafe {
        GetNamedSecurityInfoW(
            path_wide.as_ptr(),
            1, // SE_FILE_OBJECT
            1, // OWNER_SECURITY_INFORMATION
            &mut owner,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut descriptor,
        )
    };
    let matches = status == 0 && !owner.is_null() && unsafe { EqualSid(owner, expected_sid) } != 0;
    // SAFETY: these pointers are allocated by the Windows APIs above and are freed once.
    unsafe {
        if !descriptor.is_null() {
            LocalFree(descriptor);
        }
        LocalFree(expected_sid);
    }
    if status != 0 {
        return Err("Playnite target owner could not be read".into());
    }
    Ok(matches)
}

fn io_error(_: std::io::Error) -> String {
    "Playnite extension filesystem operation failed".into()
}

#[cfg(test)]
mod tests;
