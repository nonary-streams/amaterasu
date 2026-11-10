//! Replaceable control-plane runtime with bounded queues and explicit caller authority.
//!
//! Request routing, event delivery, and hooks are independent subsystems. The broker
//! authenticates an endpoint once and delegates to the appropriate subsystem.

mod broker;
mod direct;
mod ffi;
mod invocation_timing;
mod native;
mod tick_holds;
mod trace;
mod work_signal;

pub use broker::Broker;
/// The runtime's C entry points, for a host that links this crate instead of
/// loading the built library. The module test kit creates its runtime this way.
pub use ffi::{amaterasu_runtime_direct_v1, amaterasu_runtime_v1, amaterasu_runtime_wait_v1};

#[cfg(test)]
mod display_target_tests;
#[cfg(test)]
mod pairing_authority_tests;
#[cfg(test)]
mod retirement_contract_tests;
#[cfg(test)]
mod scoped_display_contract_tests;
#[cfg(test)]
mod session_presentation_tests;
#[cfg(test)]
mod tests;
