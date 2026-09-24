use crate::model::{Event, InputResult};

pub trait Backend {
    fn name(&self) -> &'static str;
    fn supported(&self) -> bool;
    fn absolute_pointer(&self) -> bool;
    fn key_encoding(&self) -> &'static str;
    fn validate(&self, event: &Event) -> InputResult<()>;
    fn start(&mut self) -> InputResult<()>;
    fn inject(&mut self, event: &Event) -> InputResult<()>;
    /// Windows uses this for non-normalized Moonlight virtual keys. Other
    /// native backends deliberately retain their platform mapping behavior.
    fn set_always_send_scancodes(&mut self, _enabled: bool) {}
    /// Maps the host's right-Alt virtual key to the left Windows key. Backends
    /// preserve the selected mapping for matching key-up events.
    fn set_key_rightalt_to_key_win(&mut self, _enabled: bool) {}
    fn stop(&mut self);
}
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::WindowsBackend as NativeBackend;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::LinuxBackend as NativeBackend;
#[cfg(not(any(windows, target_os = "linux")))]
pub struct NativeBackend;
#[cfg(not(any(windows, target_os = "linux")))]
impl NativeBackend {
    pub fn new(_: String) -> Self {
        Self
    }
}
#[cfg(not(any(windows, target_os = "linux")))]
impl Backend for NativeBackend {
    fn name(&self) -> &'static str {
        std::env::consts::OS
    }
    fn supported(&self) -> bool {
        false
    }
    fn absolute_pointer(&self) -> bool {
        false
    }
    fn key_encoding(&self) -> &'static str {
        "unsupported"
    }
    fn validate(&self, _: &Event) -> InputResult<()> {
        unsupported()
    }
    fn start(&mut self) -> InputResult<()> {
        unsupported()
    }
    fn inject(&mut self, _: &Event) -> InputResult<()> {
        unsupported()
    }
    fn stop(&mut self) {}
}
#[cfg(not(any(windows, target_os = "linux")))]
fn unsupported() -> InputResult<()> {
    Err(crate::model::InputError::new(
        "unsupported_platform",
        "native input injection is implemented only for Windows and Linux",
    ))
}
