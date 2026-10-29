//! Machine-protected approval and Linux execution of capture authority.
//!
//! The root launcher creates a private channel for a verified unprivileged
//! worker. There is no listener accepting a caller-supplied package identity.
//! Native modules within that worker remain one trusted code cohort.
#[cfg(target_os = "linux")]
mod approval;
#[cfg(target_os = "linux")]
mod files;
#[cfg(target_os = "linux")]
mod launch;
#[cfg(target_os = "linux")]
mod proxy;
#[cfg(target_os = "linux")]
mod store;

#[cfg(target_os = "linux")]
pub use approval::{APPROVAL_SCHEMA, ApprovalRecord, approval_id, validate_approval};
#[cfg(target_os = "linux")]
pub use launch::{LaunchOptions, run};
#[cfg(target_os = "linux")]
pub use store::ApprovalStore;

pub type AuthorityResult<T> = Result<T, String>;
/// Only the root launcher assigns this inherited descriptor. The worker marks
/// it close-on-exec before any plugin can launch a child process.
pub const AUTHORITY_CHANNEL_FD: i32 = 198;
