use std::{ffi::c_void, io};
#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetCurrentThread() -> *mut c_void;
    fn GetThreadPriority(thread: *mut c_void) -> i32;
    fn SetThreadPriority(thread: *mut c_void, priority: i32) -> i32;
}
pub(super) fn current() -> io::Result<i32> {
    // SAFETY: the calling thread's pseudo handle is valid and never closed.
    let priority = unsafe { GetThreadPriority(GetCurrentThread()) };
    if priority == i32::MAX {
        Err(io::Error::last_os_error())
    } else {
        Ok(priority)
    }
}
pub(super) fn set(priority: i32) -> io::Result<()> {
    // SAFETY: the pseudo handle names the current thread; no other thread changes.
    if unsafe { SetThreadPriority(GetCurrentThread(), priority) } == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}
