//! Process-local ownership for the four exclusive DS5 gadget UAPI slots.

use std::io;
#[cfg(not(test))]
use std::sync::OnceLock;
use std::sync::{Arc, Mutex, MutexGuard};

const UAPI_SLOT_COUNT: usize = 4;

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct CreateRequest {
    pub(super) slot: u32,
    pub(super) mac: [u8; 6],
    pub(super) reserved: [u8; 2],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct SlotOwner {
    pub(super) client_slot: u8,
    generation: u64,
}

#[derive(Default)]
struct SlotRegistryState {
    owners: [Option<SlotOwner>; UAPI_SLOT_COUNT],
    next_generation: u64,
}

#[derive(Clone, Default)]
struct SlotRegistry(Arc<Mutex<SlotRegistryState>>);

impl SlotRegistry {
    fn reserve_next(
        &self,
        client_slot: u8,
        excluded: &[bool; UAPI_SLOT_COUNT],
    ) -> Option<SlotReservation> {
        let mut state = self.lock();
        let index = (0..UAPI_SLOT_COUNT)
            .find(|index| !excluded[*index] && state.owners[*index].is_none())?;
        let generation = state.next_generation.checked_add(1)?;
        state.next_generation = generation;
        let owner = SlotOwner {
            client_slot,
            generation,
        };
        state.owners[index] = Some(owner);
        Some(SlotReservation {
            registry: self.clone(),
            uapi_slot: u8::try_from(index).expect("the DS5 UAPI exposes four slots"),
            owner,
        })
    }

    #[cfg(test)]
    fn owner(&self, uapi_slot: u8) -> Option<SlotOwner> {
        self.lock()
            .owners
            .get(usize::from(uapi_slot))
            .copied()
            .flatten()
    }

    fn lock(&self) -> MutexGuard<'_, SlotRegistryState> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[cfg(not(test))]
static PROCESS_SLOT_REGISTRY: OnceLock<SlotRegistry> = OnceLock::new();

#[cfg(not(test))]
fn process_slot_registry() -> &'static SlotRegistry {
    PROCESS_SLOT_REGISTRY.get_or_init(SlotRegistry::default)
}

struct SlotReservation {
    registry: SlotRegistry,
    uapi_slot: u8,
    owner: SlotOwner,
}

impl Drop for SlotReservation {
    fn drop(&mut self) {
        let mut state = self.registry.lock();
        let index = usize::from(self.uapi_slot);
        if state.owners[index] == Some(self.owner) {
            state.owners[index] = None;
        }
    }
}

pub(super) trait GadgetIo {
    fn create_slot(&mut self, request: &CreateRequest) -> io::Result<()>;
    fn destroy_slot(&mut self) -> io::Result<()>;
    fn close(&mut self);
}

/// Owns one opened UAPI FD and its process-local reservation as one lifetime.
/// The FD is closed before the reservation is released, so another local
/// creator cannot race kernel file-release cleanup and accidentally share it.
pub(super) struct OwnedGadget<H: GadgetIo> {
    handle: Option<H>,
    reservation: Option<SlotReservation>,
    cleanup_on_drop: bool,
    created: bool,
    destroyed: bool,
}

impl<H: GadgetIo> OwnedGadget<H> {
    #[cfg(not(test))]
    pub(super) fn acquire_process(
        client_slot: u8,
        mac: [u8; 6],
        open: impl FnMut(SlotOwner) -> io::Result<H>,
    ) -> io::Result<Self> {
        Self::acquire_with_registry(process_slot_registry(), client_slot, mac, open)
    }

    fn acquire_with_registry(
        registry: &SlotRegistry,
        client_slot: u8,
        mac: [u8; 6],
        mut open: impl FnMut(SlotOwner) -> io::Result<H>,
    ) -> io::Result<Self> {
        let mut excluded = [false; UAPI_SLOT_COUNT];
        let mut last_busy = None;

        for _ in 0..UAPI_SLOT_COUNT {
            let Some(reservation) = registry.reserve_next(client_slot, &excluded) else {
                return Err(last_busy.unwrap_or_else(no_slot_available));
            };
            let uapi_slot = reservation.uapi_slot;
            let expected_owner = reservation.owner;
            let handle = open(expected_owner)?;
            let mut candidate = Self {
                handle: Some(handle),
                reservation: Some(reservation),
                cleanup_on_drop: true,
                created: false,
                destroyed: false,
            };
            let request = CreateRequest {
                slot: u32::from(uapi_slot),
                mac,
                reserved: [0; 2],
            };

            match candidate.handle_mut().create_slot(&request) {
                Ok(()) => {
                    candidate.created = true;
                    return Ok(candidate);
                }
                Err(error) if error.raw_os_error() == Some(libc::EBUSY) => {
                    excluded[usize::from(uapi_slot)] = true;
                    last_busy = Some(error);
                    // EBUSY means this FD did not acquire the external slot.
                    // Do not issue DESTROY: close this FD, then release only
                    // our matching process reservation before trying another.
                    candidate.cleanup_on_drop = false;
                    candidate.close_then_release();
                }
                Err(error) => return Err(error),
            }
        }

        Err(last_busy.unwrap_or_else(no_slot_available))
    }

    pub(super) fn client_slot(&self) -> u8 {
        self.reservation
            .as_ref()
            .expect("live gadget retains its slot reservation")
            .owner
            .client_slot
    }

    pub(super) fn uapi_slot(&self) -> u8 {
        self.reservation
            .as_ref()
            .expect("live gadget retains its slot reservation")
            .uapi_slot
    }

    #[cfg(not(test))]
    pub(super) fn handle(&self) -> &H {
        self.handle
            .as_ref()
            .expect("live gadget retains its opened UAPI handle")
    }

    fn handle_mut(&mut self) -> &mut H {
        self.handle
            .as_mut()
            .expect("live gadget retains its opened UAPI handle")
    }

    pub(super) fn destroy(&mut self) -> io::Result<()> {
        if self.destroyed {
            return Ok(());
        }
        if self.created {
            self.handle_mut().destroy_slot()?;
        }
        self.destroyed = true;
        Ok(())
    }

    pub(super) fn cleanup(&mut self) {
        if self.cleanup_on_drop
            && self.created
            && !self.destroyed
            && let Some(handle) = self.handle.as_mut()
        {
            let _ = handle.destroy_slot();
        }
        self.close_then_release();
    }

    fn close_then_release(&mut self) {
        self.cleanup_on_drop = false;
        self.created = false;
        self.destroyed = true;
        if let Some(mut handle) = self.handle.take() {
            handle.close();
            drop(handle);
        }
        self.reservation.take();
    }
}

impl<H: GadgetIo> Drop for OwnedGadget<H> {
    fn drop(&mut self) {
        self.cleanup();
    }
}

fn no_slot_available() -> io::Error {
    io::Error::from_raw_os_error(libc::EBUSY)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::Barrier;
    use std::thread;

    #[derive(Default)]
    struct FakeKernel {
        active: [Option<u64>; UAPI_SLOT_COUNT],
        create_attempts: Vec<(u64, u8, [u8; 6])>,
        destroy_attempts: Vec<u64>,
        close_attempts: Vec<u64>,
        close_saw_exact_reservation: Vec<bool>,
        lifecycle: Vec<String>,
        create_error: Option<(u8, i32)>,
        fail_destroy_count: usize,
        next_handle: u64,
    }

    struct FakeHandle {
        id: u64,
        registry: SlotRegistry,
        expected_owner: SlotOwner,
        kernel: Rc<RefCell<FakeKernel>>,
        attempted_slot: Option<u8>,
        owned_slot: Option<u8>,
        closed: bool,
    }

    impl FakeHandle {
        fn open(
            registry: SlotRegistry,
            expected_owner: SlotOwner,
            kernel: Rc<RefCell<FakeKernel>>,
        ) -> Self {
            let id = {
                let mut state = kernel.borrow_mut();
                state.next_handle += 1;
                state.next_handle
            };
            Self {
                id,
                registry,
                expected_owner,
                kernel,
                attempted_slot: None,
                owned_slot: None,
                closed: false,
            }
        }

        fn close_once(&mut self) {
            if self.closed {
                return;
            }
            if let Some(slot) = self.attempted_slot {
                let matches = self.registry.owner(slot) == Some(self.expected_owner);
                self.kernel
                    .borrow_mut()
                    .close_saw_exact_reservation
                    .push(matches);
            }
            {
                let mut state = self.kernel.borrow_mut();
                state.close_attempts.push(self.id);
                state.lifecycle.push(format!("close:{}", self.id));
                if let Some(slot) = self.owned_slot.take()
                    && state.active[usize::from(slot)] == Some(self.id)
                {
                    // Closing the file models ds5_release() clearing only the
                    // gadget attached to this exact FD.
                    state.active[usize::from(slot)] = None;
                }
            }
            self.closed = true;
        }
    }

    impl GadgetIo for FakeHandle {
        fn create_slot(&mut self, request: &CreateRequest) -> io::Result<()> {
            let slot = u8::try_from(request.slot).expect("test requests use bounded UAPI slots");
            self.attempted_slot = Some(slot);
            let mut state = self.kernel.borrow_mut();
            state.create_attempts.push((self.id, slot, request.mac));
            if state.active[usize::from(slot)].is_some() {
                return Err(no_slot_available());
            }
            if let Some((failing_slot, error)) = state.create_error
                && failing_slot == slot
            {
                return Err(io::Error::from_raw_os_error(error));
            }
            state.active[usize::from(slot)] = Some(self.id);
            self.owned_slot = Some(slot);
            Ok(())
        }

        fn destroy_slot(&mut self) -> io::Result<()> {
            let mut state = self.kernel.borrow_mut();
            state.destroy_attempts.push(self.id);
            state.lifecycle.push(format!("destroy:{}", self.id));
            if state.fail_destroy_count != 0 {
                state.fail_destroy_count -= 1;
                return Err(io::Error::from_raw_os_error(libc::EIO));
            }
            let Some(slot) = self.owned_slot.take() else {
                return Err(io::Error::from_raw_os_error(libc::ENODEV));
            };
            if state.active[usize::from(slot)] == Some(self.id) {
                state.active[usize::from(slot)] = None;
            }
            Ok(())
        }

        fn close(&mut self) {
            self.close_once();
        }
    }

    impl Drop for FakeHandle {
        fn drop(&mut self) {
            self.close_once();
        }
    }

    fn acquire_fake(
        registry: &SlotRegistry,
        client_slot: u8,
        mac: [u8; 6],
        kernel: Rc<RefCell<FakeKernel>>,
    ) -> io::Result<OwnedGadget<FakeHandle>> {
        let registry_for_open = registry.clone();
        OwnedGadget::acquire_with_registry(registry, client_slot, mac, move |owner| {
            Ok(FakeHandle::open(
                registry_for_open.clone(),
                owner,
                Rc::clone(&kernel),
            ))
        })
    }

    #[test]
    fn reservations_are_atomic_across_concurrent_client_slots() {
        let registry = SlotRegistry::default();
        let barrier = Arc::new(Barrier::new(8));
        let reservations = thread::scope(|scope| {
            let workers = (0..8)
                .map(|client_slot| {
                    let registry = registry.clone();
                    let barrier = Arc::clone(&barrier);
                    scope.spawn(move || {
                        barrier.wait();
                        registry.reserve_next(client_slot, &[false; UAPI_SLOT_COUNT])
                    })
                })
                .collect::<Vec<_>>();
            workers
                .into_iter()
                .map(|worker| worker.join().unwrap())
                .collect::<Vec<_>>()
        });
        let mut allocated = reservations
            .iter()
            .flatten()
            .map(|reservation| reservation.uapi_slot)
            .collect::<Vec<_>>();
        allocated.sort_unstable();
        assert_eq!(allocated, [0, 1, 2, 3]);
        assert_eq!(registry.lock().owners.iter().flatten().count(), 4);
        drop(reservations);
        assert!(registry.lock().owners.iter().all(Option::is_none));
    }

    #[test]
    fn full_process_registry_rejects_acquisition_without_opening_an_fd() {
        let registry = SlotRegistry::default();
        let reservations = (0..UAPI_SLOT_COUNT)
            .map(|client_slot| {
                registry
                    .reserve_next(
                        u8::try_from(client_slot).expect("test client slot fits u8"),
                        &[false; UAPI_SLOT_COUNT],
                    )
                    .unwrap()
            })
            .collect::<Vec<_>>();
        let opened = Rc::new(RefCell::new(Vec::new()));
        let opened_in_factory = Rc::clone(&opened);

        let error = OwnedGadget::<FakeHandle>::acquire_with_registry(
            &registry,
            12,
            [2, 0, 0, 0, 0, 12],
            move |owner| {
                opened_in_factory.borrow_mut().push(owner);
                Err(io::Error::from_raw_os_error(libc::EMFILE))
            },
        )
        .err()
        .unwrap();

        assert_eq!(error.raw_os_error(), Some(libc::EBUSY));
        assert!(opened.borrow().is_empty());
        assert_eq!(registry.lock().owners.iter().flatten().count(), 4);
        drop(reservations);
        assert!(registry.lock().owners.iter().all(Option::is_none));
    }

    #[test]
    fn opening_failure_releases_its_unassociated_reservation() {
        let registry = SlotRegistry::default();
        let error = OwnedGadget::<FakeHandle>::acquire_with_registry(
            &registry,
            6,
            [2, 0, 0, 0, 0, 6],
            |_owner| Err(io::Error::from_raw_os_error(libc::EMFILE)),
        )
        .err()
        .unwrap();

        assert_eq!(error.raw_os_error(), Some(libc::EMFILE));
        assert!(registry.lock().owners.iter().all(Option::is_none));
    }

    #[test]
    fn congruent_client_slots_reserve_distinct_uapi_slots_and_keep_feedback_owners() {
        let registry = SlotRegistry::default();
        let kernel = Rc::new(RefCell::new(FakeKernel::default()));
        let address = [0x02, 1, 2, 3, 4, 1];
        let first = acquire_fake(&registry, 1, address, Rc::clone(&kernel)).unwrap();
        let second = acquire_fake(&registry, 5, address, Rc::clone(&kernel)).unwrap();

        assert_eq!(first.uapi_slot(), 0);
        assert_eq!(second.uapi_slot(), 1);
        assert_eq!(first.client_slot(), 1);
        assert_eq!(second.client_slot(), 5);
        assert_eq!(registry.owner(0).unwrap().client_slot, 1);
        assert_eq!(registry.owner(1).unwrap().client_slot, 5);
        assert_eq!(
            kernel
                .borrow()
                .create_attempts
                .iter()
                .map(|(_, slot, mac)| (*slot, *mac))
                .collect::<Vec<_>>(),
            [(0, address), (1, address)]
        );
    }

    #[test]
    fn clients_above_three_with_colliding_randomized_macs_get_distinct_uapi_slots() {
        let registry = SlotRegistry::default();
        let kernel = Rc::new(RefCell::new(FakeKernel::default()));
        let randomized_address = [0x02, 0x75, 0xa4, 0x33, 0x1e, 0xc0];
        let first = acquire_fake(&registry, 9, randomized_address, Rc::clone(&kernel)).unwrap();
        let second = acquire_fake(&registry, 13, randomized_address, Rc::clone(&kernel)).unwrap();

        assert!(first.client_slot() > 3);
        assert!(second.client_slot() > 3);
        assert_ne!(first.uapi_slot(), second.uapi_slot());
        assert_eq!(first.client_slot(), 9);
        assert_eq!(second.client_slot(), 13);
        assert_eq!(kernel.borrow().create_attempts[0].2, randomized_address);
        assert_eq!(kernel.borrow().create_attempts[1].2, randomized_address);
        assert_eq!(kernel.borrow().create_attempts[0].1, 0);
        assert_eq!(kernel.borrow().create_attempts[1].1, 1);
    }

    #[test]
    fn external_busy_slots_are_skipped_without_destroying_external_devices() {
        let registry = SlotRegistry::default();
        let kernel = Rc::new(RefCell::new(FakeKernel::default()));
        kernel.borrow_mut().active[0] = Some(0);
        kernel.borrow_mut().active[2] = Some(0);
        let device = acquire_fake(&registry, 7, [2, 0, 0, 0, 0, 2], Rc::clone(&kernel)).unwrap();

        assert_eq!(device.uapi_slot(), 1);
        assert_eq!(device.client_slot(), 7);
        assert_eq!(kernel.borrow().active[0], Some(0));
        assert_eq!(kernel.borrow().active[2], Some(0));
        assert!(kernel.borrow().destroy_attempts.is_empty());
        assert_eq!(
            kernel
                .borrow()
                .create_attempts
                .iter()
                .map(|(_, slot, _)| *slot)
                .collect::<Vec<_>>(),
            [0, 1]
        );
        assert_eq!(kernel.borrow().close_saw_exact_reservation, [true]);
    }

    #[test]
    fn all_external_busy_slots_close_each_fd_and_leave_no_local_reservation() {
        let registry = SlotRegistry::default();
        let kernel = Rc::new(RefCell::new(FakeKernel::default()));
        kernel.borrow_mut().active = [Some(0); UAPI_SLOT_COUNT];

        let error = acquire_fake(&registry, 12, [2, 0, 0, 0, 0, 7], Rc::clone(&kernel))
            .err()
            .unwrap();

        assert_eq!(error.raw_os_error(), Some(libc::EBUSY));
        assert_eq!(registry.lock().owners.iter().flatten().count(), 0);
        assert_eq!(kernel.borrow().active, [Some(0); UAPI_SLOT_COUNT]);
        assert!(kernel.borrow().destroy_attempts.is_empty());
        assert_eq!(kernel.borrow().close_attempts.len(), UAPI_SLOT_COUNT);
        assert_eq!(
            kernel.borrow().close_saw_exact_reservation,
            [true, true, true, true]
        );
    }

    #[test]
    fn successful_destroy_keeps_reservation_until_close_then_allows_recreation() {
        let registry = SlotRegistry::default();
        let kernel = Rc::new(RefCell::new(FakeKernel::default()));
        let mut first = acquire_fake(&registry, 3, [2, 0, 0, 0, 0, 3], Rc::clone(&kernel)).unwrap();
        let first_owner = registry.owner(first.uapi_slot()).unwrap();
        assert!(first.destroy().is_ok());
        assert_eq!(registry.owner(first.uapi_slot()), Some(first_owner));

        let second = acquire_fake(&registry, 4, [2, 0, 0, 0, 0, 4], Rc::clone(&kernel)).unwrap();
        assert_eq!(second.uapi_slot(), 1);
        drop(first);
        assert_eq!(registry.owner(0), None);
        drop(second);

        let recreated = acquire_fake(&registry, 3, [2, 0, 0, 0, 0, 3], Rc::clone(&kernel)).unwrap();
        assert_eq!(recreated.uapi_slot(), 0);
        assert_ne!(
            registry.owner(0).unwrap().generation,
            first_owner.generation
        );
        assert!(
            kernel
                .borrow()
                .close_saw_exact_reservation
                .iter()
                .all(|held| *held)
        );
    }

    #[test]
    fn create_and_destroy_failures_close_before_releasing_owned_reservations() {
        let registry = SlotRegistry::default();
        let kernel = Rc::new(RefCell::new(FakeKernel::default()));
        kernel.borrow_mut().create_error = Some((0, libc::EACCES));
        let error = acquire_fake(&registry, 2, [2, 0, 0, 0, 0, 2], Rc::clone(&kernel))
            .err()
            .unwrap();
        assert_eq!(error.raw_os_error(), Some(libc::EACCES));
        assert_eq!(registry.owner(0), None);
        assert_eq!(kernel.borrow().close_saw_exact_reservation, [true]);
        assert_eq!(kernel.borrow().close_attempts.len(), 1);
        assert!(kernel.borrow().destroy_attempts.is_empty());
        assert_eq!(kernel.borrow().lifecycle, ["close:1"]);

        kernel.borrow_mut().create_error = None;
        kernel.borrow_mut().fail_destroy_count = 2;
        let mut owned = acquire_fake(&registry, 2, [2, 0, 0, 0, 0, 2], Rc::clone(&kernel)).unwrap();
        assert!(owned.destroy().is_err());
        let owned_slot = owned.uapi_slot();
        assert!(registry.owner(owned_slot).is_some());
        drop(owned);
        assert_eq!(registry.owner(owned_slot), None);
        assert_eq!(kernel.borrow().active[usize::from(owned_slot)], None);
        assert!(
            kernel
                .borrow()
                .close_saw_exact_reservation
                .iter()
                .all(|held| *held)
        );
        let lifecycle = kernel.borrow().lifecycle.clone();
        assert!(lifecycle.ends_with(&["destroy:2".to_owned(), "close:2".to_owned()]));
    }
}
