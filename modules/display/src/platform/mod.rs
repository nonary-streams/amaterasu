use crate::model::{AvailableMode, Configuration, DisplayResult, RouteBinding, Snapshot};
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
    fn bind_inactive(&mut self, display: &str, before: &Snapshot) -> DisplayResult<RouteBinding>;
    fn prepare_preserving(
        &mut self,
        configuration: &Configuration,
        selected: &str,
        route: Option<&RouteBinding>,
    ) -> DisplayResult<Snapshot>;
    fn apply(
        &mut self,
        configuration: &Configuration,
        before: &Snapshot,
        guard: &mut dyn FnMut() -> DisplayResult<()>,
    ) -> DisplayResult<()>;
    fn apply_preserving(
        &mut self,
        configuration: &Configuration,
        before: &Snapshot,
        selected: &str,
        route: Option<&RouteBinding>,
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
    fn bind_inactive(&mut self, _: &str, _: &Snapshot) -> DisplayResult<RouteBinding> {
        unsupported()
    }
    fn prepare_preserving(
        &mut self,
        _: &Configuration,
        _: &str,
        _: Option<&RouteBinding>,
    ) -> DisplayResult<Snapshot> {
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
    fn apply_preserving(
        &mut self,
        _: &Configuration,
        _: &Snapshot,
        _: &str,
        _: Option<&RouteBinding>,
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
