//! Immediate, capture-independent frame processing. Resource descriptors and
//! synchronization travel unchanged; the SDK owns no pixel buffers or GPU waits.
mod control;
mod factory;
mod input;
mod output;
mod session;
pub use control::{configure_frame_stream, update_frame_stream};
pub use factory::{FrameProcessor, FrameProcessorFactory, into_frame_processor_binding};
pub use input::{ProcessingFrame, RetainedProcessingInput};
pub use output::ResourceOutputFrame;
pub use session::{
    FrameDelivery, FrameProcessingSession, FrameProcessingStage, MAX_PROCESSING_STAGES,
};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameResourceSelection {
    pub generation: u64,
    pub offer: u64,
}

pub const MAX_PROCESSING_STREAMS: usize = 64;
pub const MAX_PROCESSING_OFFERS: usize = 64;

#[cfg(test)]
mod tests;
