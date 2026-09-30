use crate::{
    buffers::Mailbox,
    model::{Availability, CaptureResult, SourceState, SourceType},
};
use std::sync::Arc;

pub trait SourceSession: Send {
    fn state(&self) -> SourceState;
    fn mailbox(&self) -> Arc<Mailbox>;
}

pub trait Backend: Send {
    fn availability(&self) -> Availability;
    fn select(
        &self,
        source: SourceType,
        timeout_seconds: u32,
    ) -> CaptureResult<Box<dyn SourceSession>>;
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
    pub fn new() -> Self {
        Self
    }
}

#[cfg(not(amaterasu_pipewire))]
impl Backend for NativeBackend {
    fn availability(&self) -> Availability {
        Availability::unsupported(unsupported_reason())
    }
    fn select(&self, _: SourceType, _: u32) -> CaptureResult<Box<dyn SourceSession>> {
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
