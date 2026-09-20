//! SCM-owned per-user state roots for interactive workers.
//!
//! The service runs as LocalSystem and derives the directory name from the
//! active WTS token. A signed package is verified before this code is reached;
//! the package never chooses this path. The machine migration seed remains
//! SYSTEM/Admin-only and is copied once into a SID-specific directory before
//! the user-token process starts.

use amaterasu_package::{PackageResult, random_identifier};
use std::ffi::{OsStr, c_void};
use std::fs;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};

const TOKEN_USER: u32 = 1;
const DACL_SECURITY_INFORMATION: u32 = 0x0000_0004;
const SE_FILE_OBJECT: u32 = 1;
const SE_DACL_PROTECTED: u16 = 0x1000;
const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;
const OBJECT_INHERIT_ACE: u8 = 0x01;
const CONTAINER_INHERIT_ACE: u8 = 0x02;
const FILE_ALL_ACCESS: u32 = 0x001F_01FF;
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
const INVALID_FILE_ATTRIBUTES: u32 = u32::MAX;

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
}

/// Prepares the root visible to exactly one active interactive user.
pub(crate) fn prepare(root: &Path, token: *mut c_void) -> PackageResult<PathBuf> {
    let root = canonical_real_directory(root, "runtime root")?;
    ensure_machine_private(&root)?;
    let sid = sid_string(token)?;
    if !valid_sid(&sid) {
        return Err("active user token has an invalid SID".into());
    }

    let container = root.join("user-state");
    ensure_directory(&container, "D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)")?;
    let destination = container.join(&sid);
    if destination
        .try_exists()
        .map_err(|error| error.to_string())?
    {
        let destination = canonical_real_directory(&destination, "user state root")?;
        set_dacl(
            &destination,
            &format!("D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FA;;;{sid})"),
        )?;
        return Ok(destination);
    }

    // The migration helpers seed this machine-private directory. It is copied
    // only during first materialization; later service starts preserve the
    // user's durable state rather than replaying an import.
    let data = root.join("data");
    ensure_machine_private(&data)?;
    let seed = data.join("state");
    if !seed.try_exists().map_err(|error| error.to_string())? {
        return materialize_empty(&container, &destination, &sid);
    }
    let seed = canonical_real_directory(&seed, "pending migration seed")?;
    ensure_machine_private(&seed)?;
    if !claim_pending_seed(&data, &sid)? {
        return materialize_empty(&container, &destination, &sid);
    }
    let pending = container.join(format!(".pending-{}", random_identifier()?));
    create_real_directory(&pending)?;
    let result = copy_real_tree(&seed, &pending);
    if let Err(error) = result {
        let _ = fs::remove_dir_all(&pending);
        let _ = fs::remove_file(data.join("state-seed-claim"));
        return Err(error);
    }
    let result = (|| {
        set_dacl(
            &pending,
            &format!("D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FA;;;{sid})"),
        )?;
        fs::rename(&pending, &destination).map_err(|error| error.to_string())?;
        canonical_real_directory(&destination, "new user state root")
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&pending);
        let _ = fs::remove_file(data.join("state-seed-claim"));
    }
    result
}

/// The seed can include the migrated administrator verifier and pairing
/// identity. It is therefore consumed by exactly one SID, never duplicated to
/// every account that subsequently signs in. The caller holds the runtime root
/// lock while this `create_new` marker is written; create-new also makes the
/// boundary safe if this helper is reused outside that service loop.
fn claim_pending_seed(data: &Path, sid: &str) -> PackageResult<bool> {
    let claim = data.join("state-seed-claim");
    if claim.try_exists().map_err(|error| error.to_string())? {
        let metadata = fs::symlink_metadata(&claim).map_err(|error| error.to_string())?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err("migration seed claim is invalid".into());
        }
        reject_reparse(&claim)?;
        return Ok(false);
    }
    use std::io::Write;
    let mut claim_file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&claim)
        .map_err(|error| error.to_string())?;
    claim_file
        .write_all(sid.as_bytes())
        .and_then(|()| claim_file.sync_all())
        .map_err(|error| error.to_string())?;
    Ok(true)
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
    validate_dacl(path, sddl)
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
        && value.len() <= 184
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b'-' || byte == b'S')
}

#[cfg(test)]
mod tests {
    use super::{
        AccessAllowedAce, ConvertStringSecurityDescriptorToSecurityDescriptorW,
        GetSecurityDescriptorDacl, LocalFree, canonical_real_directory, claim_pending_seed,
        copy_real_tree, expected_sids, same_windows_path, valid_sid, validate_descriptor, wide,
    };
    use std::ffi::{OsStr, c_void};
    use std::fs;

    #[test]
    fn accepts_only_string_sids_safe_for_a_directory_component() {
        assert!(valid_sid("S-1-5-21-123-456-789-1001"));
        assert!(!valid_sid("S-1-5-21-.."));
        assert!(!valid_sid("S-1-5-21-1001\\escape"));
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
    fn pending_seed_is_claimed_once_by_the_first_sid() {
        let root =
            std::env::temp_dir().join(format!("amaterasu-state-root-claim-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        assert!(claim_pending_seed(&root, "S-1-5-21-1").unwrap());
        assert!(!claim_pending_seed(&root, "S-1-5-21-2").unwrap());
        assert_eq!(
            fs::read(root.join("state-seed-claim")).unwrap(),
            b"S-1-5-21-1"
        );
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
