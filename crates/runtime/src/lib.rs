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
