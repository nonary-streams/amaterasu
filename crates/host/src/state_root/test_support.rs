//! Windows filesystem fixtures that exercise the interactive SID without the
//! hosted runner's Administrators ACEs or ambient short temp-path aliases.
use super::{GetCurrentProcess, OpenProcessToken, SidAndAttributes, sid_from_string, sid_string};
use std::{
    ffi::c_void,
    io,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    path::PathBuf,
    ptr,
};

const TOKEN_DUPLICATE: u32 = 0x0002;
const TOKEN_QUERY: u32 = 0x0008;
const TOKEN_ADJUST_DEFAULT: u32 = 0x0080;
const TOKEN_OWNER: u32 = 4;
const DISABLE_MAX_PRIVILEGE: u32 = 1;

#[link(name = "advapi32")]
unsafe extern "system" {
    fn CreateRestrictedToken(
        token: *mut c_void,
        flags: u32,
        disabled_count: u32,
        disabled: *const SidAndAttributes,
        privilege_count: u32,
        privileges: *const c_void,
        restricted_count: u32,
        restricted: *const SidAndAttributes,
        result: *mut *mut c_void,
    ) -> i32;
    fn SetTokenInformation(token: *mut c_void, class: u32, data: *const c_void, size: u32) -> i32;
    fn ImpersonateLoggedOnUser(token: *mut c_void) -> i32;
    fn RevertToSelf() -> i32;
}

pub(crate) fn temp_dir() -> PathBuf {
    std::env::temp_dir().canonicalize().unwrap()
}

pub(crate) fn with_interactive_permissions<T>(test: impl FnOnce() -> T) -> T {
    let mut process = ptr::null_mut();
    // SAFETY: the process pseudo-handle and the output slot are valid.
    require_success(unsafe {
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_DUPLICATE | TOKEN_QUERY | TOKEN_ADJUST_DEFAULT,
            &mut process,
        )
    });
    // SAFETY: OpenProcessToken returned a new owned token handle.
    let process = unsafe { OwnedHandle::from_raw_handle(process) };
    let user = sid_from_string(&sid_string(process.as_raw_handle()).unwrap()).unwrap();
    let admins = sid_from_string("S-1-5-32-544").unwrap();
    let disabled = SidAndAttributes {
        sid: admins.0,
        attributes: 0,
    };
    let mut interactive = ptr::null_mut();
    // SAFETY: all inputs are live and the output slot receives a fresh handle.
    require_success(unsafe {
        CreateRestrictedToken(
            process.as_raw_handle(),
            DISABLE_MAX_PRIVILEGE,
            1,
            &disabled,
            0,
            ptr::null(),
            0,
            ptr::null(),
            &mut interactive,
        )
    });
    // SAFETY: CreateRestrictedToken returned a new owned token handle.
    let interactive = unsafe { OwnedHandle::from_raw_handle(interactive) };

    // TOKEN_OWNER is one SID pointer. Make objects created in the fixture
    // belong to the user even if the process normally defaults to Administrators.
    // SAFETY: the SID allocation and the pointer-sized input are live.
    require_success(unsafe {
        SetTokenInformation(
            interactive.as_raw_handle(),
            TOKEN_OWNER,
            (&user.0 as *const *mut c_void).cast(),
            std::mem::size_of::<*mut c_void>() as u32,
        )
    });
    // SAFETY: the live restricted token applies to this test thread only.
    require_success(unsafe { ImpersonateLoggedOnUser(interactive.as_raw_handle()) });
    let _impersonation = RevertImpersonation;
    test()
}

fn require_success(result: i32) {
    assert_ne!(
        result,
        0,
        "Windows test identity setup failed: {}",
        io::Error::last_os_error()
    );
}

struct RevertImpersonation;

impl Drop for RevertImpersonation {
    fn drop(&mut self) {
        // SAFETY: this guard is created only after impersonation succeeds and
        // never leaves the test thread.
        if unsafe { RevertToSelf() } == 0 {
            std::process::abort();
        }
    }
}
