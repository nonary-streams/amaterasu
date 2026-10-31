//! Persistent independent Wayland registry observations for cooperative borrows.
use sha2::{Digest, Sha256};
use std::{
    ffi::{CString, c_char, c_int, c_void},
    fs,
};

#[repr(C)]
struct NativeObservation {
    global: u32,
    width: u32,
    height: u32,
    refresh: u32,
    generation: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Observation {
    pub global: u32,
    pub width: u32,
    pub height: u32,
    pub refresh: u32,
    pub generation: u64,
    pub compositor_id: [u8; 32],
}

unsafe extern "C" {
    fn am_display_observer_open() -> *mut c_void;
    fn am_display_observer_close(observer: *mut c_void);
    fn am_display_observer_peer_pid(observer: *mut c_void) -> c_int;
    fn am_display_observer_read(
        observer: *mut c_void,
        name: *const c_char,
        out: *mut NativeObservation,
    ) -> c_int;
}

pub(super) struct Observer {
    pointer: *mut c_void,
    compositor_id: [u8; 32],
}
impl Observer {
    pub fn open() -> Option<Self> {
        // SAFETY: the native observer owns its connection and returns an opaque pointer.
        let pointer = unsafe { am_display_observer_open() };
        if pointer.is_null() {
            return None;
        }
        let mut observer = Self {
            pointer,
            compositor_id: [0; 32],
        };
        observer.compositor_id = observer.peer_identity()?;
        Some(observer)
    }
    fn peer_pid(&self) -> Option<u32> {
        // SAFETY: the connection remains owned and alive for this synchronous call.
        let pid = unsafe { am_display_observer_peer_pid(self.pointer) };
        u32::try_from(pid).ok().filter(|pid| *pid != 0)
    }
    fn peer_identity(&self) -> Option<[u8; 32]> {
        let pid = self.peer_pid()?;
        let boot = fs::read_to_string("/proc/sys/kernel/random/boot_id").ok()?;
        let boot = boot.trim();
        if !crate::model::valid_identity(boot) {
            return None;
        }
        let start = process_start_time(&fs::read_to_string(format!("/proc/{pid}/stat")).ok()?)?;
        if self.peer_pid()? != pid
            || process_start_time(&fs::read_to_string(format!("/proc/{pid}/stat")).ok()?)? != start
            || fs::read_to_string("/proc/sys/kernel/random/boot_id")
                .ok()?
                .trim()
                != boot
        {
            return None;
        }
        Some(identity_digest(boot, pid, start))
    }
    pub fn read(&mut self, name: &str) -> Option<Observation> {
        if self.peer_identity()? != self.compositor_id {
            return None;
        }
        let name = CString::new(name).ok()?;
        let mut result = NativeObservation {
            global: 0,
            width: 0,
            height: 0,
            refresh: 0,
            generation: 0,
        };
        // SAFETY: the observer is uniquely borrowed; name and result live through this call.
        if unsafe { am_display_observer_read(self.pointer, name.as_ptr(), &mut result) } != 0
            || self.peer_identity()? != self.compositor_id
        {
            return None;
        }
        Some(Observation {
            global: result.global,
            width: result.width,
            height: result.height,
            refresh: result.refresh,
            generation: result.generation,
            compositor_id: self.compositor_id,
        })
    }
}
fn process_start_time(stat: &str) -> Option<u64> {
    // The parenthesized command may itself contain spaces or closing parentheses.
    let rest = stat.rsplit_once(')')?.1;
    rest.split_whitespace().nth(19)?.parse().ok()
}
fn identity_digest(boot: &str, pid: u32, start: u64) -> [u8; 32] {
    Sha256::digest(format!("amaterasu-wayland-compositor-v1\n{boot}\n{pid}\n{start}\n").as_bytes())
        .into()
}
impl Drop for Observer {
    fn drop(&mut self) {
        // SAFETY: the pointer belongs exclusively to this wrapper and is closed once.
        unsafe { am_display_observer_close(self.pointer) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn process_stat_parses_start_time_after_complex_command() {
        let stat =
            "17 (name with ) parentheses) S 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 12345 20";
        assert_eq!(process_start_time(stat), Some(12345));
        assert_eq!(process_start_time("bad"), None);
    }
    #[test]
    fn digest_distinguishes_peer_lifetimes() {
        let boot = "00000000-0000-0000-0000-000000000001";
        let digest = identity_digest(boot, 42, 123);
        assert_ne!(digest, identity_digest(boot, 42, 124));
        assert_ne!(digest, identity_digest(boot, 43, 123));
        let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
        assert_eq!(
            hex,
            "3d3d4ac4d977d80fb5c38b6cf9f90787d9051298687dd88b46b16addc7476db5"
        );
    }
}
