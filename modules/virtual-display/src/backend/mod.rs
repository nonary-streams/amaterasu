use crate::model::Specification;
use crate::protocol::Binding;
use serde::Serialize;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::NativeDriver;

#[derive(Clone, Debug, Serialize)]
pub struct Ready {
    pub device_path: String,
    pub active: bool,
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
