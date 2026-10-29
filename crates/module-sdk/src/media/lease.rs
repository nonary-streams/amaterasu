//! Retained GPU presentations. Raw frame descriptors remain borrowed views.
use amaterasu_abi::{STATUS_OK, Status};
use amaterasu_media::{DmaBufFrame, DmaBufFrameLease};
use std::{marker::PhantomData, rc::Rc};

/// One retained provider reference keeping a DMA-BUF presentation immutable.
///
/// This value releases one reference on drop. It deliberately is not `Send` or
/// `Sync`: media lease callbacks execute on the owning module thread. The unsafe
/// constructor requires the provider and native binding to outlive this value.
/// No descriptor is duplicated: all plane and acquire-fence fds remain borrowed
/// from the provider, which keeps them valid until release.
#[must_use = "dropping a GPU frame immediately releases its retained reference"]
pub struct RetainedDmaBufFrame<'provider> {
    lease: Option<DmaBufFrameLease>,
    module_thread: PhantomData<Rc<()>>,
    provider: PhantomData<&'provider crate::NativeBinding>,
}

impl<'provider> RetainedDmaBufFrame<'provider> {
    /// Retains a borrowed presentation, or propagates the provider's error.
    ///
    /// # Safety
    /// The descriptor, callbacks, and provider must obey the GPU lease ABI. In
    /// particular, successful retain pins immutable content and live descriptors
    /// until release, including during stream shutdown. The provider and its
    /// native binding must identify this lease provider; borrowing it prevents
    /// it from being released before this value and its clones. Call this on
    /// the provider's owning module thread, while the input borrow is still live.
    pub unsafe fn retain_from_borrowed(
        lease: &DmaBufFrameLease,
        _provider: &'provider crate::NativeBinding,
    ) -> Result<Self, Status> {
        // SAFETY: caller establishes that this binding owns the supplied lease.
        unsafe { Self::retain_reference(lease) }
    }

    // The public constructor establishes the provider lifetime; clones inherit it.
    unsafe fn retain_reference(lease: &DmaBufFrameLease) -> Result<Self, Status> {
        lease.validate()?;
        // SAFETY: the caller supplies a valid provider callback and live borrow.
        let status = unsafe { lease.retain.expect("validated retain callback")(lease.context) };
        if status != STATUS_OK {
            return Err(status);
        }
        Ok(Self {
            lease: Some(*lease),
            module_thread: PhantomData,
            provider: PhantomData,
        })
    }

    /// Acquires a separate retained reference subject to the provider's bound.
    /// A rejected clone leaves this reference unchanged.
    pub fn try_clone(&self) -> Result<Self, Status> {
        // SAFETY: construction guarantees the provider/binding outlives all
        // clones, and this retained reference keeps the borrowed lease live.
        unsafe { Self::retain_reference(self.descriptor()) }
    }

    /// Borrows the immutable presentation's descriptors. Waiting on each
    /// acquire fence remains required before accessing the pixels.
    pub fn frame(&self) -> &DmaBufFrame {
        &self.descriptor().frame
    }

    /// Borrows the lease for another synchronous native stage. That stage must
    /// explicitly retain it if it keeps the frame beyond its call.
    pub fn descriptor(&self) -> &DmaBufFrameLease {
        self.lease.as_ref().expect("live retained frame")
    }

    /// Releases after a GPU completion fence, transferring ownership of the fd.
    ///
    /// # Safety
    /// `completion` must be a valid sync_file covering every pending GPU access
    /// made through this reference. No descriptor or pixel view from this
    /// reference may be accessed afterward. The provider remains alive until it
    /// has processed the fence and retired the storage. Ordinary drop is valid
    /// only when GPU access is already complete.
    #[cfg(target_os = "linux")]
    pub unsafe fn release_after(mut self, completion: std::os::fd::OwnedFd) {
        use std::os::fd::IntoRawFd;
        let lease = self.lease.take().expect("live retained frame");
        // SAFETY: caller guarantees the completion fence, and this value owns
        // exactly one reference plus the transferred fd.
        unsafe {
            lease.release.expect("validated release callback")(
                lease.context,
                completion.into_raw_fd(),
            )
        };
    }
}

impl Drop for RetainedDmaBufFrame<'_> {
    fn drop(&mut self) {
        if let Some(lease) = self.lease.take() {
            // SAFETY: construction retained exactly one reference and ensures
            // the provider stays live. No asynchronous GPU operation is started
            // by the safe SDK, so ordinary drop signals completed access.
            unsafe { lease.release.expect("validated release callback")(lease.context, -1) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use amaterasu_abi::STATUS_INVALID_ARGUMENT;
    use amaterasu_media::{DMABUF_COLOR_SRGB, DMABUF_MAX_PLANES, DmaBufPlane, MEDIA_BUSY};
    use std::cell::Cell;

    struct Owner {
        references: Cell<u32>,
        released: Cell<u32>,
        limit: u32,
    }

    unsafe extern "C" fn retain(context: *mut std::ffi::c_void) -> Status {
        // SAFETY: each fixture provides an Owner that outlives every reference.
        let owner = unsafe { &*context.cast::<Owner>() };
        if owner.references.get() >= owner.limit {
            return MEDIA_BUSY;
        }
        owner.references.set(owner.references.get() + 1);
        STATUS_OK
    }

    unsafe extern "C" fn release(context: *mut std::ffi::c_void, fence: i32) {
        // SAFETY: each fixture provides an Owner that outlives every reference.
        let owner = unsafe { &*context.cast::<Owner>() };
        assert_eq!(fence, -1);
        owner.references.set(owner.references.get() - 1);
        owner.released.set(owner.released.get() + 1);
    }

    fn lease(owner: &Owner) -> DmaBufFrameLease {
        let mut planes = [DmaBufPlane::UNUSED; DMABUF_MAX_PLANES];
        planes[0] = DmaBufPlane {
            fd: 7,
            acquire_fence_fd: -1,
            offset: 0,
            pitch: 256,
        };
        DmaBufFrameLease {
            frame: DmaBufFrame {
                width: 64,
                height: 64,
                fourcc: u32::from_le_bytes(*b"XR24"),
                plane_count: 1,
                modifier: 0,
                planes,
                color: DMABUF_COLOR_SRGB,
                reserved: 0,
                timestamp_us: 15,
                sequence: 9,
            },
            context: std::ptr::from_ref(owner).cast_mut().cast(),
            retain: Some(retain),
            release: Some(release),
        }
    }

    #[test]
    fn retained_reference_outlives_borrow_and_releases_exactly_once() {
        let owner = Owner {
            references: Cell::new(1),
            released: Cell::new(0),
            limit: 3,
        };
        let retained = {
            let borrowed = lease(&owner);
            // SAFETY: owner remains live and fixture implements the lease contract.
            unsafe { RetainedDmaBufFrame::retain_reference(&borrowed) }.unwrap()
        };
        assert_eq!(retained.frame().sequence, 9);
        let clone = retained.try_clone().unwrap();
        assert!(matches!(retained.try_clone(), Err(MEDIA_BUSY)));
        assert_eq!(owner.references.get(), 3);
        drop(retained);
        assert_eq!(clone.frame().timestamp_us, 15);
        assert_eq!(owner.references.get(), 2);
        drop(clone);
        assert_eq!(owner.references.get(), 1);
        assert_eq!(owner.released.get(), 2);
    }

    #[test]
    fn rejected_or_malformed_borrow_never_releases_an_unowned_reference() {
        let owner = Owner {
            references: Cell::new(1),
            released: Cell::new(0),
            limit: 1,
        };
        let mut borrowed = lease(&owner);
        // SAFETY: fixture remains live, and correctly refuses an extra reference.
        assert!(matches!(
            unsafe { RetainedDmaBufFrame::retain_reference(&borrowed) },
            Err(MEDIA_BUSY)
        ));
        borrowed.release = None;
        // SAFETY: metadata validation refuses the missing callback before invocation.
        assert!(matches!(
            unsafe { RetainedDmaBufFrame::retain_reference(&borrowed) },
            Err(STATUS_INVALID_ARGUMENT)
        ));
        assert_eq!(owner.references.get(), 1);
        assert_eq!(owner.released.get(), 0);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn completion_fence_ownership_is_transferred_without_second_drop_release() {
        use std::{
            cell::RefCell,
            os::fd::{FromRawFd, OwnedFd},
            os::unix::net::UnixStream,
        };
        struct FencedOwner {
            releases: Cell<u32>,
            fence: RefCell<Option<OwnedFd>>,
        }
        unsafe extern "C" fn retain_fenced(_: *mut std::ffi::c_void) -> Status {
            STATUS_OK
        }
        unsafe extern "C" fn release_fenced(context: *mut std::ffi::c_void, fence: i32) {
            // SAFETY: the fixture context is live and the SDK transfers a live fd.
            let owner = unsafe { &*context.cast::<FencedOwner>() };
            owner.releases.set(owner.releases.get() + 1);
            assert!(fence >= 0);
            // SAFETY: ownership of this descriptor transferred with the callback.
            *owner.fence.borrow_mut() = Some(unsafe { OwnedFd::from_raw_fd(fence) });
        }
        let owner = FencedOwner {
            releases: Cell::new(0),
            fence: RefCell::new(None),
        };
        let metadata = Owner {
            references: Cell::new(1),
            released: Cell::new(0),
            limit: 2,
        };
        let mut borrowed = lease(&metadata);
        borrowed.context = std::ptr::from_ref(&owner).cast_mut().cast();
        borrowed.retain = Some(retain_fenced);
        borrowed.release = Some(release_fenced);
        // SAFETY: the fenced fixture remains live and accepts one reference.
        let retained = unsafe { RetainedDmaBufFrame::retain_reference(&borrowed) }.unwrap();
        let (fd, _peer) = UnixStream::pair().unwrap();
        // SAFETY: this fixture only checks descriptor transfer, never GPU access.
        // A production provider must validate sync_file type and quarantine failures.
        unsafe { retained.release_after(fd.into()) };
        assert_eq!(owner.releases.get(), 1);
        assert!(owner.fence.borrow().is_some());
    }
}
