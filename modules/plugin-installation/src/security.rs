//! Machine policy and staging access checks at the OS boundary.
use amaterasu_package::PackageResult;
use std::path::Path;
pub fn real_path(path: &Path) -> PackageResult<()> {
    if !path.is_absolute() {
        return Err("driver_path_not_absolute".into());
    }
    for ancestor in path.ancestors() {
        let metadata = std::fs::symlink_metadata(ancestor).map_err(|e| e.to_string())?;
        if metadata.file_type().is_symlink() {
            return Err("driver_reparse_path".into());
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if metadata.file_attributes() & 0x400 != 0 {
                return Err("driver_reparse_path".into());
            }
        }
    }
    Ok(())
}
pub fn machine_private(path: &Path) -> PackageResult<()> {
    real_path(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
        if metadata.uid() != 0 || metadata.mode() & 0o077 != 0 {
            return Err("driver_policy_not_protected".into());
        }
        Ok(())
    }
    #[cfg(windows)]
    {
        windows::verify(path, true)
    }
}
#[cfg(windows)]
pub fn windows_parent(path: &Path) -> PackageResult<()> {
    windows::verify(path, false)
}
#[cfg(windows)]
pub fn runtime_read(path: &Path, identity: &str) -> PackageResult<()> {
    windows::runtime_read(path, identity)
}
#[cfg(windows)]
mod windows {
    use super::*;
    use std::{ffi::c_void, os::windows::ffi::OsStrExt, ptr::null_mut};
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::{Authorization::*, *},
        System::SystemServices::{ACCESS_ALLOWED_ACE_TYPE, ACCESS_DENIED_ACE_TYPE},
    };
    struct Allocation(*mut c_void);
    impl Drop for Allocation {
        fn drop(&mut self) {
            unsafe {
                LocalFree(self.0);
            }
        }
    }
    fn sid_text(sid: PSID) -> PackageResult<String> {
        let mut text = null_mut();
        // SAFETY: SID belongs to the live descriptor; Windows allocates the string.
        if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 {
            return Err("driver_invalid_owner".into());
        }
        let _allocation = Allocation(text.cast());
        let mut length = 0;
        while length < 184 && unsafe { *text.add(length) } != 0 {
            length += 1;
        }
        if length == 184 {
            return Err("driver_invalid_owner".into());
        }
        Ok(String::from_utf16_lossy(unsafe {
            std::slice::from_raw_parts(text, length)
        }))
    }
    pub fn verify(path: &Path, private: bool) -> PackageResult<()> {
        let target: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let (mut owner, mut dacl, mut descriptor) = (null_mut(), null_mut(), null_mut());
        // SAFETY: outputs point into one LocalFree-owned descriptor retained below.
        let status = unsafe {
            GetNamedSecurityInfoW(
                target.as_ptr(),
                SE_FILE_OBJECT,
                OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
                &mut owner,
                null_mut(),
                &mut dacl,
                null_mut(),
                &mut descriptor,
            )
        };
        let _descriptor = Allocation(descriptor);
        if status != 0 || descriptor.is_null() || owner.is_null() || dacl.is_null() {
            return Err("driver_policy_acl_unreadable".into());
        }
        if !matches!(sid_text(owner)?.as_str(), "S-1-5-18" | "S-1-5-32-544") {
            return Err("driver_policy_owner_untrusted".into());
        }
        let (mut control, mut revision) = (0, 0);
        if unsafe { GetSecurityDescriptorControl(descriptor, &mut control, &mut revision) } == 0
            || private && control & SE_DACL_PROTECTED == 0
        {
            return Err("driver_policy_acl_inherited".into());
        }
        let mut info: ACL_SIZE_INFORMATION = unsafe { std::mem::zeroed() };
        if unsafe {
            GetAclInformation(
                dacl,
                (&mut info as *mut ACL_SIZE_INFORMATION).cast(),
                std::mem::size_of_val(&info) as u32,
                AclSizeInformation,
            )
        } == 0
        {
            return Err("driver_policy_acl_unreadable".into());
        }
        for index in 0..info.AceCount {
            let mut ace = null_mut();
            if unsafe { GetAce(dacl, index, &mut ace) } == 0 || ace.is_null() {
                return Err("driver_policy_acl_unreadable".into());
            }
            // SAFETY: GetAce returned a kernel-validated ACE. Read the header
            // first, then the fixed allow layout only after type/size checks.
            let header = unsafe { &*ace.cast::<ACE_HEADER>() };
            let ace_type = u32::from(header.AceType);
            if ace_type == ACCESS_DENIED_ACE_TYPE {
                continue;
            }
            if ace_type != ACCESS_ALLOWED_ACE_TYPE
                || usize::from(header.AceSize) < std::mem::size_of::<ACCESS_ALLOWED_ACE>()
            {
                return Err("driver_policy_acl_unsupported".into());
            }
            let allow = unsafe { &*ace.cast::<ACCESS_ALLOWED_ACE>() };
            let sid = sid_text((&allow.SidStart as *const u32).cast_mut().cast())?;
            let trusted = matches!(sid.as_str(), "S-1-5-18" | "S-1-5-32-544");
            const MUTATE: u32 = 0x500d_0156;
            if !trusted && (private || allow.Mask & MUTATE != 0) {
                return Err("driver_policy_acl_too_broad".into());
            }
        }
        Ok(())
    }
    pub fn runtime_read(path: &Path, identity: &str) -> PackageResult<()> {
        if !identity.starts_with("S-1-5-21-")
            || !identity
                .bytes()
                .all(|c| c.is_ascii_digit() || c == b'S' || c == b'-')
        {
            return Err("driver_invalid_runtime_sid".into());
        }
        let source =
            format!("O:BAG:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FRFX;;;{identity})");
        let source: Vec<u16> = source.encode_utf16().chain(Some(0)).collect();
        let target: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let mut descriptor = null_mut();
        // SAFETY: the descriptor uses only a validated SID and fixed access bits.
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                source.as_ptr(),
                1,
                &mut descriptor,
                null_mut(),
            )
        } == 0
        {
            return Err("driver_runtime_acl_failed".into());
        }
        let _descriptor = Allocation(descriptor);
        if unsafe {
            SetFileSecurityW(
                target.as_ptr(),
                OWNER_SECURITY_INFORMATION
                    | DACL_SECURITY_INFORMATION
                    | PROTECTED_DACL_SECURITY_INFORMATION,
                descriptor,
            )
        } == 0
        {
            return Err("driver_runtime_acl_failed".into());
        }
        Ok(())
    }
}
