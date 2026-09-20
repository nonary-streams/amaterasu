//! Package integrity, trust verification, and local activation primitives.
//!
//! These primitives support development packages. Production release repository
//! metadata, key rotation, and installed-system permissions are not implemented yet.

mod dependencies;
mod digest;
pub mod handoff;
pub mod importer;
mod manifest;
pub mod migration;
mod staging;
mod storage;
mod verification;
pub mod web;

pub use dependencies::ordered_components;
pub use digest::{decode_hex, encode_hex, random_identifier, sha256};
pub use manifest::*;
pub use staging::{package_directory, stage_package};
pub use storage::{
    RootLock, create_machine_private_directory, create_private_directory, read_bounded, read_json,
    write_json_atomically,
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
