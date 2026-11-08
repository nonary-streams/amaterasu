//! Helpers for borrowed CPU frames and explicitly retained GPU presentations.
/// Native interface families used for typed registration and binding. These
/// identities are independent of service aliases and imply the complete ABI
/// and ownership contract for each family.
pub mod native_contracts {
    pub const CAPTURE: &str = "amaterasu.video-capture.native/1";
    pub const ENCODER: &str = "amaterasu.video-encoder.native/1";
    pub const ENCODER_PROVIDER: &str = "amaterasu.encoder-provider.native/1";
    pub const FRAME_RESOURCE_CONSUMER: &str = "amaterasu.frame-resource-consumer.native/1";
    pub const FRAME_RESOURCE_PROCESSOR: &str = "amaterasu.frame-resource-processor.native/1";
    pub const CPU_PROCESSOR: &str = "amaterasu.cpu-frame-processor.native/1";
    pub const GPU_PROCESSOR: &str = "amaterasu.gpu-frame-processor.native/1";
}

mod generic;
mod lease;
mod readiness;
mod timing;
use amaterasu_abi::{STATUS_INVALID_ARGUMENT, Status};
use amaterasu_media::{CpuFrame, frame_size};
pub use generic::{BorrowedResourceFrame, OwnedResourceCpuFrame, RetainedResourceFrame};
pub use lease::RetainedDmaBufFrame;
pub use readiness::encoder_gpu_input_ready;
pub use timing::*;

/// A validated CPU frame whose pixels remain borrowed for its lifetime.
/// Native callbacks may read the descriptor only during the synchronous call;
/// consumers must copy pixels they need after it returns.
pub struct BorrowedCpuFrame<'a> {
    frame: CpuFrame,
    pixels: &'a [u8],
}

impl<'a> BorrowedCpuFrame<'a> {
    /// Wraps a descriptor received from a native capture callback.
    ///
    /// # Safety
    /// Its pixel allocation must contain the advertised initialized bytes and
    /// remain live and immutable for `'a`. The descriptor must come from a
    /// trusted provider obeying the synchronous callback lifetime contract.
    pub unsafe fn from_descriptor(frame: &'a CpuFrame) -> Result<Self, Status> {
        // SAFETY: the caller guarantees the allocation and borrowing contract.
        let pixels = unsafe { frame.bytes()? };
        Ok(Self {
            frame: *frame,
            pixels,
        })
    }

    /// Constructs a tightly described BGRA or NV12 frame. Row padding is
    /// represented by `stride` and must be included in `pixels`.
    pub fn new(
        width: u32,
        height: u32,
        format: u32,
        stride: u32,
        pixels: &'a [u8],
        timestamp_us: u64,
        sequence: u64,
    ) -> Result<Self, Status> {
        if pixels.len() != frame_size(width, height, format, stride)? {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        Ok(Self {
            frame: CpuFrame {
                width,
                height,
                format,
                stride,
                data: pixels.as_ptr(),
                len: pixels.len(),
                timestamp_us,
                sequence,
            },
            pixels,
        })
    }

    /// The descriptor and its pixels remain live while this value is borrowed.
    pub fn descriptor(&self) -> &CpuFrame {
        &self.frame
    }

    pub fn pixels(&self) -> &'a [u8] {
        self.pixels
    }

    /// Copies pixels and metadata into independent storage for later work.
    pub fn to_owned(&self) -> OwnedCpuFrame {
        OwnedCpuFrame {
            width: self.frame.width,
            height: self.frame.height,
            format: self.frame.format,
            stride: self.frame.stride,
            pixels: self.pixels.to_vec(),
            timestamp_us: self.frame.timestamp_us,
            sequence: self.frame.sequence,
        }
    }
}

/// Independent CPU pixels and their capture metadata. This does not retain a
/// capture-provider buffer or represent a GPU frame lease.
pub struct OwnedCpuFrame {
    width: u32,
    height: u32,
    format: u32,
    stride: u32,
    pixels: Vec<u8>,
    timestamp_us: u64,
    sequence: u64,
}

impl OwnedCpuFrame {
    /// Validates owned storage against the frame layout.
    pub fn new(
        width: u32,
        height: u32,
        format: u32,
        stride: u32,
        pixels: Vec<u8>,
        timestamp_us: u64,
        sequence: u64,
    ) -> Result<Self, Status> {
        if pixels.len() != frame_size(width, height, format, stride)? {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        Ok(Self {
            width,
            height,
            format,
            stride,
            pixels,
            timestamp_us,
            sequence,
        })
    }

    /// Creates a descriptor whose pixels are borrowed from this frame. Use it
    /// only for a synchronous callback or encoder submit call.
    pub fn borrow(&self) -> BorrowedCpuFrame<'_> {
        BorrowedCpuFrame {
            frame: CpuFrame {
                width: self.width,
                height: self.height,
                format: self.format,
                stride: self.stride,
                data: self.pixels.as_ptr(),
                len: self.pixels.len(),
                timestamp_us: self.timestamp_us,
                sequence: self.sequence,
            },
            pixels: &self.pixels,
        }
    }

    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// Processing may change pixels in place without changing the valid layout.
    pub fn pixels_mut(&mut self) -> &mut [u8] {
        &mut self.pixels
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use amaterasu_media::{PIXEL_BGRA8, PIXEL_NV12};

    #[test]
    fn borrowed_frame_preserves_pixels_and_metadata() {
        let pixels = [7; 24];
        let frame = BorrowedCpuFrame::new(4, 4, PIXEL_NV12, 4, &pixels, 9, 2).unwrap();
        assert_eq!(frame.pixels(), &pixels);
        assert_eq!(frame.descriptor().data, pixels.as_ptr());
        assert_eq!(frame.descriptor().timestamp_us, 9);
        assert_eq!(frame.descriptor().sequence, 2);
        assert!(frame.descriptor().validate().is_ok());
    }

    #[test]
    fn borrowed_frame_rejects_invalid_layout_and_length() {
        let pixels = [0; 24];
        assert!(BorrowedCpuFrame::new(4, 4, PIXEL_NV12, 4, &pixels[..23], 0, 0).is_err());
        assert!(BorrowedCpuFrame::new(3, 4, PIXEL_NV12, 4, &pixels, 0, 0).is_err());
        assert!(BorrowedCpuFrame::new(4, 4, PIXEL_BGRA8, 4, &pixels, 0, 0).is_err());
    }

    #[test]
    fn owned_copy_outlives_source_and_keeps_metadata() {
        let mut source = vec![7; 24];
        let frame = BorrowedCpuFrame::new(4, 4, PIXEL_NV12, 4, &source, 99, 8).unwrap();
        // SAFETY: the source pixels remain live and immutable during this borrow.
        let owned = unsafe { BorrowedCpuFrame::from_descriptor(frame.descriptor()) }
            .unwrap()
            .to_owned();
        source.fill(3);
        drop(source);
        let borrowed = owned.borrow();
        assert_eq!(borrowed.pixels(), &[7; 24]);
        let descriptor = borrowed.descriptor();
        assert_eq!(
            (
                descriptor.width,
                descriptor.height,
                descriptor.format,
                descriptor.stride
            ),
            (4, 4, PIXEL_NV12, 4)
        );
        assert_eq!((descriptor.timestamp_us, descriptor.sequence), (99, 8));
        assert_eq!(descriptor.data, owned.pixels().as_ptr());
    }

    #[test]
    fn owned_frame_checks_layout_and_supports_processing() {
        assert!(OwnedCpuFrame::new(4, 4, PIXEL_NV12, 4, vec![0; 23], 0, 0).is_err());
        let mut frame = OwnedCpuFrame::new(4, 4, PIXEL_NV12, 4, vec![0; 24], 0, 1).unwrap();
        frame.pixels_mut()[0] = 42;
        assert_eq!(frame.borrow().pixels()[0], 42);
        assert!(frame.borrow().descriptor().validate().is_ok());
    }
}
