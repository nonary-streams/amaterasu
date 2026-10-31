use amaterasu_module_sdk::{
    abi::{AbiHeader, NativeInterface, STATUS_INVALID_ARGUMENT, STATUS_OK},
    encode::pool::{InputPool, Retirement},
    media_abi::*,
};
use std::{
    cell::{Cell, UnsafeCell},
    ffi::c_void,
    mem::size_of,
    ptr,
    rc::Rc,
};

fn none() -> FrameDescriptor {
    FrameDescriptor {
        kind: FRAME_SYNC_NONE,
        version: 0,
        size: 0,
        reserved: 0,
        namespace: [0; 16],
        data: ptr::null(),
    }
}
#[derive(Clone, Copy, PartialEq)]
enum Slot {
    Free,
    Leased(u64),
    Pending,
}
struct State {
    generation: u64,
    next: u64,
    slots: [Slot; 2],
    pixels: [u8; 2],
    stopped: bool,
    reject: bool,
    quarantined: bool,
    retirement_failure: Option<i32>,
}
// Mock guard owns the table and callback context; it does not model a host runtime
// or dynamic library, which must be pinned separately in a real integration.
struct Fixture {
    state: Box<UnsafeCell<State>>,
    table: Box<EncoderInputPoolApiV1>,
    dropped: Rc<Cell<bool>>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.dropped.set(true);
    }
}
// Every callback receives a pointer to the stable State held by Fixture, and
// tests call them serially on the owner thread.
unsafe fn state<'a>(ctx: *mut c_void) -> &'a mut State {
    unsafe { &mut *ctx.cast::<State>() }
}
unsafe extern "C" fn enumerate(
    _: *mut c_void,
    _: u64,
    _: CaptureResourceOfferSink,
    _: *mut u64,
) -> i32 {
    MEDIA_UNSUPPORTED
}
unsafe extern "C" fn configure(
    ctx: *mut c_void,
    stream: u64,
    config: *const EncoderInputPoolConfig,
    out: *mut u64,
) -> i32 {
    let s = unsafe { state(ctx) };
    if stream != 1 || s.stopped || !encoder_input_pool_config_valid(unsafe { &*config }) {
        return STATUS_INVALID_ARGUMENT;
    }
    s.generation += 1;
    unsafe { *out = s.generation };
    STATUS_OK
}
unsafe extern "C" fn acquire(
    ctx: *mut c_void,
    stream: u64,
    generation: u64,
    out: *mut EncoderInputTarget,
) -> i32 {
    let s = unsafe { state(ctx) };
    if stream != 1 || s.stopped || generation != s.generation {
        return STATUS_INVALID_ARGUMENT;
    }
    let Some(i) = s.slots.iter().position(|slot| *slot == Slot::Free) else {
        return MEDIA_BUSY;
    };
    s.next += 1;
    s.slots[i] = Slot::Leased(s.next);
    let resource = FrameDescriptor {
        kind: FRAME_RESOURCE_CPU,
        version: 1,
        size: 1,
        reserved: 0,
        namespace: [0; 16],
        data: (&raw const s.pixels[i]).cast(),
    };
    unsafe {
        *out = EncoderInputTarget {
            generation,
            target_id: i as u64 + 1,
            lease_id: s.next,
            resource,
            acquire_sync: none(),
        }
    };
    STATUS_OK
}
unsafe extern "C" fn commit(
    ctx: *mut c_void,
    stream: u64,
    submission: *const EncoderInputSubmission,
) -> i32 {
    let s = unsafe { state(ctx) };
    let v = unsafe { &*submission };
    let i = v.target_id.wrapping_sub(1) as usize;
    if stream != 1
        || s.stopped
        || v.generation != s.generation
        || i >= 2
        || s.slots[i] != Slot::Leased(v.lease_id)
    {
        return STATUS_INVALID_ARGUMENT;
    }
    if s.reject {
        s.reject = false;
        return MEDIA_BUSY;
    }
    s.slots[i] = Slot::Pending;
    STATUS_OK
}
unsafe extern "C" fn abandon(
    ctx: *mut c_void,
    stream: u64,
    generation: u64,
    id: u64,
    lease: u64,
    _: *const FrameDescriptor,
) -> i32 {
    let s = unsafe { state(ctx) };
    let i = id.wrapping_sub(1) as usize;
    if stream != 1 || generation != s.generation || i >= 2 || s.slots[i] != Slot::Leased(lease) {
        return STATUS_INVALID_ARGUMENT;
    }
    s.slots[i] = Slot::Free;
    STATUS_OK
}
unsafe extern "C" fn begin(ctx: *mut c_void, stream: u64) -> i32 {
    if stream != 1 {
        return STATUS_INVALID_ARGUMENT;
    }
    unsafe { state(ctx) }.stopped = true;
    STATUS_OK
}
unsafe extern "C" fn retirement(
    ctx: *mut c_void,
    stream: u64,
    generation: u64,
    out: *mut EncoderInputPoolRetirement,
) -> i32 {
    let s = unsafe { state(ctx) };
    if stream != 1 || generation != s.generation || !s.stopped {
        return STATUS_INVALID_ARGUMENT;
    }
    if let Some(status) = s.retirement_failure {
        return status;
    }
    if s.quarantined {
        unsafe {
            *out = EncoderInputPoolRetirement {
                version: 1,
                size: size_of::<EncoderInputPoolRetirement>() as u32,
                generation,
                state: ENCODER_INPUT_POOL_QUARANTINED,
                reserved: 0,
            }
        };
        return STATUS_OK;
    }
    if s.slots.iter().any(|v| *v != Slot::Free) {
        return MEDIA_PENDING;
    }
    unsafe {
        *out = EncoderInputPoolRetirement {
            version: 1,
            size: size_of::<EncoderInputPoolRetirement>() as u32,
            generation,
            state: ENCODER_INPUT_POOL_RETIRED,
            reserved: 0,
        }
    };
    STATUS_OK
}
fn fixture() -> Rc<Fixture> {
    let state = Box::new(UnsafeCell::new(State {
        generation: 0,
        next: 0,
        slots: [Slot::Free; 2],
        pixels: [0; 2],
        stopped: false,
        reject: false,
        quarantined: false,
        retirement_failure: None,
    }));
    let table = Box::new(EncoderInputPoolApiV1 {
        base: NativeInterface {
            header: AbiHeader::of::<EncoderInputPoolApiV1>(),
            context: state.get().cast(),
            release: None,
        },
        enumerate,
        configure,
        acquire,
        commit,
        abandon,
        begin_retirement: begin,
        retirement,
    });
    Rc::new(Fixture {
        state,
        table,
        dropped: Rc::new(Cell::new(false)),
    })
}
fn config() -> EncoderInputPoolConfig {
    EncoderInputPoolConfig {
        version: 1,
        size: size_of::<EncoderInputPoolConfig>() as u32,
        offer_generation: 1,
        offer_id: 1,
        width: 1,
        height: 1,
        target_count: 2,
        reserved: 0,
    }
}
fn submission(t: &EncoderInputTarget) -> EncoderInputSubmission {
    EncoderInputSubmission {
        version: 1,
        size: size_of::<EncoderInputSubmission>() as u32,
        generation: t.generation,
        target_id: t.target_id,
        lease_id: t.lease_id,
        control_id: 0,
        timestamp_us: 0,
        sequence: 1,
        color: FrameColorMetadata::default(),
        clock: FrameClockMetadata::default(),
        completion_sync: none(),
    }
}

#[test]
fn finite_pool_ownership_and_retirement() {
    let fixture = fixture();
    let dropped = Rc::clone(&fixture.dropped);
    let pool = unsafe { InputPool::new(&*fixture.table, Rc::clone(&fixture), 1) }.unwrap();
    let generation = pool.configure(&config()).unwrap();
    let a = pool.acquire().unwrap();
    let b = pool.acquire().unwrap();
    assert!(matches!(pool.acquire(), Err(MEDIA_BUSY)));
    let a_submission = submission(a.target());
    // The fixture's raw byte is a synthetic writable resource and no GPU work exists.
    unsafe {
        *(a.target().resource.data.cast_mut().cast::<u8>()) = 42;
    }
    assert!(unsafe { a.commit(&a_submission) }.is_ok());
    assert_eq!(unsafe { (*fixture.state.get()).pixels[0] }, 42);
    assert_eq!(
        unsafe { (fixture.table.commit)(fixture.table.base.context, 1, &a_submission) },
        STATUS_INVALID_ARGUMENT
    );
    let mut stale = submission(b.target());
    stale.generation += 1;
    assert_eq!(
        unsafe { (fixture.table.commit)(fixture.table.base.context, 1, &stale) },
        STATUS_INVALID_ARGUMENT
    );
    assert_eq!(
        unsafe { (fixture.table.commit)(fixture.table.base.context, 2, &submission(b.target())) },
        STATUS_INVALID_ARGUMENT
    );
    // Test simulates a failed submission without transferring the lease.
    unsafe {
        (*fixture.table.base.context.cast::<State>()).reject = true;
    }
    let b_submission = submission(b.target());
    let b = match unsafe { b.commit(&b_submission) } {
        Err((MEDIA_BUSY, lease)) => lease,
        _ => panic!("expected retryable failure"),
    };
    assert!(unsafe { b.abandon(&none()) }.is_ok());
    let c = pool.acquire().unwrap();
    assert_ne!(c.target().target_id, a_submission.target_id);
    assert_ne!(c.target().lease_id, a_submission.lease_id);
    pool.begin_retirement().unwrap();
    assert_eq!(pool.retirement(generation), Ok(Retirement::Pending));
    assert!(unsafe { c.abandon(&none()) }.is_ok());
    assert_eq!(pool.retirement(generation), Ok(Retirement::Pending));
    // The encoded read retires only after this simulated completion.
    unsafe {
        (*fixture.table.base.context.cast::<State>()).slots[0] = Slot::Free;
    }
    assert_eq!(pool.retirement(generation), Ok(Retirement::Retired));
    assert!(pool.finish().is_ok());
    drop(fixture);
    assert!(dropped.get());
}

#[test]
fn unresolved_mock_lease_does_not_drop_mock_guard() {
    let fixture = fixture();
    let dropped = Rc::clone(&fixture.dropped);
    let pool = unsafe { InputPool::new(&*fixture.table, Rc::clone(&fixture), 1) }.unwrap();
    pool.configure(&config()).unwrap();
    let lease = pool.acquire().unwrap();
    drop(lease);
    drop(pool);
    drop(fixture);
    assert!(!dropped.get());
}

#[test]
fn quarantined_mock_does_not_drop_mock_guard() {
    let fixture = fixture();
    let dropped = Rc::clone(&fixture.dropped);
    let pool = unsafe { InputPool::new(&*fixture.table, Rc::clone(&fixture), 1) }.unwrap();
    let generation = pool.configure(&config()).unwrap();
    pool.begin_retirement().unwrap();
    unsafe {
        (*fixture.state.get()).quarantined = true;
    }
    assert_eq!(pool.retirement(generation), Ok(Retirement::Quarantined));
    let pool = match pool.finish() {
        Err(pool) => pool,
        Ok(()) => panic!("quarantine released"),
    };
    drop(pool);
    drop(fixture);
    assert!(!dropped.get());
}

#[test]
fn contradictory_retirement_results_permanently_block_guard_release() {
    for contradiction in 0..3 {
        let fixture = fixture();
        let dropped = Rc::clone(&fixture.dropped);
        let pool = unsafe { InputPool::new(&*fixture.table, Rc::clone(&fixture), 1) }.unwrap();
        let generation = pool.configure(&config()).unwrap();
        pool.begin_retirement().unwrap();
        assert_eq!(pool.retirement(generation), Ok(Retirement::Retired));
        // Simulate a provider that retracts its previous terminal result.
        let state = unsafe { &mut *fixture.state.get() };
        match contradiction {
            0 => state.retirement_failure = Some(MEDIA_PENDING),
            1 => state.quarantined = true,
            _ => state.retirement_failure = Some(MEDIA_BUSY),
        }
        match contradiction {
            0 => assert_eq!(pool.retirement(generation), Ok(Retirement::Pending)),
            1 => assert_eq!(pool.retirement(generation), Ok(Retirement::Quarantined)),
            _ => assert_eq!(pool.retirement(generation), Err(MEDIA_BUSY)),
        }
        let pool = match pool.finish() {
            Err(pool) => pool,
            Ok(()) => panic!("stale retirement released guard"),
        };
        let state = unsafe { &mut *fixture.state.get() };
        state.retirement_failure = None;
        state.quarantined = false;
        assert_eq!(pool.retirement(generation), Ok(Retirement::Retired));
        let pool = match pool.finish() {
            Err(pool) => pool,
            Ok(()) => panic!("contradiction was forgotten"),
        };
        drop(pool);
        drop(fixture);
        assert!(!dropped.get());
    }
}
