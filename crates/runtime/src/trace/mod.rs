//! Flow tracing: where time goes between the stages of one input event or frame.
//!
//! Producers record fixed-size observations into a lock-free queue. A collector
//! thread owned by the runtime joins them into flows, keeps per-step statistics
//! and explains slow flows with the thread spans that overlapped them. Nothing
//! here runs on a producer's thread except the queue push.
mod collector;
mod hub;
mod names;
mod report;
mod ring;
pub(crate) mod sink;

pub(crate) use hub::TraceHub;

#[cfg(test)]
mod tests;
