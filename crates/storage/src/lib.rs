//! Durable file primitives shared by state and package libraries.
//! This crate has no knowledge of release signatures, sessions, or configuration schemas.
mod files;

pub use files::{
    RootLock, create_machine_private_directory, create_private_directory,
    protect_machine_private_file, read_bounded, read_json, seal_machine_private_directory,
    seal_machine_private_file, sync_directory, write_atomically, write_json_atomically,
};

pub type StorageResult<T> = Result<T, String>;

pub fn random_identifier() -> StorageResult<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|error| error.to_string())?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}
