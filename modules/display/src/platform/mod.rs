use crate::model::{
    Address, AvailableMode, Configuration, DisplayError, DisplayResult, KScreenSnapshot,
    RouteBinding, Snapshot,
};
use serde_json::Value;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TopologyShape {
    PreserveActive,
    PrimaryActive,
    TranslateActive,
    OnlySelected,
    CloneActive,
}

pub trait Platform {
    fn name(&self) -> &'static str;
    fn supported(&self) -> bool;
    /// A narrower capability for caller-owned target composition. Providers may
    /// offer this without enabling arbitrary desktop mutation.
    fn supports_scoped_composition(&self) -> bool {
        self.supported()
    }
    fn enumerate(
        &mut self,
        display: Option<&str>,
        offset: usize,
        limit: usize,
    ) -> DisplayResult<Value>;
    fn available_modes(&mut self, display: &str) -> DisplayResult<Vec<AvailableMode>>;
    fn snapshot(&mut self) -> DisplayResult<Snapshot>;
    /// Read-only session configuration view exposed by the Snapshot operation.
    /// Providers whose native recovery state differs from the Windows CCD model
    /// can override this without pretending their state is a CCD snapshot.
    fn read_configuration_snapshot(&mut self) -> DisplayResult<Value> {
        let snapshot = self.snapshot()?;
        Ok(serde_json::json!({
            "configuration": snapshot.configuration()?,
            "topology_revision": snapshot.revision()?,
        }))
    }
    /// Capture a provider-specific durable recovery baseline. Providers that
    /// use the legacy CCD model return None and keep its wire format unchanged.
    fn capture_kscreen_snapshot(&mut self) -> DisplayResult<Option<KScreenSnapshot>> {
        Ok(None)
    }
    /// Validate a direct KScreen configuration against the durable baseline
    /// and the provider's currently available modes without changing outputs.
    fn prepare_kscreen_snapshot(
        &mut self,
        _: &Configuration,
        _: &KScreenSnapshot,
    ) -> DisplayResult<()> {
        Err(DisplayError::new(
            "unsupported_platform",
            "KScreen configuration preparation is not implemented by this provider",
        ))
    }
    /// Restore a KScreen journal baseline, including conservative hotplug
    /// reconciliation and readback verification before returning success. This
    /// defaults to fail-closed so a provider cannot treat it as a CCD snapshot.
    fn restore_kscreen_snapshot(
        &mut self,
        _: &KScreenSnapshot,
        _: &mut dyn FnMut() -> DisplayResult<()>,
    ) -> DisplayResult<()> {
        Err(DisplayError::new(
            "unsupported_platform",
            "KScreen journal restoration is not implemented by this provider",
        ))
    }
    /// Apply a KScreen configuration and return success only after exact
    /// requested-state readback verification.
    fn apply_kscreen_snapshot(
        &mut self,
        _: &Configuration,
        _: &KScreenSnapshot,
        _: &mut dyn FnMut() -> DisplayResult<()>,
    ) -> DisplayResult<()> {
        Err(DisplayError::new(
            "unsupported_platform",
            "KScreen apply is not implemented by this provider",
        ))
    }
    fn prepare(&mut self, configuration: &Configuration) -> DisplayResult<Snapshot>;
    fn bind_inactive(&mut self, display: &str, before: &Snapshot) -> DisplayResult<RouteBinding>;
    fn bind_clone(
        &mut self,
        display: &str,
        source: &Address,
        before: &Snapshot,
    ) -> DisplayResult<RouteBinding>;
    fn prepare_preserving(
        &mut self,
        configuration: &Configuration,
        selected: &str,
        route: Option<&RouteBinding>,
        shape: TopologyShape,
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
        shape: TopologyShape,
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
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::LinuxPlatform as NativePlatform;
#[cfg(not(any(windows, target_os = "linux")))]
#[derive(Default)]
pub struct NativePlatform;
#[cfg(not(any(windows, target_os = "linux")))]
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
    fn bind_clone(&mut self, _: &str, _: &Address, _: &Snapshot) -> DisplayResult<RouteBinding> {
        unsupported()
    }
    fn prepare_preserving(
        &mut self,
        _: &Configuration,
        _: &str,
        _: Option<&RouteBinding>,
        _: TopologyShape,
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
        _: TopologyShape,
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
#[cfg(not(any(windows, target_os = "linux")))]
fn unsupported<T>() -> DisplayResult<T> {
    Err(crate::model::DisplayError::new(
        "unsupported_platform",
        "native physical display management is implemented only for Windows; no Linux/macOS fallback is active",
    ))
}
