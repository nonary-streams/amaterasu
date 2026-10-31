//! Package integrity, trust verification, and local activation primitives.
//!
//! Native activation remains separate from repository catalog verification.
//! Installed-service authorization and native publisher trust are not integrated.

mod dependencies;
mod digest;
pub mod handoff;
pub mod importer;
mod manifest;
pub mod migration;
pub mod playnite_extension;
mod privileged;
pub mod repository;
mod service_selection;
mod staging;
mod storage;
mod verification;
pub mod web;

pub use dependencies::ordered_components;
pub use digest::{decode_hex, encode_hex, random_identifier, sha256, sha256_reader};
pub use manifest::*;
pub use privileged::*;
pub use staging::{package_directory, stage_package};
pub use storage::{
    RootLock, create_machine_private_directory, create_private_directory,
    protect_machine_private_file, read_bounded, read_json, write_json_atomically,
};
pub use verification::{
    VerificationPolicy, verify_detached_signature, verify_package, verify_package_with_progress,
};

pub type PackageResult<T> = Result<T, String>;

pub fn current_target() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

pub fn unix_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock is before the Unix epoch")
        .as_secs()
}

#[cfg(test)]
mod tests;
