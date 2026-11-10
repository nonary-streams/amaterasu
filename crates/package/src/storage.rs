//! Shared storage primitives are independent of package signatures and metadata.
pub use amaterasu_storage::{
    ROOT_BUSY, RootLock, create_machine_private_directory, create_private_directory,
    protect_machine_private_file, read_bounded, read_json, sync_directory, write_json_atomically,
};
