//! Bounded ownership of ready CPU resources, independent of their capture method.
use super::CpuSourcePresentation;
use crate::abi::*;
use amaterasu_media::*;
use std::{
    ffi::c_void,
    marker::PhantomData,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
const MAX_REFERENCES: usize = 64;
const MAX_BYTES: usize = 256 * 1024 * 1024;
struct Quota {
    max_references: usize,
    max_bytes: usize,
    references: AtomicUsize,
    bytes: AtomicUsize,
    quarantined: AtomicUsize,
}
fn reserve(counter: &AtomicUsize, amount: usize, limit: usize) -> Result<(), Status> {
    let mut current = counter.load(Ordering::Acquire);
    loop {
        let next = current
            .checked_add(amount)
            .filter(|value| *value <= limit)
            .ok_or(MEDIA_BUSY)?;
        match counter.compare_exchange_weak(current, next, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => return Ok(()),
            Err(updated) => current = updated,
        }
    }
}
#[derive(Clone)]
pub struct CpuFrameLeasePool {
    quota: Arc<Quota>,
}
impl Default for CpuFrameLeasePool {
    fn default() -> Self {
        Self::with_limits(MAX_REFERENCES, MAX_BYTES).expect("valid SDK limits")
    }
}
impl CpuFrameLeasePool {
    pub fn with_limits(max_references: usize, max_bytes: usize) -> Result<Self, Status> {
        if max_references == 0
            || max_references > MAX_REFERENCES
            || max_bytes == 0
            || max_bytes > MAX_BYTES
        {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        Ok(Self {
            quota: Arc::new(Quota {
                max_references,
                max_bytes,
                references: AtomicUsize::new(0),
                bytes: AtomicUsize::new(0),
                quarantined: AtomicUsize::new(0),
            }),
        })
    }
    pub fn retirement_ready(&self) -> Result<bool, Status> {
        if self.quota.quarantined.load(Ordering::Acquire) != 0 {
            return Err(MEDIA_BACKEND_ERROR);
        }
        Ok(self.quota.references.load(Ordering::Acquire) == 0)
    }
    pub fn lease(
        &self,
        mut presentation: CpuSourcePresentation,
        generation: u64,
        id: u64,
    ) -> Result<CpuResourceLease, Status> {
        if generation == 0 || id == 0 {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        let cpu = CpuFrame {
            width: presentation.width,
            height: presentation.height,
            format: presentation.pixel_format,
            stride: presentation.stride,
            data: presentation.bytes.as_ptr(),
            len: presentation.bytes.len(),
            timestamp_us: presentation.timestamp_us,
            sequence: presentation.sequence,
        };
        cpu.validate()?;
        reserve(&self.quota.references, 1, self.quota.max_references)?;
        if let Err(error) = reserve(&self.quota.bytes, cpu.len, self.quota.max_bytes) {
            self.quota.references.fetch_sub(1, Ordering::AcqRel);
            return Err(error);
        }
        let receipt = presentation.receipt.take();
        let owner = Rc::new(Owner {
            presentation,
            cpu,
            quota: self.quota.clone(),
        });
        let frame = CaptureResourceFrame {
            offer_generation: generation,
            offer_id: id,
            timestamp_us: cpu.timestamp_us,
            capture_pacing_timestamp_us: owner.presentation.capture_pacing_timestamp_us,
            sequence: cpu.sequence,
            width: cpu.width,
            height: cpu.height,
            freshness: owner.presentation.freshness,
            reserved: 0,
            color: owner.presentation.color,
            clock: Default::default(),
            pacing_clock: Default::default(),
            resource: FrameDescriptor {
                kind: FRAME_RESOURCE_CPU,
                version: if owner.presentation.color.version == 1 {
                    2
                } else {
                    1
                },
                size: std::mem::size_of::<CpuFrame>() as u32,
                reserved: 0,
                namespace: [0; 16],
                data: std::ptr::from_ref(&owner.cpu).cast(),
            },
            acquire_sync: NONE,
            context: Rc::as_ptr(&owner).cast_mut().cast(),
            retain,
            release,
        };
        let _ = Rc::into_raw(owner);
        Ok(CpuResourceLease {
            frame,
            receipt,
            module_thread: PhantomData,
        })
    }
}
const NONE: FrameDescriptor = FrameDescriptor {
    kind: FRAME_SYNC_NONE,
    version: 0,
    size: 0,
    reserved: 0,
    namespace: [0; 16],
    data: std::ptr::null(),
};
struct Owner {
    presentation: CpuSourcePresentation,
    cpu: CpuFrame,
    quota: Arc<Quota>,
}
impl Drop for Owner {
    fn drop(&mut self) {
        self.quota.bytes.fetch_sub(self.cpu.len, Ordering::AcqRel);
    }
}
/// Owns the initial source reference through synchronous delivery. Retained
/// consumers share the original allocation; they release only after their reads.
pub struct CpuResourceLease {
    frame: CaptureResourceFrame,
    receipt: Option<super::CpuPresentationReceipt>,
    module_thread: PhantomData<Rc<()>>,
}
impl CpuResourceLease {
    pub fn finish_delivery(&mut self, status: Status) {
        if let Some(receipt) = self.receipt.take() {
            receipt.finish(status);
        }
    }
    pub fn descriptor(&self) -> &CaptureResourceFrame {
        &self.frame
    }
}
impl Drop for CpuResourceLease {
    fn drop(&mut self) {
        // SAFETY: this owns the initial callback reference, which queued no work.
        unsafe { (self.frame.release)(self.frame.context, &NONE) }
    }
}
unsafe extern "C" fn retain(context: *mut c_void) -> Status {
    // SAFETY: a lent or retained reference pins this owner on its module thread.
    let owner = unsafe { &*context.cast::<Owner>() };
    if let Err(error) = reserve(&owner.quota.references, 1, owner.quota.max_references) {
        return error;
    }
    unsafe { Rc::increment_strong_count(context.cast::<Owner>()) };
    STATUS_OK
}
unsafe extern "C" fn release(context: *mut c_void, completion: *const FrameDescriptor) {
    // SAFETY: release consumes exactly one acquired reference on the owner thread.
    let owner = unsafe { Rc::from_raw(context.cast::<Owner>()) };
    let valid = !completion.is_null() && {
        let d = unsafe { &*completion };
        d.kind == FRAME_SYNC_NONE
            && d.version == 0
            && d.size == 0
            && d.reserved == 0
            && d.namespace == [0; 16]
            && d.data.is_null()
    };
    if !valid {
        // Only NONE was negotiated. Never wait or infer readiness from another
        // schema; quarantine occupies its reserved bounded capacity permanently.
        owner.quota.quarantined.fetch_add(1, Ordering::AcqRel);
        std::mem::forget(owner);
        return;
    }
    owner.quota.references.fetch_sub(1, Ordering::AcqRel);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::CpuPixels;
    fn pixels(bytes: Arc<Vec<u8>>) -> CpuSourcePresentation {
        CpuSourcePresentation {
            receipt: None,
            width: 2,
            height: 2,
            stride: 8,
            pixel_format: PIXEL_BGRA8,
            color: FrameColorMetadata::default(),
            bytes: bytes.into(),
            timestamp_us: 1,
            capture_pacing_timestamp_us: 1,
            sequence: 1,
            freshness: CAPTURE_FRAME_FRESH,
        }
    }
    #[test]
    fn retained_cpu_resource_shares_source_allocation_and_survives_initial_release() {
        let pool = CpuFrameLeasePool::default();
        let source = Arc::new(vec![7; 16]);
        let lease = pool.lease(pixels(source.clone()), 1, 1).unwrap();
        let frame = *lease.descriptor();
        let cpu = unsafe { &*frame.resource.data.cast::<CpuFrame>() };
        assert_eq!(cpu.data, source.as_ptr());
        assert_eq!(unsafe { (frame.retain)(frame.context) }, STATUS_OK);
        drop(lease);
        drop(source);
        assert_eq!(
            unsafe { std::slice::from_raw_parts(cpu.data, cpu.len) },
            [7; 16]
        );
        assert_eq!(pool.retirement_ready(), Ok(false));
        unsafe { (frame.release)(frame.context, &NONE) };
        assert_eq!(pool.retirement_ready(), Ok(true));
    }
    #[test]
    fn byte_and_reference_bounds_return_busy_and_recover_after_release() {
        let pool = CpuFrameLeasePool::with_limits(2, 16).unwrap();
        let lease = pool.lease(pixels(Arc::new(vec![0; 16])), 1, 1).unwrap();
        let frame = *lease.descriptor();
        assert!(matches!(
            pool.lease(pixels(Arc::new(vec![0; 16])), 1, 1),
            Err(MEDIA_BUSY)
        ));
        assert_eq!(unsafe { (frame.retain)(frame.context) }, STATUS_OK);
        assert_eq!(unsafe { (frame.retain)(frame.context) }, MEDIA_BUSY);
        drop(lease);
        assert_eq!(pool.retirement_ready(), Ok(false));
        unsafe { (frame.release)(frame.context, &NONE) };
        assert!(pool.lease(pixels(Arc::new(vec![0; 16])), 1, 1).is_ok());
    }
    #[test]
    fn unsupported_completion_quarantines_without_waiting_or_reusing_storage() {
        let pool = CpuFrameLeasePool::with_limits(2, 16).unwrap();
        let lease = pool.lease(pixels(Arc::new(vec![0; 16])), 1, 1).unwrap();
        let frame = *lease.descriptor();
        assert_eq!(unsafe { (frame.retain)(frame.context) }, STATUS_OK);
        let invalid = FrameDescriptor {
            kind: FRAME_SYNC_VENDOR_BASE,
            version: 1,
            namespace: [1; 16],
            ..NONE
        };
        unsafe { (frame.release)(frame.context, &invalid) };
        drop(lease);
        assert_eq!(pool.retirement_ready(), Err(MEDIA_BACKEND_ERROR));
        assert!(matches!(
            pool.lease(pixels(Arc::new(vec![0; 16])), 1, 1),
            Err(MEDIA_BUSY)
        ));
    }
    #[test]
    fn capture_frame_owner_can_be_shared_without_extracting_or_copying_pixels() {
        struct CaptureBuffer(Vec<u8>);
        impl AsRef<[u8]> for CaptureBuffer {
            fn as_ref(&self) -> &[u8] {
                &self.0
            }
        }
        let source = Arc::new(CaptureBuffer(vec![1; 16]));
        let data = source.0.as_ptr();
        let shared = CpuPixels::from_owner(source);
        assert_eq!(shared.as_ptr(), data);
        assert_eq!(shared.clone().as_ptr(), data);
    }
}
