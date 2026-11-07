mod model;
pub use model::{CaptureSelection, GeometryError, SelectedOutput};

#[cfg(windows)]
mod platform;
#[cfg(windows)]
pub use platform::{
    resolve_target_matching, resolve_with, sdr_white_level_for_target,
    with_verified_physical_topology,
};

#[cfg(test)]
mod topology_tests;
