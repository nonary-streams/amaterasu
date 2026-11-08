use crate::abi::{STATUS_OK, Status};
use amaterasu_media::{CaptureResourceFrame, FRAME_SYNC_NONE, FrameDescriptor};

/// Synchronous view of any negotiated resource. Access requires the provider's
/// exact resource schema and acquire dependency; a descriptor is not ready pixels.
pub struct ProcessingFrame<'a> {
    pub(super) frame: &'a CaptureResourceFrame,
}
impl ProcessingFrame<'_> {
    pub fn descriptor(&self) -> &CaptureResourceFrame {
        self.frame
    }
    /// Offers this same resource to the next stage under its selected output
    /// identity. No allocation, pixel copy, or wait is introduced. Independent
    /// readers retain the original provider's reference and readiness signal.
    pub fn forward(
        &self,
        output: super::FrameResourceSelection,
    ) -> Result<super::ResourceOutputFrame, Status> {
        if output.generation == 0 || output.offer == 0 {
            return Err(crate::abi::STATUS_INVALID_ARGUMENT);
        }
        let retained = self.retain()?;
        let frame = CaptureResourceFrame {
            offer_generation: output.generation,
            offer_id: output.offer,
            ..*retained.descriptor()
        };
        // SAFETY: retain transfers exactly one reference; this output takes it
        // over without changing storage, synchronization or lifetime callbacks.
        Ok(unsafe { super::ResourceOutputFrame::from_owned(frame) })
    }
    /// Retain before queuing work beyond this call. No pixels or handles copy.
    pub fn retain(&self) -> Result<RetainedProcessingInput, Status> {
        // SAFETY: the native adapter lends a live provider-owned reference.
        let status = unsafe { (self.frame.retain)(self.frame.context) };
        if status != STATUS_OK {
            return Err(status);
        }
        Ok(RetainedProcessingInput { frame: *self.frame })
    }
}
/// An independent input reference. An abandoned reference stays pinned rather
/// than authorizing storage reuse while GPU reads may still be outstanding.
/// The processor must release all references before its cancellation barrier.
#[must_use = "release after GPU completion or with its negotiated completion dependency"]
pub struct RetainedProcessingInput {
    frame: CaptureResourceFrame,
}
impl RetainedProcessingInput {
    /// Permanently pins this reference when completion cannot be proven. The
    /// provider's retirement barrier must keep its storage and code alive.
    pub fn quarantine(self) {}
    pub fn descriptor(&self) -> &CaptureResourceFrame {
        &self.frame
    }
    /// # Safety
    /// Completion must cover every access under this reference and obey its
    /// negotiated schema. Payloads/handles stay live through release, and the
    /// source binding remains live through retirement of the pending dependency.
    pub unsafe fn release_after(self, completion: &FrameDescriptor) {
        unsafe { (self.frame.release)(self.frame.context, completion) };
    }
    /// # Safety
    /// All accesses under this reference have completed.
    pub unsafe fn release_completed(self) {
        let none = FrameDescriptor {
            kind: FRAME_SYNC_NONE,
            version: 0,
            size: 0,
            reserved: 0,
            namespace: [0; 16],
            data: std::ptr::null(),
        };
        unsafe { self.release_after(&none) };
    }
}
