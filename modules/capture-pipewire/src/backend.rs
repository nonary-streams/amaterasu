use crate::{
    buffers::Mailbox,
    model::{Availability, CaptureResult, SourceDiagnostics, SourceState, SourceType},
};
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct ObservedCapture {
    pub version: u8,
    pub protocol: &'static str,
    pub compositor_id: String,
    pub registry_global: u32,
    pub output_name: String,
    pub width: u32,
    pub height: u32,
    pub refresh_millihz: u32,
}

pub trait SourceSession: Send {
    fn observed_capture(&self) -> Option<ObservedCapture> {
        None
    }
    fn geometry(&self) -> Option<amaterasu_media::CaptureGeometry> {
        None
    }
    fn state(&self) -> SourceState;
    fn diagnostics(&self) -> SourceDiagnostics {
        SourceDiagnostics::default()
    }
    fn mailbox(&self) -> Arc<Mailbox>;
}

pub trait Backend: Send {
    fn availability(&self) -> Availability;
    fn select(
        &self,
        owner: &str,
        source: SourceType,
        timeout_seconds: u32,
    ) -> CaptureResult<Box<dyn SourceSession>>;
    fn can_restore(&self, _owner: &str, _source: SourceType) -> bool {
        false
    }
    fn select_output(&self, _output: &str, _timeout: u32) -> CaptureResult<Box<dyn SourceSession>> {
        Err("named_capture_unavailable")
    }
    /// Inspect only compositor capabilities/output inventory. Never create a
    /// screencast stream, a portal selection, a worker, or a display.
    fn preflight_output(
        &self,
        _output: Option<&str>,
        _managed: bool,
        _mode: &amaterasu_module_sdk::capture::SourceMode,
    ) -> CaptureResult<()> {
        Err("named_capture_unavailable")
    }
    fn forget(&self, _owner: &str, _source: SourceType) {}
}

#[cfg(amaterasu_pipewire)]
#[path = "platform/mod.rs"]
mod platform;
#[cfg(amaterasu_pipewire)]
pub use platform::NativeBackend;

#[cfg(not(amaterasu_pipewire))]
pub struct NativeBackend;

#[cfg(not(amaterasu_pipewire))]
impl NativeBackend {
    #[cfg(test)]
    pub fn unavailable_for_test() -> Self {
        Self
    }

    pub fn new() -> Self {
        Self
    }
    pub fn configure_persistence(&mut self) -> CaptureResult<()> {
        Err(unsupported_reason())
    }
}

#[cfg(not(amaterasu_pipewire))]
impl Backend for NativeBackend {
    fn availability(&self) -> Availability {
        Availability::unsupported(unsupported_reason())
    }
    fn select(&self, _: &str, _: SourceType, _: u32) -> CaptureResult<Box<dyn SourceSession>> {
        Err(unsupported_reason())
    }
}

#[cfg(not(amaterasu_pipewire))]
fn unsupported_reason() -> &'static str {
    if cfg!(target_os = "linux") {
        "native_dependencies_unavailable"
    } else {
        "unsupported_platform"
    }
}
