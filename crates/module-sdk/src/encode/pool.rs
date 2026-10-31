//! Thread-affine consumer for an encoder-owned writable input pool.
//!
//! Unresolved work deliberately leaks the caller's lifetime guard on drop. This
//! retains only what the guard actually owns; it cannot pin independently owned
//! runtime, provider code, callback contexts or resources. In particular, the
//! existing host `NativeBinding` alone is not a sufficient guard. The caller
//! must provide a host-supported mechanism retaining every dependency until
//! process exit if completion is uncertain. Process exit and leaking do not prove
//! external GPU, DMA or shared-resource access has stopped. Actual completion
//! or a platform/driver termination guarantee is required for that conclusion.
//! Once executable work and callbacks are independently known to have stopped,
//! a separate resource-only quarantine may be possible; this adapter does not
//! establish that condition or implement such a quarantine.
use amaterasu_abi::{STATUS_INVALID_ARGUMENT, STATUS_OK, Status};
use amaterasu_media::{
    CaptureResourceOfferSink, ENCODER_INPUT_POOL_QUARANTINED, ENCODER_INPUT_POOL_RETIRED,
    EncoderInputPoolApiV1, EncoderInputPoolConfig, EncoderInputPoolRetirement,
    EncoderInputSubmission, EncoderInputTarget, FrameDescriptor, MEDIA_PENDING,
    encoder_input_pool_available, encoder_input_pool_config_valid,
};
use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    mem::{ManuallyDrop, MaybeUninit},
    rc::Rc,
};

struct Inner<G> {
    table: *const EncoderInputPoolApiV1,
    guard: ManuallyDrop<G>,
    stream: u64,
    current: Option<u64>,
    generations: HashSet<u64>,
    retired: HashSet<u64>,
    active: HashMap<u64, (u64, u64)>,
    seen_leases: HashSet<u64>,
    stopping: bool,
    poisoned: bool,
    safe_to_release: bool,
}

impl<G> Drop for Inner<G> {
    fn drop(&mut self) {
        if self.safe_to_release {
            // SAFETY: finish confirms all generations retired and no leases remain.
            unsafe { ManuallyDrop::drop(&mut self.guard) };
        }
        // Otherwise do not drop the guard. This only retains its actual owners;
        // its construction contract requires all dependent lifetimes be pinned.
    }
}

/// A pool binding and its lifetime guard. Neither this type nor its leases are
/// Send or Sync. Dropping before `finish` intentionally leaks the guard, but
/// this protects dependencies only if the unsafe construction contract is met.
/// The existing host `NativeBinding` alone cannot satisfy that contract.
pub struct InputPool<G> {
    inner: Rc<RefCell<Inner<G>>>,
}

/// An exclusive acquired writer lease. Drop does not invent a completion fence;
/// dropping an unresolved lease poisons the pool and causes its guard to leak.
#[must_use = "commit or abandon with an explicit writer completion descriptor"]
pub struct InputLease<G> {
    inner: Rc<RefCell<Inner<G>>>,
    target: Option<Box<EncoderInputTarget>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Retirement {
    Pending,
    Retired,
    Quarantined,
}

impl<G> InputPool<G> {
    /// Create an adapter for a distinct `vendor.encode-pool/1` binding.
    ///
    /// # Safety
    /// `table` must be the exact valid pool table and stay live in a stable
    /// address while `guard` lives. The guard must own and pin the pool and
    /// encoder bindings, provider code, source, device and storage resources;
    /// leaking it must keep all of them alive through process exit, including
    /// the host runtime and every callback context. Process exit alone does not
    /// establish external work has stopped; actual completion or a suitable
    /// platform/driver termination guarantee must establish that separately.
    /// A `NativeBinding` alone does
    /// not pin host-owned provider code or the runtime and is insufficient.
    /// The caller must establish actual host lifetime support before using this
    /// adapter with a real provider, including on every uncertain teardown path.
    /// The stream must be pristine: no preexisting configured generations,
    /// acquired leases, in-flight work or externally retained pool dependencies.
    /// It must belong to the same principal/session as the encoder. Provider ABI,
    /// callback validity and owner-thread rules must hold throughout. The caller
    /// must exclusively control this stream's pool state for the full lifetime.
    pub unsafe fn new(
        table: *const EncoderInputPoolApiV1,
        guard: G,
        stream: u64,
    ) -> Result<Self, Status> {
        if table.is_null() || stream == 0 {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        // SAFETY: caller supplies a live, aligned table with valid prefix.
        if !encoder_input_pool_available(unsafe { (*table).base.header }) {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        Ok(Self {
            inner: Rc::new(RefCell::new(Inner {
                table,
                guard: ManuallyDrop::new(guard),
                stream,
                current: None,
                generations: HashSet::new(),
                retired: HashSet::new(),
                active: HashMap::new(),
                seen_leases: HashSet::new(),
                stopping: false,
                poisoned: false,
                safe_to_release: false,
            })),
        })
    }

    /// Enumerate offers through a synchronous sink, returning its generation.
    ///
    /// # Safety
    /// The sink context and callback must be valid during this call, may not
    /// unwind or reenter this adapter, and must copy any data it retains.
    pub unsafe fn enumerate(&self, sink: CaptureResourceOfferSink) -> Result<u64, Status> {
        let inner = self.inner.borrow();
        if inner.stopping || inner.poisoned {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        let table = unsafe { &*inner.table };
        let mut generation = 0;
        // SAFETY: the caller guarantees the sink; construction guarantees table validity.
        let status =
            unsafe { (table.enumerate)(table.base.context, inner.stream, sink, &mut generation) };
        if status != STATUS_OK {
            return Err(status);
        }
        if generation == 0 {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        Ok(generation)
    }

    /// Configure a selected and independently negotiated offer. The provider
    /// checks actual schemas, devices and offer generation.
    pub fn configure(&self, config: &EncoderInputPoolConfig) -> Result<u64, Status> {
        let mut inner = self.inner.borrow_mut();
        if inner.stopping || inner.poisoned || !encoder_input_pool_config_valid(config) {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        let mut generation = 0;
        // SAFETY: construction establishes the live table, context and owning thread.
        let table = unsafe { &*inner.table };
        let status =
            unsafe { (table.configure)(table.base.context, inner.stream, config, &mut generation) };
        if status != STATUS_OK {
            return Err(status);
        }
        if generation == 0 || !inner.generations.insert(generation) {
            inner.poisoned = true;
            return Err(STATUS_INVALID_ARGUMENT);
        }
        inner.current = Some(generation);
        Ok(generation)
    }

    /// Acquire an exclusive lease for the current configured generation.
    pub fn acquire(&self) -> Result<InputLease<G>, Status> {
        let mut inner = self.inner.borrow_mut();
        if inner.stopping || inner.poisoned {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        let generation = inner.current.ok_or(STATUS_INVALID_ARGUMENT)?;
        let mut target = MaybeUninit::<EncoderInputTarget>::uninit();
        // SAFETY: a successful ABI call initializes the output; construction
        // establishes live table, context, and thread affinity.
        let table = unsafe { &*inner.table };
        let status = unsafe {
            (table.acquire)(
                table.base.context,
                inner.stream,
                generation,
                target.as_mut_ptr(),
            )
        };
        if status != STATUS_OK {
            return Err(status);
        }
        let target = unsafe { target.assume_init() };
        if target.generation != generation
            || target.target_id == 0
            || target.lease_id == 0
            || inner.seen_leases.contains(&target.lease_id)
            || inner
                .active
                .values()
                .any(|id| *id == (generation, target.target_id))
        {
            inner.poisoned = true;
            return Err(STATUS_INVALID_ARGUMENT);
        }
        inner.seen_leases.insert(target.lease_id);
        inner
            .active
            .insert(target.lease_id, (generation, target.target_id));
        drop(inner);
        Ok(InputLease {
            inner: Rc::clone(&self.inner),
            target: Some(Box::new(target)),
        })
    }

    /// Stop admission. Outstanding leases remain owned and must be abandoned.
    pub fn begin_retirement(&self) -> Result<(), Status> {
        let mut inner = self.inner.borrow_mut();
        if inner.stopping {
            return Ok(());
        }
        // SAFETY: construction establishes the live callback and owner thread.
        let table = unsafe { &*inner.table };
        let status = unsafe { (table.begin_retirement)(table.base.context, inner.stream) };
        if status != STATUS_OK {
            return Err(status);
        }
        inner.stopping = true;
        Ok(())
    }

    /// Query a known generation; pending and quarantine never release the guard.
    pub fn retirement(&self, generation: u64) -> Result<Retirement, Status> {
        let mut inner = self.inner.borrow_mut();
        if !inner.stopping || !inner.generations.contains(&generation) {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        // A new observation supersedes earlier certification. Contradictory
        // or failed later observations permanently poison this pool: a provider
        // that recants retirement cannot be trusted to prove its own safety.
        let previously_retired = inner.retired.remove(&generation);
        let mut result = MaybeUninit::<EncoderInputPoolRetirement>::uninit();
        // SAFETY: successful callback initializes result and table is live.
        let table = unsafe { &*inner.table };
        let status = unsafe {
            (table.retirement)(
                table.base.context,
                inner.stream,
                generation,
                result.as_mut_ptr(),
            )
        };
        if status == MEDIA_PENDING {
            inner.poisoned |= previously_retired;
            return Ok(Retirement::Pending);
        }
        if status != STATUS_OK {
            inner.poisoned |= previously_retired;
            return Err(status);
        }
        let result = unsafe { result.assume_init() };
        if result.version != 1
            || result.size < size_of::<EncoderInputPoolRetirement>() as u32
            || result.generation != generation
            || result.reserved != 0
        {
            inner.poisoned = true;
            return Err(STATUS_INVALID_ARGUMENT);
        }
        match result.state {
            ENCODER_INPUT_POOL_RETIRED if !inner.active.values().any(|id| id.0 == generation) => {
                inner.retired.insert(generation);
                Ok(Retirement::Retired)
            }
            ENCODER_INPUT_POOL_QUARANTINED => {
                inner.poisoned |= previously_retired;
                Ok(Retirement::Quarantined)
            }
            _ => {
                inner.poisoned = true;
                Err(STATUS_INVALID_ARGUMENT)
            }
        }
    }

    /// Release the guard only after every generation is confirmed retired.
    /// Failure returns the pool for further queries; dropping it leaks the guard.
    pub fn finish(self) -> Result<(), Self> {
        let ready = {
            let inner = self.inner.borrow();
            inner.stopping
                && !inner.poisoned
                && inner.active.is_empty()
                && inner.generations == inner.retired
        };
        if !ready {
            return Err(self);
        }
        self.inner.borrow_mut().safe_to_release = true;
        Ok(())
    }
}

impl<G> InputLease<G> {
    /// Descriptor data and handles remain borrowed only for this lease. Writers
    /// must wait for acquire and resource-specific fences before accessing it.
    pub fn target(&self) -> &EncoderInputTarget {
        self.target.as_deref().expect("live lease")
    }

    /// Commit this lease; failure returns it for retry or abandon.
    ///
    /// # Safety
    /// Submission and completion descriptor obey the negotiated schemas, cover
    /// every writer access, and all pointed-to data and handles are valid for the
    /// call. NONE may be supplied only when all accesses actually completed.
    /// Frame-option identity/timing must match the prepared transaction.
    pub unsafe fn commit(
        mut self,
        submission: &EncoderInputSubmission,
    ) -> Result<(), (Status, Self)> {
        let target = self.target();
        if submission.version != 1
            || submission.size < size_of::<EncoderInputSubmission>() as u32
            || (submission.control_id != 0 && submission.control_id != submission.sequence)
            || submission.generation != target.generation
            || submission.target_id != target.target_id
            || submission.lease_id != target.lease_id
        {
            return Err((STATUS_INVALID_ARGUMENT, self));
        }
        let status = {
            let inner = self.inner.borrow();
            if inner.stopping || inner.poisoned || inner.current != Some(target.generation) {
                STATUS_INVALID_ARGUMENT
            } else {
                // SAFETY: caller guarantees submission and completion validity.
                let table = unsafe { &*inner.table };
                unsafe { (table.commit)(table.base.context, inner.stream, submission) }
            }
        };
        if status != STATUS_OK {
            return Err((status, self));
        }
        self.consume();
        Ok(())
    }

    /// Abandon this lease with an explicit writer completion descriptor.
    ///
    /// # Safety
    /// The descriptor follows the negotiated schema and covers every writer
    /// access; NONE is valid only if all accesses have actually completed. Its
    /// data and handles remain valid throughout the callback.
    pub unsafe fn abandon(mut self, completion: &FrameDescriptor) -> Result<(), (Status, Self)> {
        let target = self.target();
        let status = {
            let inner = self.inner.borrow();
            // SAFETY: caller guarantees completion validity and target is owned.
            let table = unsafe { &*inner.table };
            unsafe {
                (table.abandon)(
                    table.base.context,
                    inner.stream,
                    target.generation,
                    target.target_id,
                    target.lease_id,
                    completion,
                )
            }
        };
        if status != STATUS_OK {
            return Err((status, self));
        }
        self.consume();
        Ok(())
    }

    fn consume(&mut self) {
        let target = self.target.take().expect("live lease");
        self.inner.borrow_mut().active.remove(&target.lease_id);
    }
}

impl<G> Drop for InputLease<G> {
    fn drop(&mut self) {
        if self.target.is_some() {
            self.inner.borrow_mut().poisoned = true;
        }
    }
}
