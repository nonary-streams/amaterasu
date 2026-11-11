use crate::model::Specification;
use crate::protocol::Binding;
use serde::Serialize;

#[cfg(any(windows, test))]
mod capture_target;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::NativeDriver;

#[derive(Clone, Debug, Serialize)]
pub struct Ready {
    pub device_path: String,
    pub active: bool,
    /// Opaque exact selector, present only after mode, color and capture
    /// enumeration independently agree on this owned target.
    pub capture_output: Option<String>,
    pub capture_error: Option<&'static str>,
}
pub struct Observation {
    pub exists: bool,
    pub occupied_by_other_lease: bool,
    pub specification_matches: bool,
    pub lease_display_count: u32,
    pub pending_departure: bool,
    pub ready: Option<Ready>,
}

pub trait Driver {
    /// `None` means no compatible producer handshake has been observed yet.
    fn requested_resolution_catalog_supported(&self) -> Option<bool> {
        None
    }
    /// Read-only capability validation before a durable allocation intent.
    fn validate_create(&mut self, specification: &Specification) -> Result<(), &'static str> {
        if specification.requested_resolution_only
            && self.requested_resolution_catalog_supported() != Some(true)
        {
            return Err("unsupported_requested_resolution_catalog");
        }
        Ok(())
    }
    /// Permanent displays are configured only from the trusted startup settings.
    fn permanent_count_supported(&self) -> bool {
        false
    }
    fn query_permanent_count(&mut self) -> Result<u32, &'static str> {
        Err("unsupported_permanent_monitors")
    }
    fn set_permanent_count(&mut self, _count: u8) -> Result<(), &'static str> {
        Err("unsupported_permanent_monitors")
    }
    /// Independently observe a requested effective mode. This never changes the
    /// native allocation and cannot stand in for ownership inspection.
    fn capture_target(
        &mut self,
        _binding: Option<Binding>,
        _expected: &Specification,
    ) -> Result<Option<Ready>, &'static str> {
        Err("unsupported_mode_observation")
    }
    fn supported(&self) -> bool;
    fn create(
        &mut self,
        specification: &Specification,
        lease: u64,
        display: u64,
        capability: &[u8; 32],
    ) -> Result<Binding, &'static str>;
    fn inspect(
        &mut self,
        lease: u64,
        display: u64,
        binding: Option<Binding>,
        specification: &Specification,
    ) -> Result<Observation, &'static str>;
    fn feed(&mut self, lease: u64) -> Result<(), &'static str>;
    fn reclaim(
        &mut self,
        display: u64,
        lease: u64,
        capability: &[u8; 32],
    ) -> Result<(), &'static str>;
    fn remove(&mut self, lease: u64, display: u64) -> Result<(), &'static str>;
}

#[cfg(not(windows))]
#[derive(Default)]
#[cfg_attr(all(amaterasu_libvirtualdisplay, test), allow(dead_code))]
pub struct NativeDriver;

impl NativeDriver {
    #[cfg_attr(all(amaterasu_libvirtualdisplay, test), allow(dead_code))]
    pub fn new() -> Self {
        #[cfg(windows)]
        {
            Self::default()
        }
        #[cfg(not(windows))]
        {
            Self
        }
    }
}
#[cfg(not(windows))]
impl Driver for NativeDriver {
    fn supported(&self) -> bool {
        false
    }
    fn create(
        &mut self,
        _: &Specification,
        _: u64,
        _: u64,
        _: &[u8; 32],
    ) -> Result<Binding, &'static str> {
        Err("unsupported_platform")
    }
    fn inspect(
        &mut self,
        _: u64,
        _: u64,
        _: Option<Binding>,
        _: &Specification,
    ) -> Result<Observation, &'static str> {
        Err("unsupported_platform")
    }
    fn feed(&mut self, _: u64) -> Result<(), &'static str> {
        Err("unsupported_platform")
    }
    fn reclaim(&mut self, _: u64, _: u64, _: &[u8; 32]) -> Result<(), &'static str> {
        Err("unsupported_platform")
    }
    fn remove(&mut self, _: u64, _: u64) -> Result<(), &'static str> {
        Err("unsupported_platform")
    }
}
