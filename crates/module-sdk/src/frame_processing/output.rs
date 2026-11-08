use amaterasu_media::{CaptureResourceFrame, FRAME_SYNC_NONE, FrameDescriptor};
/// One provider-owned reference to a negotiated resource output (processor). The SDK delivers it, then releases this initial reference; consumers
/// retain their own. Descriptor payloads must stay valid while referenced.
pub struct ResourceOutputFrame {
    frame: CaptureResourceFrame,
}

impl ResourceOutputFrame {
    /// # Safety
    /// Transfers exactly one live reference. The frame's descriptors, payloads
    /// and storage obey the selected schema and the frame-resource retain and
    /// release rules, and the provider stays loaded until every reference and
    /// any pending work retire.
    pub unsafe fn from_owned(frame: CaptureResourceFrame) -> Self {
        Self { frame }
    }

    pub fn frame(&self) -> &CaptureResourceFrame {
        &self.frame
    }
}

impl Drop for ResourceOutputFrame {
    fn drop(&mut self) {
        // The initial reference covers no consumer work: release with NONE.
        let none = FrameDescriptor {
            kind: FRAME_SYNC_NONE,
            version: 0,
            size: 0,
            reserved: 0,
            namespace: [0; 16],
            data: std::ptr::null(),
        };
        // SAFETY: construction transferred one reference; delivery only borrows it.
        unsafe { (self.frame.release)(self.frame.context, &none) };
    }
}
