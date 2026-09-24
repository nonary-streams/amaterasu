use crate::model::{Event, InputResult};
use std::time::Instant;

pub trait Backend {
    fn name(&self) -> &'static str;
    fn supported(&self) -> bool;
    fn absolute_pointer(&self) -> bool;
    fn controller_input_supported(&self) -> bool {
        false
    }
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
    /// Advances native timers that need service even when the client sends no packet.
    fn advance(&mut self, _now: Instant) -> InputResult<()> {
        Ok(())
    }
    fn set_back_button_timeout(&mut self, _timeout_ms: i32) {}
    fn stop(&mut self) -> InputResult<()>;
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
    fn stop(&mut self) -> InputResult<()> {
        Ok(())
    }
}
#[cfg(not(any(windows, target_os = "linux")))]
fn unsupported() -> InputResult<()> {
    Err(crate::model::InputError::new(
        "unsupported_platform",
        "native input injection is implemented only for Windows and Linux",
    ))
}
