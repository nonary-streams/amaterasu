//! Bounded ENet wire codec for the uncompressed, checksum-free GameStream profile.
pub mod peer;
mod types;
mod wire;

pub use types::*;
pub use wire::{decode, encode};

#[cfg(test)]
mod tests;
