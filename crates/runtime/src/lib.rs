//! Replaceable control-plane runtime with bounded queues and explicit caller authority.
//!
//! Request routing, event delivery, and hooks are independent subsystems. The broker
//! authenticates an endpoint once and delegates to the appropriate subsystem.

mod broker;
mod ffi;
mod native;

pub use broker::Broker;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod input_extension_tests;
