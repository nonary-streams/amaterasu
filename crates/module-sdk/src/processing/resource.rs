//! Explicit standard CPU resource processing. Source and output colors are
//! independently negotiated; neither a storage format nor a service alias
//! establishes color intent. No stock transform or fallback is supplied.
mod consumer;
mod factory;
mod format;
pub use consumer::{CpuProcessingTarget, CpuResourceProcessingSession};
pub use factory::{
    CpuResourceProcessor, ResourceProcessorFactory, into_resource_processor_binding,
};
pub use format::{CpuProcessingDescription, CpuProcessingOffer, CpuResourceInput};

pub const MAX_PROCESSING_OFFERS: usize = 64;
pub const MAX_PROCESSING_STREAMS: usize = 64;

#[cfg(test)]
mod tests;
