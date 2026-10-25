//! Native modules are in-process trusted code. The ordinary loader never runs
//! under an elevated/root token; privileged handlers need a separate broker.
use amaterasu_package::PackageResult;

#[cfg(windows)]
pub fn require_user_process() -> PackageResult<()> {
    use std::ffi::c_void;
    const TOKEN_QUERY: u32 = 0x0008;
    const TOKEN_ELEVATION_CLASS: u32 = 20;
    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn OpenProcessToken(process: *mut c_void, access: u32, token: *mut *mut c_void) -> i32;
        fn GetTokenInformation(
            token: *mut c_void,
            class: u32,
            information: *mut c_void,
            length: u32,
            returned: *mut u32,
        ) -> i32;
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentProcess() -> *mut c_void;
        fn CloseHandle(handle: *mut c_void) -> i32;
    }
    let mut token = std::ptr::null_mut();
    // SAFETY: output points to writable storage, and the pseudo process handle
    // remains valid for this call. The returned real token handle is closed.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(format!(
            "cannot inspect worker token: {}",
            std::io::Error::last_os_error()
        ));
    }
    let mut elevated = 0u32;
    let mut returned = 0u32;
    // SAFETY: `elevated` is a writable 4-byte TOKEN_ELEVATION result; the token
    // was opened above and is closed before this function returns.
    let inspected = unsafe {
        GetTokenInformation(
            token,
            TOKEN_ELEVATION_CLASS,
            (&mut elevated as *mut u32).cast(),
            std::mem::size_of::<u32>() as u32,
            &mut returned,
        )
    };
    let error = std::io::Error::last_os_error();
    // SAFETY: this handle was obtained from OpenProcessToken exactly once.
    unsafe { CloseHandle(token) };
    if inspected == 0 || returned != std::mem::size_of::<u32>() as u32 {
        return Err(format!("cannot inspect worker elevation: {error}"));
    }
    if elevated != 0 {
        return Err("ordinary native modules cannot load in an elevated worker".into());
    }
    Ok(())
}

#[cfg(unix)]
pub fn require_user_process() -> PackageResult<()> {
    unsafe extern "C" {
        fn geteuid() -> u32;
    }
    // SAFETY: geteuid reads the calling process's effective identity.
    if unsafe { geteuid() } == 0 {
        return Err("ordinary native modules cannot load as root".into());
    }
    Ok(())
}

#[cfg(not(any(windows, unix)))]
pub fn require_user_process() -> PackageResult<()> {
    Err("ordinary native loading has no privilege check on this platform".into())
}
