use crate::model::{AvailableMode, Configuration, DisplayResult, Snapshot};
use serde_json::Value;

pub trait Platform {
    fn name(&self) -> &'static str;
    fn supported(&self) -> bool;
    fn enumerate(
        &mut self,
        display: Option<&str>,
        offset: usize,
        limit: usize,
    ) -> DisplayResult<Value>;
    fn available_modes(&mut self, display: &str) -> DisplayResult<Vec<AvailableMode>>;
    fn snapshot(&mut self) -> DisplayResult<Snapshot>;
    fn prepare(&mut self, configuration: &Configuration) -> DisplayResult<Snapshot>;
    fn apply(
        &mut self,
        configuration: &Configuration,
        before: &Snapshot,
        guard: &mut dyn FnMut() -> DisplayResult<()>,
    ) -> DisplayResult<()>;
    fn restore(
        &mut self,
        snapshot: &Snapshot,
        guard: &mut dyn FnMut() -> DisplayResult<()>,
    ) -> DisplayResult<()>;
}
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::WindowsPlatform as NativePlatform;
#[cfg(not(windows))]
#[derive(Default)]
pub struct NativePlatform;
#[cfg(not(windows))]
impl Platform for NativePlatform {
    fn name(&self) -> &'static str {
        std::env::consts::OS
    }
    fn supported(&self) -> bool {
        false
    }
    fn enumerate(&mut self, _: Option<&str>, _: usize, _: usize) -> DisplayResult<Value> {
        unsupported()
    }
    fn available_modes(&mut self, _: &str) -> DisplayResult<Vec<AvailableMode>> {
        unsupported()
    }
    fn snapshot(&mut self) -> DisplayResult<Snapshot> {
        unsupported()
    }
    fn prepare(&mut self, _: &Configuration) -> DisplayResult<Snapshot> {
        unsupported()
    }
    fn apply(
        &mut self,
        _: &Configuration,
        _: &Snapshot,
        _: &mut dyn FnMut() -> DisplayResult<()>,
    ) -> DisplayResult<()> {
        unsupported()
    }
    fn restore(
        &mut self,
        _: &Snapshot,
        _: &mut dyn FnMut() -> DisplayResult<()>,
    ) -> DisplayResult<()> {
        unsupported()
    }
}
#[cfg(not(windows))]
fn unsupported<T>() -> DisplayResult<T> {
    Err(crate::model::DisplayError::new(
        "unsupported_platform",
        "native physical display management is implemented only for Windows; no Linux/macOS fallback is active",
    ))
}
