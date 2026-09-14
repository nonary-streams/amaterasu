mod model;
pub use model::{CaptureSelection, GeometryError, SelectedOutput};

#[cfg(windows)]
mod platform;
#[cfg(windows)]
pub use platform::resolve_with;

#[cfg(test)]
mod topology_tests;
