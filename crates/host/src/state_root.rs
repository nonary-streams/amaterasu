//! SCM-owned per-user state roots for interactive workers.
//!
//! The service runs as LocalSystem and derives the directory name from the
//! active WTS token. A signed package is verified before this code is reached;
//! the package never chooses this path. The machine migration seed remains
//! SYSTEM/Admin-only and is staged under the protected parent before the
//! user-token process merges it into that user's durable state.

use amaterasu_package::{PackageResult, random_identifier};
use std::ffi::{OsStr, c_void};
use std::fs;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};

mod management_tls;
mod seed;
pub(crate) use management_tls::apply_staged_tls;
pub(crate) use management_tls::protect_development_files;
pub(crate) use seed::apply_pending_seed;

const TOKEN_USER: u32 = 1;
const DACL_SECURITY_INFORMATION: u32 = 0x0000_0004;
const OWNER_SECURITY_INFORMATION: u32 = 0x0000_0001;
const SE_FILE_OBJECT: u32 = 1;
const SE_DACL_PROTECTED: u16 = 0x1000;
const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;
const OBJECT_INHERIT_ACE: u8 = 0x01;
const CONTAINER_INHERIT_ACE: u8 = 0x02;
const FILE_ALL_ACCESS: u32 = 0x001F_01FF;
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
const INVALID_FILE_ATTRIBUTES: u32 = u32::MAX;
const MAX_SID_LENGTH: usize = 184;

#[repr(C)]
struct SidAndAttributes {
    sid: *mut c_void,
    attributes: u32,
}

#[repr(C)]
struct AceHeader {
    ace_type: u8,
    ace_flags: u8,
    ace_size: u16,
}

#[repr(C)]
struct AccessAllowedAce {
    header: AceHeader,
    access_mask: u32,
    sid_start: u32,
}

#[repr(C)]
struct AclSizeInformation {
    ace_count: u32,
    acl_bytes_in_use: u32,
    acl_bytes_free: u32,
}

#[link(name = "advapi32")]
unsafe extern "system" {
    fn GetTokenInformation(
        token: *mut c_void,
        class: u32,
        buffer: *mut c_void,
        length: u32,
        returned: *mut u32,
    ) -> i32;
    fn ConvertStringSecurityDescriptorToSecurityDescriptorW(
        source: *const u16,
        revision: u32,
        descriptor: *mut *mut c_void,
        length: *mut u32,
    ) -> i32;
    fn SetFileSecurityW(path: *const u16, information: u32, descriptor: *const c_void) -> i32;
    fn GetNamedSecurityInfoW(
        object: *const u16,
        object_type: u32,
        information: u32,
        owner: *mut *mut c_void,
        group: *mut *mut c_void,
        dacl: *mut *mut c_void,
        sacl: *mut *mut c_void,
        descriptor: *mut *mut c_void,
    ) -> u32;
    fn GetSecurityDescriptorControl(
        descriptor: *const c_void,
        control: *mut u16,
        revision: *mut u32,
    ) -> i32;
    #[cfg(test)]
    fn GetSecurityDescriptorDacl(
        descriptor: *const c_void,
        present: *mut i32,
        dacl: *mut *mut c_void,
        defaulted: *mut i32,
    ) -> i32;
    fn GetAclInformation(
        acl: *const c_void,
        information: *mut c_void,
        length: u32,
        information_class: u32,
    ) -> i32;
    fn GetAce(acl: *const c_void, index: u32, entry: *mut *mut c_void) -> i32;
    fn EqualSid(left: *const c_void, right: *const c_void) -> i32;
    fn ConvertStringSidToSidW(text: *const u16, sid: *mut *mut c_void) -> i32;
}

#[link(name = "advapi32")]
unsafe extern "system" {
    fn ConvertSidToStringSidW(sid: *mut c_void, text: *mut *mut u16) -> i32;
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn LocalFree(memory: *mut c_void) -> *mut c_void;
    fn GetFileAttributesW(path: *const u16) -> u32;
    fn GetCurrentProcess() -> *mut c_void;
    fn CloseHandle(handle: *mut c_void) -> i32;
}

#[link(name = "advapi32")]
unsafe extern "system" {
    fn OpenProcessToken(process: *mut c_void, access: u32, token: *mut *mut c_void) -> i32;
}

pub(crate) struct PreparedUserState {
    pub(crate) root: PathBuf,
    pub(crate) seed: Option<PathBuf>,
}

/// Prepare the only parent a provider may use for durable Steam artwork.
/// The worker runs as the active user, so this uses the same protected SID
/// boundary as its state root. A package never supplies either path segment.
pub(crate) fn prepare_steam_artwork_cache_parent(state_root: &Path) -> PackageResult<PathBuf> {
    let state_root = canonical_real_directory(state_root, "worker state root")?;
    let mut token = std::ptr::null_mut();
    // SAFETY: the current process pseudo-handle is valid and the output receives
    // one owned token handle, closed below.
    if unsafe { OpenProcessToken(GetCurrentProcess(), 8, &mut token) } == 0 {
        return Err("worker token is unavailable".into());
    }
    let sid = sid_string(token);
    // SAFETY: OpenProcessToken returned this owned handle.
    unsafe { CloseHandle(token) };
    let sid = sid?;
    let sddl = format!("D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FA;;;{sid})");
    validate_owner(&state_root, &[&sid, "S-1-5-18"])?;
    validate_dacl(&state_root, &sddl)?;
    let mut parent = state_root;
    for segment in ["provider-cache", "steam"] {
        parent = parent.join(segment);
        if !parent.try_exists().map_err(|error| error.to_string())? {
            create_real_directory(&parent)?;
        }
        parent = canonical_real_directory(&parent, "Steam artwork cache parent")?;
        validate_owner(&parent, &[&sid])?;
        set_dacl(&parent, &sddl)?;
    }
    Ok(parent)
}

fn validate_owner(path: &Path, allowed_sids: &[&str]) -> PackageResult<()> {
    let path_wide = wide(path.as_os_str())?;
    let mut owner = std::ptr::null_mut();
    let mut descriptor = std::ptr::null_mut();
    // SAFETY: the path is NUL-terminated. The owner is borrowed from the
    // LocalFree-owned descriptor until this function releases it.
    let status = unsafe {
        GetNamedSecurityInfoW(
            path_wide.as_ptr(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION,
            &mut owner,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut descriptor,
        )
    };
    if status != 0 || descriptor.is_null() || owner.is_null() {
        return Err("Steam artwork cache owner is unavailable".into());
    }
    let matches = allowed_sids.iter().any(|sid| {
        let Ok(expected) = sid_from_string(sid) else {
            return false;
        };
        // SAFETY: both SIDs remain live through this call.
        (unsafe { EqualSid(owner, expected.0) }) != 0
    });
    // SAFETY: GetNamedSecurityInfoW allocated the descriptor containing owner.
    unsafe { LocalFree(descriptor) };
    if !matches {
        return Err("Steam artwork cache has an untrusted owner".into());
    }
    Ok(())
}

/// Check provisioned browser TLS material without changing its security descriptor.
/// Every ancestor is checked by the loading boundary before this call.
pub(crate) fn validate_management_tls_acl(path: &Path) -> PackageResult<()> {
    let mut token = std::ptr::null_mut();
    // SAFETY: the process pseudo-handle is valid; the returned token is closed below.
    if unsafe { OpenProcessToken(GetCurrentProcess(), 8, &mut token) } == 0 {
        return Err("management TLS worker token is unavailable".into());
    }
    let sid = sid_string(token);
    // SAFETY: OpenProcessToken returned this owned token handle.
    unsafe { CloseHandle(token) };
    let sid = sid?;
    validate_tls_acl(
        path,
        &[&sid, "S-1-5-18"],
        &[
            (&sid, FILE_ALL_ACCESS),
            ("S-1-5-18", FILE_ALL_ACCESS),
            ("S-1-5-32-544", FILE_ALL_ACCESS),
        ],
        false,
    )
}

fn set_tls_stage_dacl(path: &Path, sid: &str) -> PackageResult<()> {
    let sddl = format!("D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;GR;;;{sid})");
    apply_dacl(path, &sddl)?;
    validate_tls_stage_acl(path, sid)
}

fn validate_tls_stage_acl(path: &Path, sid: &str) -> PackageResult<()> {
    validate_tls_acl(
        path,
        &["S-1-5-18"],
        &[
            (sid, 0x8000_0000),
            ("S-1-5-18", FILE_ALL_ACCESS),
            ("S-1-5-32-544", FILE_ALL_ACCESS),
        ],
        false,
    )
}

fn validate_machine_tls_acl(path: &Path) -> PackageResult<()> {
    validate_tls_acl(
        path,
        &["S-1-5-18", "S-1-5-32-544"],
        &[
            ("S-1-5-18", FILE_ALL_ACCESS),
            ("S-1-5-32-544", FILE_ALL_ACCESS),
        ],
        false,
    )
}

fn validate_inherited_machine_tls_file_acl(path: &Path) -> PackageResult<()> {
    validate_tls_acl(
        path,
        &["S-1-5-18", "S-1-5-32-544"],
        &[
            ("S-1-5-18", FILE_ALL_ACCESS),
            ("S-1-5-32-544", FILE_ALL_ACCESS),
        ],
        true,
    )
}

fn validate_tls_acl(
    path: &Path,
    owners: &[&str],
    expected: &[(&str, u32)],
    inherited: bool,
) -> PackageResult<()> {
    validate_owner(path, owners)?;
    let path_wide = wide(path.as_os_str())?;
    let mut dacl = std::ptr::null_mut();
    let mut descriptor = std::ptr::null_mut();
    // SAFETY: path is NUL-terminated. The descriptor owns dacl until LocalFree.
    let status = unsafe {
        GetNamedSecurityInfoW(
            path_wide.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut dacl,
            std::ptr::null_mut(),
            &mut descriptor,
        )
    };
    if status != 0 || descriptor.is_null() || dacl.is_null() {
        return Err("management TLS ACL is unavailable".into());
    }
    let result = validate_management_tls_descriptor(descriptor, dacl, expected, inherited);
    // SAFETY: GetNamedSecurityInfoW allocated this descriptor.
    unsafe { LocalFree(descriptor) };
    result
}

fn validate_management_tls_descriptor(
    descriptor: *const c_void,
    dacl: *const c_void,
    expected: &[(&str, u32)],
    inherited: bool,
) -> PackageResult<()> {
    let mut control = 0u16;
    let mut revision = 0u32;
    // SAFETY: descriptor remains live through this function.
    if unsafe { GetSecurityDescriptorControl(descriptor, &mut control, &mut revision) } == 0
        || (control & SE_DACL_PROTECTED != 0) == inherited
    {
        return Err("management TLS ACL is not protected".into());
    }
    let mut information = AclSizeInformation {
        ace_count: 0,
        acl_bytes_in_use: 0,
        acl_bytes_free: 0,
    };
    // SAFETY: dacl belongs to the live descriptor; information is writable.
    if unsafe {
        GetAclInformation(
            dacl,
            (&mut information as *mut AclSizeInformation).cast(),
            std::mem::size_of::<AclSizeInformation>() as u32,
            2,
        )
    } == 0
        || information.ace_count as usize != expected.len()
    {
        return Err("management TLS ACL has unexpected entries".into());
    }
    for (expected_sid, expected_mask) in expected {
        let expected_sid = sid_from_string(expected_sid)?;
        let found = (0..information.ace_count).any(|index| {
            let mut ace = std::ptr::null_mut();
            // SAFETY: index is below the ACL-reported count.
            if unsafe { GetAce(dacl, index, &mut ace) } == 0 || ace.is_null() {
                return false;
            }
            // SAFETY: GetAce returned a complete ACE with a fixed header.
            let allowed = unsafe { &*(ace.cast::<AccessAllowedAce>()) };
            allowed.header.ace_type == ACCESS_ALLOWED_ACE_TYPE
                && (allowed.header.ace_flags & 0x10 != 0) == inherited
                && allowed.access_mask == *expected_mask
                // SAFETY: the SID begins at sid_start in AccessAllowedAce.
                && unsafe { EqualSid((&allowed.sid_start as *const u32).cast(), expected_sid.0) } != 0
        });
        if !found {
            return Err("management TLS ACL grants an unexpected principal".into());
        }
    }
    Ok(())
}

/// Prepares a user root and a separate SID-readable migration snapshot.
pub(crate) fn prepare(
    root: &Path,
    digest: &str,
    token: *mut c_void,
) -> PackageResult<PreparedUserState> {
    let root = canonical_real_directory(root, "runtime root")?;
    ensure_machine_private(&root)?;
    let sid = sid_string(token)?;
    if !valid_sid(&sid) {
        return Err("active user token has an invalid SID".into());
    }

    let container = root.join("user-state");
    ensure_directory(&container, "D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)")?;
    let mut destination = container.join(&sid);
    let existing = destination
        .try_exists()
        .map_err(|error| error.to_string())?;
    if existing {
        destination = canonical_real_directory(&destination, "user state root")?;
        set_dacl(
            &destination,
            &format!("D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FA;;;{sid})"),
        )?;
    }

    if !existing {
        destination = materialize_empty(&container, &destination, &sid)?;
    }

    let bootstrap: amaterasu_package::BootstrapConfig =
        amaterasu_package::read_json(&root.join("bootstrap.json"))?;
    let package = amaterasu_package::verify_package(
        &amaterasu_package::package_directory(&root, digest)?,
        &bootstrap,
        amaterasu_package::VerificationPolicy {
            minimum_sequence: 0,
            allow_historical: true,
            now: amaterasu_package::unix_timestamp(),
        },
    )?;
    if package.digest != digest || !package.signed {
        return Err("selected management TLS package is not authenticated".into());
    }
    let active_tls = crate::loading::management_tls::active_file_names(&package.runtime_config)?;
    management_tls::stage_for_user(&root, &container, &sid, active_tls.as_ref())?;

    // The service never writes through an existing SID-controlled tree.
    // It stages under a SYSTEM-only parent; the worker applies the snapshot.
    let data = root.join("data");
    ensure_machine_private(&data)?;
    let seed = data.join("state");
    if !seed.try_exists().map_err(|error| error.to_string())? {
        return Ok(PreparedUserState {
            root: destination,
            seed: None,
        });
    }
    let seed = canonical_real_directory(&seed, "pending migration seed")?;
    ensure_machine_private(&seed)?;
    if !claim_pending_seed(&data, &sid)? {
        return Ok(PreparedUserState {
            root: destination,
            seed: None,
        });
    }
    let staged = stage_seed_snapshot(&seed, &container, &sid)?;
    Ok(PreparedUserState {
        root: destination,
        seed: Some(staged),
    })
}

/// The machine seed can gain missing namespace values on a later validated
/// import. Replace the previous snapshot only after the new copy is ready.
fn stage_seed_snapshot(seed: &Path, container: &Path, sid: &str) -> PackageResult<PathBuf> {
    remove_abandoned_pending_snapshots(container)?;
    let staged = container.join(format!(".seed-{sid}"));
    let pending = container.join(format!(".pending-{}", random_identifier()?));
    create_real_directory(&pending)?;
    let result = copy_real_tree(seed, &pending);
    if let Err(error) = result {
        let _ = fs::remove_dir_all(&pending);
        return Err(error);
    }
    let result = (|| {
        set_dacl(
            &pending,
            &format!("D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FA;;;{sid})"),
        )?;
        secure_user_directories(&pending, sid)?;
        if staged.try_exists().map_err(|error| error.to_string())? {
            canonical_real_directory(&staged, "existing staged migration seed")?;
            validate_real_tree(&staged)?;
            fs::remove_dir_all(&staged).map_err(|error| error.to_string())?;
        }
        fs::rename(&pending, &staged).map_err(|error| error.to_string())?;
        Ok(staged)
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&pending);
    }
    result
}

/// A process can stop during the copy, before its temporary snapshot is
/// renamed. The service holds the runtime lock while staging, so no pending
/// snapshot is active when the next preparation starts.
fn remove_abandoned_pending_snapshots(container: &Path) -> PackageResult<()> {
    for entry in fs::read_dir(container).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        if !entry.file_name().to_string_lossy().starts_with(".pending-") {
            continue;
        }
        let path = entry.path();
        canonical_real_directory(&path, "abandoned staged migration seed")?;
        validate_real_tree(&path)?;
        // Rust's Windows remove_dir_all uses handle-relative traversal and
        // does not follow links introduced after the checks above.
        fs::remove_dir_all(&path).map_err(|error| error.to_string())?;
    }
    Ok(())
}

/// Refuse to recursively remove a previous user-readable stage if it contains
/// links, junctions, or other entries outside the regular seed format.
fn validate_real_tree(root: &Path) -> PackageResult<()> {
    reject_reparse(root)?;
    for entry in fs::read_dir(root).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path).map_err(|error| error.to_string())?;
        reject_reparse(&path)?;
        if metadata.is_dir() {
            validate_real_tree(&path)?;
        } else if !metadata.is_file() {
            return Err("staged migration seed contains a non-regular entry".into());
        }
    }
    Ok(())
}

/// The seed can include the migrated administrator verifier and pairing
/// identity. It is therefore consumed by exactly one SID, never duplicated to
/// every account that subsequently signs in. The caller holds the runtime root
/// lock while this `create_new` marker is written; create-new also makes the
/// boundary safe if this helper is reused outside that service loop.
fn claim_pending_seed(data: &Path, sid: &str) -> PackageResult<bool> {
    let claim = data.join("state-seed-claim");
    if claim.try_exists().map_err(|error| error.to_string())? {
        let owner = read_seed_owner(&claim)?;
        // A crash after claiming but before publishing the per-user root must
        // not permanently strand imported credentials. The caller holds the
        // runtime root lock; an existing destination can still need a merge.
        return Ok(owner == sid);
    }
    write_seed_owner(&claim, sid)?;
    Ok(true)
}

fn write_seed_owner(path: &Path, sid: &str) -> PackageResult<()> {
    use std::io::Write;
    let temporary = path.with_file_name(format!(".seed-marker-{}", random_identifier()?));
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .map_err(|error| error.to_string())?;
        file.write_all(sid.as_bytes())
            .and_then(|()| file.sync_all())
            .map_err(|error| error.to_string())?;
        fs::rename(&temporary, path).map_err(|error| error.to_string())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn read_seed_owner(path: &Path) -> PackageResult<String> {
    let metadata = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("migration seed marker is invalid".into());
    }
    reject_reparse(path)?;
    if metadata.len() == 0 || metadata.len() > MAX_SID_LENGTH as u64 {
        return Err("migration seed marker is invalid".into());
    }
    let owner = fs::read_to_string(path).map_err(|error| error.to_string())?;
    if !valid_sid(&owner) || owner.len() as u64 != metadata.len() {
        return Err("migration seed marker is invalid".into());
    }
    Ok(owner)
}

fn materialize_empty(container: &Path, destination: &Path, sid: &str) -> PackageResult<PathBuf> {
    let pending = container.join(format!(".pending-{}", random_identifier()?));
    create_real_directory(&pending)?;
    set_dacl(
        &pending,
        &format!("D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FA;;;{sid})"),
    )?;
    fs::rename(&pending, destination).map_err(|error| error.to_string())?;
    canonical_real_directory(destination, "new user state root")
}

fn copy_real_tree(source: &Path, destination: &Path) -> PackageResult<()> {
    for entry in fs::read_dir(source).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        let source_path = entry.path();
        let metadata = fs::symlink_metadata(&source_path).map_err(|error| error.to_string())?;
        reject_reparse(&source_path)?;
        let destination_path = destination.join(entry.file_name());
        if metadata.is_dir() {
            create_real_directory(&destination_path)?;
            copy_real_tree(&source_path, &destination_path)?;
        } else if metadata.is_file() {
            fs::copy(&source_path, &destination_path).map_err(|error| error.to_string())?;
        } else {
            return Err("migration seed contains a non-regular entry".into());
        }
    }
    Ok(())
}

fn secure_user_directories(root: &Path, sid: &str) -> PackageResult<()> {
    for entry in fs::read_dir(root).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path).map_err(|error| error.to_string())?;
        reject_reparse(&path)?;
        if metadata.is_dir() {
            set_user_dacl(&path, sid)?;
            secure_user_directories(&path, sid)?;
        } else if metadata.is_file() {
            set_user_dacl(&path, sid)?;
        } else {
            return Err("migration seed contains a non-regular entry".into());
        }
    }
    Ok(())
}

fn set_user_dacl(path: &Path, sid: &str) -> PackageResult<()> {
    set_dacl(
        path,
        &format!("D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FA;;;{sid})"),
    )
}

fn ensure_directory(path: &Path, sddl: &str) -> PackageResult<()> {
    if !path.try_exists().map_err(|error| error.to_string())? {
        create_real_directory(path)?;
    }
    let path = canonical_real_directory(path, "state root container")?;
    set_dacl(&path, sddl)
}

/// Machine-owned data, recovery records, and importer journals inherit this
/// protected ACL. It is applied to existing roots as well, so a permissive
/// pre-service creation cannot survive into the SYSTEM transaction.
fn ensure_machine_private(path: &Path) -> PackageResult<()> {
    if !path.try_exists().map_err(|error| error.to_string())? {
        create_real_directory(path)?;
    }
    let path = canonical_real_directory(path, "machine-private state directory")?;
    set_dacl(&path, "D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)")
}

fn create_real_directory(path: &Path) -> PackageResult<()> {
    fs::create_dir(path).map_err(|error| error.to_string())?;
    reject_reparse(path)
}

fn canonical_real_directory(path: &Path, description: &str) -> PackageResult<PathBuf> {
    let metadata = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if !metadata.is_dir() {
        return Err(format!("{description} is not a directory"));
    }
    reject_reparse(path)?;
    let canonical = path.canonicalize().map_err(|error| error.to_string())?;
    if !same_windows_path(&canonical, path) {
        return Err(format!("{description} contains a reparse point"));
    }
    Ok(canonical)
}

fn same_windows_path(left: &Path, right: &Path) -> bool {
    fn normalize(path: &Path) -> String {
        let path = path.to_string_lossy();
        path.strip_prefix(r"\\?\")
            .unwrap_or(&path)
            .replace('/', "\\")
            .to_ascii_lowercase()
    }
    normalize(left) == normalize(right)
}

fn reject_reparse(path: &Path) -> PackageResult<()> {
    let path = wide(path.as_os_str())?;
    // SAFETY: `path` is NUL-terminated for this synchronous Win32 call.
    let attributes = unsafe { GetFileAttributesW(path.as_ptr()) };
    if attributes == INVALID_FILE_ATTRIBUTES || attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err("state root contains a reparse point".into());
    }
    Ok(())
}

fn sid_string(token: *mut c_void) -> PackageResult<String> {
    let mut length = 0u32;
    // SAFETY: sizing call intentionally supplies no output buffer.
    unsafe { GetTokenInformation(token, TOKEN_USER, std::ptr::null_mut(), 0, &mut length) };
    if length < std::mem::size_of::<SidAndAttributes>() as u32 {
        return Err("active user token has no SID".into());
    }
    let mut buffer = vec![0u8; length as usize];
    // SAFETY: `buffer` has exactly the size requested by Windows.
    if unsafe {
        GetTokenInformation(
            token,
            TOKEN_USER,
            buffer.as_mut_ptr().cast(),
            length,
            &mut length,
        )
    } == 0
    {
        return Err("active user token SID is unavailable".into());
    }
    // SAFETY: TOKEN_USER writes a SID_AND_ATTRIBUTES at the buffer start.
    let sid = unsafe { &*(buffer.as_ptr().cast::<SidAndAttributes>()) }.sid;
    if sid.is_null() {
        return Err("active user token SID is unavailable".into());
    }
    let mut text = std::ptr::null_mut();
    // SAFETY: Windows allocates a NUL-terminated SID string released with LocalFree.
    if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 || text.is_null() {
        return Err("active user token SID is unavailable".into());
    }
    let mut length = 0usize;
    // SAFETY: `text` points to a NUL-terminated UTF-16 string allocated by Windows.
    while unsafe { *text.add(length) } != 0 {
        length += 1;
    }
    // SAFETY: the range was found before the terminator and is still owned locally.
    let value = String::from_utf16(unsafe { std::slice::from_raw_parts(text, length) })
        .map_err(|_| "active user token SID is invalid")?;
    // SAFETY: ConvertSidToStringSidW allocated this exact buffer.
    unsafe { LocalFree(text.cast()) };
    Ok(value)
}

fn set_dacl(path: &Path, sddl: &str) -> PackageResult<()> {
    apply_dacl(path, sddl)?;
    validate_dacl(path, sddl)
}

fn apply_dacl(path: &Path, sddl: &str) -> PackageResult<()> {
    let sddl_wide = wide(OsStr::new(sddl))?;
    let path_wide = wide(path.as_os_str())?;
    let mut descriptor = std::ptr::null_mut();
    let mut length = 0u32;
    // SAFETY: SDDL is NUL-terminated; Windows allocates the descriptor on success.
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl_wide.as_ptr(),
            1,
            &mut descriptor,
            &mut length,
        )
    } == 0
        || descriptor.is_null()
    {
        return Err("state root ACL could not be constructed".into());
    }
    // SAFETY: both buffers remain valid for this synchronous call.
    let applied =
        unsafe { SetFileSecurityW(path_wide.as_ptr(), DACL_SECURITY_INFORMATION, descriptor) };
    // SAFETY: the security descriptor came from ConvertStringSecurityDescriptorToSecurityDescriptorW.
    unsafe { LocalFree(descriptor) };
    if applied == 0 {
        return Err("state root ACL could not be applied".into());
    }
    Ok(())
}

/// Reads the effective descriptor back from the filesystem and accepts only
/// the protected allow ACEs we wrote. This catches ACL inheritance, a failed
/// replacement, or an unexpected broad grant before a user worker receives a
/// state path.
fn validate_dacl(path: &Path, sddl: &str) -> PackageResult<()> {
    let expected = expected_sids(sddl)?;
    let path_wide = wide(path.as_os_str())?;
    let mut dacl = std::ptr::null_mut();
    let mut descriptor = std::ptr::null_mut();
    // SAFETY: path is NUL-terminated; Windows returns a LocalFree-owned descriptor.
    let status = unsafe {
        GetNamedSecurityInfoW(
            path_wide.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut dacl,
            std::ptr::null_mut(),
            &mut descriptor,
        )
    };
    if status != 0 || descriptor.is_null() || dacl.is_null() {
        return Err("state root ACL could not be read back".into());
    }
    let result = validate_descriptor(descriptor, dacl, &expected);
    // SAFETY: GetNamedSecurityInfoW allocated this descriptor on success.
    unsafe { LocalFree(descriptor) };
    result
}

fn expected_sids(sddl: &str) -> PackageResult<Vec<String>> {
    let mut values = vec!["S-1-5-18".to_owned(), "S-1-5-32-544".to_owned()];
    if let Some(user) = sddl
        .split(";;;")
        .nth(3)
        .and_then(|value| value.strip_suffix(')'))
    {
        values.push(user.to_owned());
    }
    Ok(values)
}

fn validate_descriptor(
    descriptor: *const c_void,
    dacl: *const c_void,
    expected: &[String],
) -> PackageResult<()> {
    let mut control = 0u16;
    let mut revision = 0u32;
    // SAFETY: descriptor came from GetNamedSecurityInfoW and remains live here.
    if unsafe { GetSecurityDescriptorControl(descriptor, &mut control, &mut revision) } == 0
        || control & SE_DACL_PROTECTED == 0
    {
        return Err("state root ACL is not protected".into());
    }
    let mut information = AclSizeInformation {
        ace_count: 0,
        acl_bytes_in_use: 0,
        acl_bytes_free: 0,
    };
    // SAFETY: DACL pointer belongs to the live security descriptor.
    if unsafe {
        GetAclInformation(
            dacl,
            (&mut information as *mut AclSizeInformation).cast(),
            std::mem::size_of::<AclSizeInformation>() as u32,
            2,
        )
    } == 0
        || information.ace_count as usize != expected.len()
    {
        return Err("state root ACL has unexpected entries".into());
    }
    for expected_sid in expected {
        let expected_sid = sid_from_string(expected_sid)?;
        let found = (0..information.ace_count).any(|index| {
            let mut ace = std::ptr::null_mut();
            // SAFETY: index is below the ACL-reported ACE count.
            if unsafe { GetAce(dacl, index, &mut ace) } == 0 || ace.is_null() {
                return false;
            }
            // SAFETY: GetAce returned one complete ACE; validate header before tail access.
            let allowed = unsafe { &*(ace.cast::<AccessAllowedAce>()) };
            allowed.header.ace_type == ACCESS_ALLOWED_ACE_TYPE
                && allowed.header.ace_flags == OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE
                && allowed.access_mask == FILE_ALL_ACCESS
                // SAFETY: AccessAllowedAce contains the SID immediately after the fixed fields.
                && unsafe { EqualSid((&allowed.sid_start as *const u32).cast(), expected_sid.0) } != 0
        });
        if !found {
            return Err("state root ACL does not grant exactly the required SID".into());
        }
    }
    Ok(())
}

struct LocalSid(*mut c_void);

impl Drop for LocalSid {
    fn drop(&mut self) {
        // SAFETY: ConvertStringSidToSidW allocated this exact SID buffer.
        unsafe { LocalFree(self.0) };
    }
}

fn sid_from_string(value: &str) -> PackageResult<LocalSid> {
    let value = wide(OsStr::new(value))?;
    let mut sid = std::ptr::null_mut();
    // SAFETY: source is NUL-terminated and Windows allocates the returned SID.
    if unsafe { ConvertStringSidToSidW(value.as_ptr(), &mut sid) } == 0 || sid.is_null() {
        return Err("state root ACL contains an invalid SID".into());
    }
    Ok(LocalSid(sid))
}

fn wide(value: &OsStr) -> PackageResult<Vec<u16>> {
    let mut text: Vec<u16> = value.encode_wide().collect();
    if text.contains(&0) {
        return Err("Windows path contains a NUL character".into());
    }
    text.push(0);
    Ok(text)
}

fn valid_sid(value: &str) -> bool {
    value.starts_with("S-1-")
        && value.len() <= MAX_SID_LENGTH
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b'-' || byte == b'S')
}

#[cfg(test)]
mod tests {
    use super::seed::{SeedNamespace, read_seed_namespace};
    use super::{
        AccessAllowedAce, CloseHandle, ConvertStringSecurityDescriptorToSecurityDescriptorW,
        GetCurrentProcess, GetSecurityDescriptorDacl, LocalFree, OpenProcessToken,
        SetFileSecurityW, canonical_real_directory, claim_pending_seed, copy_real_tree,
        expected_sids, prepare_steam_artwork_cache_parent, same_windows_path, set_dacl, sid_string,
        stage_seed_snapshot, valid_sid, validate_dacl, validate_descriptor, wide,
    };
    use serde_json::Value;
    use std::collections::BTreeMap;
    use std::ffi::{OsStr, c_void};
    use std::fs;

    fn current_sid() -> String {
        let mut token = std::ptr::null_mut();
        // SAFETY: the pseudo process handle is valid and OpenProcessToken writes one handle.
        assert_ne!(
            unsafe { OpenProcessToken(GetCurrentProcess(), 8, &mut token) },
            0
        );
        let sid = sid_string(token).unwrap();
        // SAFETY: OpenProcessToken returned this owned handle.
        unsafe { CloseHandle(token) };
        sid
    }

    fn make_permissive(path: &std::path::Path) {
        let sddl = wide(OsStr::new("D:P(A;OICI;FA;;;WD)")).unwrap();
        let path = wide(path.as_os_str()).unwrap();
        let mut descriptor = std::ptr::null_mut();
        let mut length = 0;
        // SAFETY: SDDL is NUL-terminated; Windows allocates descriptor on success.
        assert_ne!(
            unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    sddl.as_ptr(),
                    1,
                    &mut descriptor,
                    &mut length,
                )
            },
            0
        );
        // SAFETY: buffers remain valid for this synchronous security update.
        assert_ne!(unsafe { SetFileSecurityW(path.as_ptr(), 4, descriptor) }, 0);
        // SAFETY: conversion API allocated this descriptor.
        unsafe { LocalFree(descriptor) };
    }

    #[test]
    fn accepts_only_string_sids_safe_for_a_directory_component() {
        assert!(valid_sid("S-1-5-21-123-456-789-1001"));
        assert!(!valid_sid("S-1-5-21-.."));
        assert!(!valid_sid("S-1-5-21-1001\\escape"));
    }

    #[test]
    fn steam_cache_parent_is_private_and_stable_across_restart() {
        let root = std::env::temp_dir().join(format!(
            "amaterasu-steam-host-cache-{}-{}",
            std::process::id(),
            amaterasu_package::random_identifier().unwrap()
        ));
        fs::create_dir(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let sddl = format!(
            "D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FA;;;{})",
            current_sid()
        );
        make_permissive(&root);
        assert!(prepare_steam_artwork_cache_parent(&root).is_err());
        set_dacl(&root, &sddl).unwrap();
        let first = prepare_steam_artwork_cache_parent(&root).unwrap();
        assert_eq!(first, root.join("provider-cache").join("steam"));
        validate_dacl(&root.join("provider-cache"), &sddl).unwrap();
        validate_dacl(&first, &sddl).unwrap();
        assert_eq!(prepare_steam_artwork_cache_parent(&root).unwrap(), first);
        fs::write(root.join("unrelated.txt"), b"keep").unwrap();
        assert_eq!(fs::read(root.join("unrelated.txt")).unwrap(), b"keep");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn pending_seed_copy_isolated_from_its_machine_source() {
        let root =
            std::env::temp_dir().join(format!("amaterasu-state-root-copy-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let seed = root.join("seed");
        let destination = root.join("destination");
        fs::create_dir_all(seed.join("namespaces")).unwrap();
        fs::write(seed.join("namespaces/state.json"), b"seed").unwrap();
        fs::create_dir(&destination).unwrap();
        copy_real_tree(&seed, &destination).unwrap();
        fs::write(destination.join("namespaces/state.json"), b"user-change").unwrap();
        assert_eq!(
            fs::read(seed.join("namespaces/state.json")).unwrap(),
            b"seed"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn interrupted_stage_refreshes_new_seed_keys_and_preserves_user_credential() {
        let root = std::env::temp_dir().join(format!(
            "amaterasu-state-root-refreshed-stage-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let machine = root.join("machine");
        let container = root.join("container");
        let user = root.join("user");
        let namespace = "616d617465726173752e61757468656e7469636174696f6e";
        let relative = std::path::Path::new("namespaces")
            .join(namespace)
            .join("state.json");
        let pairing_namespace: String = "amaterasu.pairing"
            .as_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let pairing_relative = std::path::Path::new("namespaces")
            .join(pairing_namespace)
            .join("state.json");
        fs::create_dir_all(machine.join("namespaces").join(namespace)).unwrap();
        fs::create_dir_all(user.join("namespaces").join(namespace)).unwrap();
        fs::create_dir(&container).unwrap();
        fs::write(
            machine.join(&relative),
            br#"{"format_version":1,"revision":1,"values":{"authentication/v1":{"credential":"imported"}}}"#,
        )
        .unwrap();
        fs::write(
            user.join(&relative),
            br#"{"format_version":1,"revision":7,"values":{"authentication/v1":{"credential":"current"}}}"#,
        )
        .unwrap();
        let sid = current_sid();
        let staged = stage_seed_snapshot(&machine, &container, &sid).unwrap();
        assert!(!staged.join(&pairing_relative).exists());
        let orphan = container.join(".pending-abandoned");
        fs::create_dir(&orphan).unwrap();
        fs::write(orphan.join("partial"), b"partial copy").unwrap();
        assert!(stage_seed_snapshot(&root.join("missing-seed"), &container, &sid).is_err());
        assert!(staged.join(&relative).exists());

        // A later validated import added pairing after the first worker exited
        // before applying its stage.
        fs::create_dir_all(machine.join(pairing_relative.parent().unwrap())).unwrap();
        fs::write(
            machine.join(&pairing_relative),
            br#"{"format_version":1,"revision":1,"values":{"pairing/v1":{"identity":"new"}}}"#,
        )
        .unwrap();
        assert_eq!(
            stage_seed_snapshot(&machine, &container, &sid).unwrap(),
            staged
        );
        assert!(!orphan.exists());
        super::apply_pending_seed(&staged, &user).unwrap();
        let result = read_seed_namespace(&user.join(&relative)).unwrap();
        assert_eq!(result.values["authentication/v1"]["credential"], "current");
        assert_eq!(result.revision, 7);
        let pairing = read_seed_namespace(&user.join(&pairing_relative)).unwrap();
        assert_eq!(pairing.values["pairing/v1"]["identity"], "new");
        assert!(user.join(".migration-seed-applied").exists());

        // A later validated import refreshes the service snapshot. The same
        // user receives its missing values on the next worker start, while
        // the local credential remains authoritative.
        fs::write(
            machine.join(&relative),
            br#"{"format_version":1,"revision":2,"values":{"authentication/v1":{"credential":"imported"},"authentication/new/v1":{"credential":"later"}}}"#,
        )
        .unwrap();
        stage_seed_snapshot(&machine, &container, &sid).unwrap();
        super::apply_pending_seed(&staged, &user).unwrap();
        let result = read_seed_namespace(&user.join(&relative)).unwrap();
        assert_eq!(result.values["authentication/v1"]["credential"], "current");
        assert_eq!(
            result.values["authentication/new/v1"]["credential"],
            "later"
        );
        let revision = result.revision;
        super::apply_pending_seed(&staged, &user).unwrap();
        assert_eq!(
            read_seed_namespace(&user.join(&relative)).unwrap().revision,
            revision
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn legacy_completion_marker_replays_once_and_upgrades() {
        let root = std::env::temp_dir().join(format!(
            "amaterasu-state-root-legacy-marker-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let seed = root.join("seed");
        let user = root.join("user");
        let namespace = "616d617465726173752e61757468656e7469636174696f6e";
        fs::create_dir_all(seed.join("namespaces").join(namespace)).unwrap();
        fs::create_dir_all(&user).unwrap();
        fs::write(user.join(".migration-seed-applied"), b"S-1-5-18").unwrap();
        fs::write(
            seed.join("namespaces").join(namespace).join("state.json"),
            br#"{"format_version":1,"revision":1,"values":{"authentication/v1":{"credential":"imported"}}}"#,
        )
        .unwrap();

        super::apply_pending_seed(&seed, &user).unwrap();
        let path = user.join("namespaces").join(namespace).join("state.json");
        assert_eq!(
            read_seed_namespace(&path).unwrap().values["authentication/v1"]["credential"],
            "imported"
        );
        let marker: serde_json::Value =
            serde_json::from_slice(&fs::read(user.join(".migration-seed-applied")).unwrap())
                .unwrap();
        assert_eq!(marker["version"], 1);
        fs::remove_file(path).unwrap();
        super::apply_pending_seed(&seed, &user).unwrap();
        assert!(
            !user
                .join("namespaces")
                .join(namespace)
                .join("state.json")
                .exists()
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn existing_user_state_receives_only_missing_seed_files_once() {
        let root = std::env::temp_dir().join(format!(
            "amaterasu-state-root-existing-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let seed = root.join("seed");
        let user = root.join("user");
        let data = root.join("data");
        let namespace = "616d617465726173752e61757468656e7469636174696f6e";
        fs::create_dir_all(seed.join("namespaces").join(namespace)).unwrap();
        fs::create_dir_all(user.join("namespaces").join(namespace)).unwrap();
        fs::create_dir(&data).unwrap();
        fs::write(
            seed.join("namespaces").join(namespace).join("state.json"),
            br#"{"format_version":1,"revision":1,"values":{"authentication/v1":{"credential":"imported"}}}"#,
        )
        .unwrap();
        fs::write(seed.join("host.lock"), b"source-lock").unwrap();
        fs::create_dir(seed.join("other")).unwrap();
        fs::write(seed.join("other/host.lock"), b"nested-state").unwrap();
        fs::write(
            user.join("namespaces").join(namespace).join("state.json"),
            br#"{"format_version":1,"revision":2,"values":{"authentication/sessions/v1":{"sessions":[]}}}"#,
        )
        .unwrap();
        let sid = current_sid();
        let namespace_path = user.join("namespaces").join(namespace);
        let expected = format!("D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FA;;;{sid})");
        make_permissive(&namespace_path);
        assert!(validate_dacl(&namespace_path, &expected).is_err());
        assert!(claim_pending_seed(&data, &sid).unwrap());
        let held = amaterasu_package::RootLock::acquire(&user).unwrap();
        assert!(super::apply_pending_seed(&seed, &user).is_err());
        assert!(!user.join(".migration-seed-applied").exists());
        drop(held);
        super::apply_pending_seed(&seed, &user).unwrap();
        assert!(user.join(".migration-seed-applied").exists());
        let imported =
            read_seed_namespace(&user.join("namespaces").join(namespace).join("state.json"))
                .unwrap();
        assert_eq!(imported.revision, 3);
        assert!(imported.values.contains_key("authentication/v1"));
        assert!(imported.values.contains_key("authentication/sessions/v1"));
        assert!(fs::read(user.join("host.lock")).unwrap().is_empty());
        assert_eq!(
            fs::read(user.join("other/host.lock")).unwrap(),
            b"nested-state"
        );
        validate_dacl(&namespace_path, &expected).unwrap();
        validate_dacl(&namespace_path.join("state.json"), &expected).unwrap();
        super::apply_pending_seed(&seed, &user).unwrap();
        let imported =
            read_seed_namespace(&user.join("namespaces").join(namespace).join("state.json"))
                .unwrap();
        assert_eq!(imported.revision, 3);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn existing_authentication_credential_wins_over_imported_seed() {
        let root = std::env::temp_dir().join(format!(
            "amaterasu-state-root-credential-wins-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let seed = root.join("seed");
        let user = root.join("user");
        let namespace = "616d617465726173752e61757468656e7469636174696f6e";
        fs::create_dir_all(seed.join("namespaces").join(namespace)).unwrap();
        fs::create_dir_all(user.join("namespaces").join(namespace)).unwrap();
        fs::write(
            seed.join("namespaces").join(namespace).join("state.json"),
            br#"{"format_version":1,"revision":1,"values":{"authentication/v1":{"credential":"imported"}}}"#,
        )
        .unwrap();
        fs::write(
            user.join("namespaces").join(namespace).join("state.json"),
            br#"{"format_version":1,"revision":7,"values":{"authentication/v1":{"credential":"current"}}}"#,
        )
        .unwrap();
        super::apply_pending_seed(&seed, &user).unwrap();
        let current =
            read_seed_namespace(&user.join("namespaces").join(namespace).join("state.json"))
                .unwrap();
        assert_eq!(current.values["authentication/v1"]["credential"], "current");
        assert_eq!(current.revision, 7);
        assert!(user.join(".migration-seed-applied").exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn oversized_namespace_merge_preserves_existing_state_and_no_completion_marker() {
        let root = std::env::temp_dir().join(format!(
            "amaterasu-state-root-namespace-limit-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let source_root = root.join("seed");
        let destination_root = root.join("user");
        let namespace = "616d617465726173752e61757468656e7469636174696f6e";
        fs::create_dir_all(source_root.join("namespaces").join(namespace)).unwrap();
        fs::create_dir_all(destination_root.join("namespaces").join(namespace)).unwrap();
        let source = source_root
            .join("namespaces")
            .join(namespace)
            .join("state.json");
        let destination = destination_root
            .join("namespaces")
            .join(namespace)
            .join("state.json");
        let make_namespace = |prefix: &str| SeedNamespace {
            format_version: 1,
            revision: 1,
            values: (0..80)
                .map(|index| (format!("{prefix}-{index}"), Value::String("x".repeat(8000))))
                .collect::<BTreeMap<_, _>>(),
        };
        amaterasu_package::write_json_atomically(&source, &make_namespace("seed")).unwrap();
        amaterasu_package::write_json_atomically(&destination, &make_namespace("current")).unwrap();
        let original = fs::read(&destination).unwrap();
        assert!(super::apply_pending_seed(&source_root, &destination_root).is_err());
        assert_eq!(fs::read(&destination).unwrap(), original);
        assert!(!destination_root.join(".migration-seed-applied").exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn pending_seed_is_claimed_once_by_the_first_sid() {
        let root =
            std::env::temp_dir().join(format!("amaterasu-state-root-claim-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        assert!(claim_pending_seed(&root, "S-1-5-21-1").unwrap());
        assert!(claim_pending_seed(&root, "S-1-5-21-1").unwrap());
        assert!(!claim_pending_seed(&root, "S-1-5-21-2").unwrap());
        assert_eq!(
            fs::read(root.join("state-seed-claim")).unwrap(),
            b"S-1-5-21-1"
        );
        fs::remove_file(root.join("state-seed-claim")).unwrap();
        let long_sid = format!("S-1-5{}", "-4294967295".repeat(12));
        assert!(long_sid.len() > 128 && valid_sid(&long_sid));
        assert!(claim_pending_seed(&root, &long_sid).unwrap());
        assert!(claim_pending_seed(&root, &long_sid).unwrap());
        assert!(!claim_pending_seed(&root, "S-1-5-21-2").unwrap());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn incomplete_seed_claim_fails_closed_before_another_user_can_copy_it() {
        let root = std::env::temp_dir().join(format!(
            "amaterasu-state-root-incomplete-claim-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let claim = root.join("state-seed-claim");
        fs::write(&claim, b"").unwrap();
        assert!(claim_pending_seed(&root, "S-1-5-21-2").is_err());
        fs::write(&claim, b"S-1-5-21-1\n").unwrap();
        assert!(claim_pending_seed(&root, "S-1-5-21-1").is_err());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn validates_the_exact_protected_machine_and_user_acl_descriptors() {
        for sddl in [
            "D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)",
            "D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FA;;;S-1-5-21-1)",
        ] {
            let sddl_wide = wide(OsStr::new(sddl)).unwrap();
            let mut descriptor: *mut c_void = std::ptr::null_mut();
            let mut length = 0u32;
            // SAFETY: test input is NUL-terminated and Windows owns descriptor on success.
            assert_ne!(
                unsafe {
                    ConvertStringSecurityDescriptorToSecurityDescriptorW(
                        sddl_wide.as_ptr(),
                        1,
                        &mut descriptor,
                        &mut length,
                    )
                },
                0
            );
            let mut present = 0;
            let mut dacl = std::ptr::null_mut();
            let mut defaulted = 0;
            // SAFETY: descriptor is live for this direct descriptor query.
            assert_ne!(
                unsafe {
                    GetSecurityDescriptorDacl(descriptor, &mut present, &mut dacl, &mut defaulted)
                },
                0
            );
            assert_ne!(present, 0);
            validate_descriptor(descriptor, dacl, &expected_sids(sddl).unwrap()).unwrap();
            // SAFETY: the conversion API allocated this descriptor.
            unsafe { LocalFree(descriptor) };
        }
        assert_eq!(std::mem::size_of::<AccessAllowedAce>(), 12);
    }

    #[test]
    fn rejects_a_non_directory_state_root_and_normalizes_verbatim_paths() {
        let root = std::env::temp_dir().join(format!(
            "amaterasu-state-root-tamper-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let file = root.join("not-a-directory");
        fs::write(&file, b"tamper").unwrap();
        assert!(canonical_real_directory(&file, "test").is_err());
        let verbatim = std::path::PathBuf::from(format!(r"\\?\{}", root.display()));
        assert!(same_windows_path(&root, &verbatim));
        let _ = fs::remove_dir_all(root);
    }
}
