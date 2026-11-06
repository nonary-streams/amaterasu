//! Synchronous views of negotiated capture resources.
use super::{BorrowedCpuFrame, OwnedCpuFrame};
use amaterasu_abi::{STATUS_INVALID_ARGUMENT, STATUS_OK, Status};
use amaterasu_media::{
    CaptureResourceFrame, CpuFrame, FRAME_RESOURCE_CPU, FRAME_SYNC_NONE, FrameClockMetadata,
    FrameColorMetadata, FrameDescriptor,
};
use std::{marker::PhantomData, rc::Rc};

/// A frame borrowed from a synchronous capture callback. It cannot cross threads
/// or outlive that callback. Unknown resource schemas remain opaque.
pub struct BorrowedResourceFrame<'a> {
    frame: &'a CaptureResourceFrame,
    module_thread: PhantomData<Rc<()>>,
}

/// Independent CPU pixels and the original capture presentation metadata.
/// Color and clock values are copied verbatim, including unknown versions and
/// domains; their presence alone does not establish a color or clock meaning.
pub struct OwnedResourceCpuFrame {
    pub pixels: OwnedCpuFrame,
    pub offer_generation: u64,
    pub offer_id: u64,
    pub timestamp_us: u64,
    pub capture_pacing_timestamp_us: u64,
    pub sequence: u64,
    pub width: u32,
    pub height: u32,
    pub freshness: u32,
    pub color: FrameColorMetadata,
    pub clock: FrameClockMetadata,
    pub pacing_clock: FrameClockMetadata,
}

/// One explicitly retained presentation. No safe SDK operation starts GPU work.
/// Drop releases with NONE, which asserts that all consumer work has completed.
/// The native binding and this reference are confined to the module thread.
#[must_use = "dropping releases this frame reference"]
pub struct RetainedResourceFrame<'provider> {
    frame: Option<CaptureResourceFrame>,
    provider: PhantomData<&'provider crate::NativeBinding>,
    module_thread: PhantomData<Rc<()>>,
}

impl<'provider> RetainedResourceFrame<'provider> {
    /// Retains a presentation from its synchronous callback.
    ///
    /// # Safety
    /// The frame and its callbacks must obey the negotiated lease ABI, and
    /// `provider` must be the binding that owns them. The provider and binding
    /// must remain alive through any pending completion fences. Call on the
    /// owning module thread. Any GPU access started using raw handles must be
    /// completed before ordinary drop, or explicitly released with a valid
    /// completion descriptor covering every pending access, including on unwind.
    pub unsafe fn retain_from_borrowed(
        frame: &CaptureResourceFrame,
        _provider: &'provider crate::NativeBinding,
    ) -> Result<Self, Status> {
        // SAFETY: caller establishes provider identity and lease guarantees.
        unsafe { Self::retain_reference(frame) }
    }

    unsafe fn retain_reference(frame: &CaptureResourceFrame) -> Result<Self, Status> {
        // SAFETY: caller guarantees a live provider callback on its owner thread.
        let status = unsafe { (frame.retain)(frame.context) };
        if status != STATUS_OK {
            return Err(status);
        }
        Ok(Self {
            frame: Some(*frame),
            provider: PhantomData,
            module_thread: PhantomData,
        })
    }

    /// Descriptor pointers and handles remain borrowed from the provider. A
    /// downstream stage must retain independently if it outlives its call.
    pub fn descriptor(&self) -> &CaptureResourceFrame {
        self.frame.as_ref().expect("live retained frame")
    }

    /// Consume this reference using the negotiated completion schema.
    ///
    /// # Safety
    /// The descriptor and its borrowed data/handles must be valid for this call
    /// and describe completion of every pending access under this reference.
    /// No handles or descriptor data from this reference may be used afterward.
    /// The provider and its binding must remain alive until the provider has
    /// retired any pending completion work.
    pub unsafe fn release_after(mut self, completion: &FrameDescriptor) {
        let frame = self.frame.take().expect("live retained frame");
        // SAFETY: caller provides a valid completion and this owns one reference.
        unsafe { (frame.release)(frame.context, completion) };
    }
}

impl Drop for RetainedResourceFrame<'_> {
    fn drop(&mut self) {
        if let Some(frame) = self.frame.take() {
            let complete = FrameDescriptor {
                kind: FRAME_SYNC_NONE,
                version: 0,
                size: 0,
                reserved: 0,
                namespace: [0; 16],
                data: std::ptr::null(),
            };
            // SAFETY: construction requires all work to complete before Drop;
            // this value holds exactly one reference on the owner thread.
            unsafe { (frame.release)(frame.context, &complete) };
        }
    }
}

impl<'a> BorrowedResourceFrame<'a> {
    /// Wraps a provider-owned frame.
    ///
    /// # Safety
    /// The frame, all descriptor data and handles, and any CPU pixel allocation
    /// must remain valid and immutable for `'a` and obey the negotiated ABI.
    /// Construct and use the view on the provider's owning module thread.
    pub unsafe fn from_descriptor(frame: &'a CaptureResourceFrame) -> Self {
        Self {
            frame,
            module_thread: PhantomData,
        }
    }

    /// Raw descriptor fields are opaque until their schema has been agreed.
    pub fn descriptor(&self) -> &CaptureResourceFrame {
        self.frame
    }

    /// Copy a standard CPU v1 or v2 presentation and its metadata into independent
    /// storage. Metadata must agree between the presentation and CPU descriptor.
    /// This does not retain the source resource or interpret color/clock values.
    pub fn copy_cpu(&self) -> Result<OwnedResourceCpuFrame, Status> {
        let resource = &self.frame.resource;
        let sync = &self.frame.acquire_sync;
        if self.frame.reserved != 0
            || resource.kind != FRAME_RESOURCE_CPU
            || !matches!(resource.version, 1 | 2)
            || resource.reserved != 0
            || resource.namespace != [0; 16]
            || (resource.size as usize) < std::mem::size_of::<CpuFrame>()
            || resource.data.is_null()
            || !(resource.data as usize).is_multiple_of(std::mem::align_of::<CpuFrame>())
            || sync.kind != FRAME_SYNC_NONE
            || sync.version != 0
            || sync.size != 0
            || sync.reserved != 0
            || sync.namespace != [0; 16]
            || !sync.data.is_null()
        {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        if resource.version == 2 {
            use amaterasu_media::*;
            let color = self.frame.color;
            if color.version != 1
                || color.size != size_of::<FrameColorMetadata>() as u32
                || !matches!(
                    color.primaries,
                    FRAME_PRIMARIES_BT709 | FRAME_PRIMARIES_BT2020
                )
                || !matches!(
                    color.transfer,
                    FRAME_TRANSFER_SRGB
                        | FRAME_TRANSFER_BT709
                        | FRAME_TRANSFER_PQ
                        | FRAME_TRANSFER_HLG
                )
                || !matches!(
                    color.matrix,
                    FRAME_MATRIX_RGB | FRAME_MATRIX_BT709 | FRAME_MATRIX_BT2020_NCL
                )
                || !matches!(color.range, FRAME_RANGE_FULL | FRAME_RANGE_LIMITED)
            {
                return Err(STATUS_INVALID_ARGUMENT);
            }
        }
        // SAFETY: construction guarantees descriptor data is live and readable;
        // the schema, size and alignment were checked above.
        let cpu = unsafe { &*resource.data.cast::<CpuFrame>() };
        if cpu.width != self.frame.width
            || cpu.height != self.frame.height
            || cpu.timestamp_us != self.frame.timestamp_us
            || cpu.sequence != self.frame.sequence
        {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        // SAFETY: construction guarantees the CPU allocation's lifetime and
        // immutability. BorrowedCpuFrame validates the layout before copying.
        let pixels = unsafe { BorrowedCpuFrame::from_descriptor(cpu)? }.to_owned();
        Ok(OwnedResourceCpuFrame {
            pixels,
            offer_generation: self.frame.offer_generation,
            offer_id: self.frame.offer_id,
            timestamp_us: self.frame.timestamp_us,
            capture_pacing_timestamp_us: self.frame.capture_pacing_timestamp_us,
            sequence: self.frame.sequence,
            width: self.frame.width,
            height: self.frame.height,
            freshness: self.frame.freshness,
            color: self.frame.color,
            clock: self.frame.clock,
            pacing_clock: self.frame.pacing_clock,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use amaterasu_abi::STATUS_OK;
    use amaterasu_media::{FrameDescriptor, PIXEL_BGRA8};
    use std::cell::Cell;
    use std::ffi::c_void;

    unsafe extern "C" fn retain(_: *mut c_void) -> Status {
        STATUS_OK
    }
    unsafe extern "C" fn release(_: *mut c_void, _: *const FrameDescriptor) {}

    #[test]
    fn cpu_copy_is_independent_and_checks_schema_and_metadata() {
        let mut pixels = [9; 16];
        let cpu = CpuFrame {
            width: 2,
            height: 2,
            format: PIXEL_BGRA8,
            stride: 8,
            data: pixels.as_ptr(),
            len: pixels.len(),
            timestamp_us: 12,
            sequence: 3,
        };
        let none = FrameDescriptor {
            kind: FRAME_SYNC_NONE,
            version: 0,
            size: 0,
            reserved: 0,
            namespace: [0; 16],
            data: std::ptr::null(),
        };
        let mut frame = CaptureResourceFrame {
            offer_generation: 1,
            offer_id: 1,
            timestamp_us: 12,
            capture_pacing_timestamp_us: 10,
            sequence: 3,
            width: 2,
            height: 2,
            freshness: 1,
            reserved: 0,
            color: FrameColorMetadata {
                version: 1,
                size: 24,
                primaries: 2,
                transfer: 3,
                matrix: 4,
                range: 5,
            },
            clock: FrameClockMetadata {
                version: 1,
                size: 32,
                domain: 6,
                reserved: 0,
                epoch_id: [7; 16],
            },
            pacing_clock: FrameClockMetadata {
                version: 1,
                size: 32,
                domain: 8,
                reserved: 0,
                epoch_id: [9; 16],
            },
            resource: FrameDescriptor {
                kind: FRAME_RESOURCE_CPU,
                version: 1,
                size: std::mem::size_of::<CpuFrame>() as u32,
                reserved: 0,
                namespace: [0; 16],
                data: (&cpu as *const CpuFrame).cast(),
            },
            acquire_sync: none,
            context: std::ptr::null_mut(),
            retain,
            release,
        };
        // SAFETY: fixture descriptors and pixels remain live during the view.
        let owned = unsafe { BorrowedResourceFrame::from_descriptor(&frame) }
            .copy_cpu()
            .unwrap();
        pixels.fill(0);
        assert_eq!(owned.pixels.pixels(), &[9; 16]);
        assert_eq!(
            (
                owned.offer_generation,
                owned.offer_id,
                owned.timestamp_us,
                owned.capture_pacing_timestamp_us,
                owned.sequence,
                owned.width,
                owned.height,
                owned.freshness
            ),
            (1, 1, 12, 10, 3, 2, 2, 1)
        );
        assert_eq!(
            (
                owned.color.version,
                owned.color.size,
                owned.color.primaries,
                owned.color.transfer,
                owned.color.matrix,
                owned.color.range
            ),
            (1, 24, 2, 3, 4, 5)
        );
        assert_eq!(
            (
                owned.clock.version,
                owned.clock.size,
                owned.clock.domain,
                owned.clock.reserved,
                owned.clock.epoch_id
            ),
            (1, 32, 6, 0, [7; 16])
        );
        assert_eq!(
            (
                owned.pacing_clock.version,
                owned.pacing_clock.size,
                owned.pacing_clock.domain,
                owned.pacing_clock.reserved,
                owned.pacing_clock.epoch_id
            ),
            (1, 32, 8, 0, [9; 16])
        );
        frame.resource.version = 3;
        // SAFETY: same fixture lifetime; the unknown schema is rejected.
        assert!(
            unsafe { BorrowedResourceFrame::from_descriptor(&frame) }
                .copy_cpu()
                .is_err()
        );
        frame.resource.version = 2;
        frame.color = crate::capture::high_depth::p010_color(
            true,
            crate::capture::high_depth::YuvRange::Limited,
        );
        // SAFETY: V2 retains the canonical CPU frame layout and known color.
        let explicit = unsafe { BorrowedResourceFrame::from_descriptor(&frame) }
            .copy_cpu()
            .unwrap();
        assert_eq!(explicit.color.transfer, amaterasu_media::FRAME_TRANSFER_PQ);
        frame.color.range = 0;
        // SAFETY: same fixture, but V2 cannot omit negotiated color.
        assert!(
            unsafe { BorrowedResourceFrame::from_descriptor(&frame) }
                .copy_cpu()
                .is_err()
        );
        frame.resource.version = 1;
        frame.sequence += 1;
        // SAFETY: same fixture lifetime; inconsistent metadata is rejected.
        assert!(
            unsafe { BorrowedResourceFrame::from_descriptor(&frame) }
                .copy_cpu()
                .is_err()
        );
    }

    #[test]
    fn retained_reference_releases_once_and_propagates_rejection() {
        struct Counts {
            retains: Cell<u32>,
            releases: Cell<u32>,
            completion: Cell<u32>,
            reject: Cell<bool>,
        }
        unsafe extern "C" fn acquire(context: *mut c_void) -> Status {
            // SAFETY: fixture keeps Counts alive through every callback.
            let counts = unsafe { &*context.cast::<Counts>() };
            if counts.reject.get() {
                return STATUS_INVALID_ARGUMENT;
            }
            counts.retains.set(counts.retains.get() + 1);
            STATUS_OK
        }
        unsafe extern "C" fn retire(context: *mut c_void, completion: *const FrameDescriptor) {
            // SAFETY: fixture context and completion live for this call.
            let counts = unsafe { &*context.cast::<Counts>() };
            counts.releases.set(counts.releases.get() + 1);
            counts.completion.set(unsafe { (*completion).kind });
        }
        let counts = Counts {
            retains: Cell::new(0),
            releases: Cell::new(0),
            completion: Cell::new(0),
            reject: Cell::new(false),
        };
        let none = FrameDescriptor {
            kind: FRAME_SYNC_NONE,
            version: 0,
            size: 0,
            reserved: 0,
            namespace: [0; 16],
            data: std::ptr::null(),
        };
        let frame = CaptureResourceFrame {
            offer_generation: 1,
            offer_id: 1,
            timestamp_us: 1,
            capture_pacing_timestamp_us: 1,
            sequence: 1,
            width: 1,
            height: 1,
            freshness: 1,
            reserved: 0,
            color: Default::default(),
            clock: Default::default(),
            pacing_clock: Default::default(),
            resource: none,
            acquire_sync: none,
            context: (&counts as *const Counts).cast_mut().cast(),
            retain: acquire,
            release: retire,
        };
        // SAFETY: fixture owns all callbacks and does not start GPU work.
        drop(unsafe { RetainedResourceFrame::retain_reference(&frame) }.unwrap());
        assert_eq!(
            (
                counts.retains.get(),
                counts.releases.get(),
                counts.completion.get()
            ),
            (1, 1, FRAME_SYNC_NONE)
        );
        // SAFETY: fixture owns callbacks and accepts this synthetic completion.
        let retained = unsafe { RetainedResourceFrame::retain_reference(&frame) }.unwrap();
        let completion = FrameDescriptor { kind: 42, ..none };
        // SAFETY: this fixture uses a synthetic completion and starts no work.
        unsafe { retained.release_after(&completion) };
        assert_eq!(
            (
                counts.retains.get(),
                counts.releases.get(),
                counts.completion.get()
            ),
            (2, 2, 42)
        );
        counts.reject.set(true);
        // SAFETY: the fixture context and callbacks are still live.
        assert!(unsafe { RetainedResourceFrame::retain_reference(&frame) }.is_err());
        assert_eq!(counts.releases.get(), 2);
    }
}
