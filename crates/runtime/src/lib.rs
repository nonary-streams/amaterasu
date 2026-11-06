//! Replaceable control-plane runtime with bounded queues and explicit caller authority.
//!
//! Request routing, event delivery, and hooks are independent subsystems. The broker
//! authenticates an endpoint once and delegates to the appropriate subsystem.

mod broker;
mod ffi;
mod native;
mod work_signal;

pub use broker::Broker;

#[cfg(test)]
mod pairing_authority_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod session_presentation_tests;
