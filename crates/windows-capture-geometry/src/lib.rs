mod model;
pub use model::{CaptureSelection, GeometryError, SelectedOutput};

#[cfg(windows)]
mod platform;
#[cfg(windows)]
pub use platform::{resolve_with, with_verified_physical_topology};

#[cfg(test)]
mod topology_tests;
