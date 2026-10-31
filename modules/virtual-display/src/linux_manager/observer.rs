//! Persistent independent Wayland registry observations for cooperative borrows.
use std::ffi::{CString, c_char, c_int, c_void};

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Observation {
    pub global: u32,
    pub width: u32,
    pub height: u32,
    pub refresh: u32,
    pub generation: u64,
}

unsafe extern "C" {
    fn am_display_observer_open() -> *mut c_void;
    fn am_display_observer_close(observer: *mut c_void);
    fn am_display_observer_read(
        observer: *mut c_void,
        name: *const c_char,
        out: *mut Observation,
    ) -> c_int;
}

pub(super) struct Observer(*mut c_void);
impl Observer {
    pub fn open() -> Option<Self> {
        // SAFETY: the native observer owns its connection and returns an opaque pointer.
        let pointer = unsafe { am_display_observer_open() };
        (!pointer.is_null()).then_some(Self(pointer))
    }
    pub fn read(&mut self, name: &str) -> Option<Observation> {
        let name = CString::new(name).ok()?;
        let mut result = Observation {
            global: 0,
            width: 0,
            height: 0,
            refresh: 0,
            generation: 0,
        };
        // SAFETY: the observer is uniquely borrowed; name and result live through this call.
        (unsafe { am_display_observer_read(self.0, name.as_ptr(), &mut result) } == 0)
            .then_some(result)
    }
}
impl Drop for Observer {
    fn drop(&mut self) {
        // SAFETY: the pointer belongs exclusively to this wrapper and is closed once.
        unsafe { am_display_observer_close(self.0) }
    }
}
