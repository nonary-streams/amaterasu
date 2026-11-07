use super::*;
use crate::abi::{AbiHeader, NativeInterface};
use std::{
    cell::{Cell, UnsafeCell},
    rc::Rc,
};

struct State {
    caps: EncoderInputCpuWritableCapabilities,
    pixels: Vec<u8>,
    target: EncoderInputCpuWritableFrame,
    generation: u64,
    next: u64,
    leased: bool,
    busy: bool,
    reject: bool,
    reject_abandon: bool,
    bad_descriptor: bool,
    acquire_fence: bool,
    abandoned: u32,
    submitted: Option<EncoderInputSubmission>,
    stopped: bool,
}
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
unsafe fn state<'a>(context: *mut c_void) -> &'a mut State {
    unsafe { &mut *context.cast::<State>() }
}
unsafe extern "C" fn enumerate(
    ctx: *mut c_void,
    _: u64,
    sink: CaptureResourceOfferSink,
    generation: *mut u64,
) -> Status {
    let s = unsafe { state(ctx) };
    let offer = CaptureResourceOffer {
        id: 8,
        ownership: FRAME_OWNERSHIP_BORROWED,
        accessibility: FRAME_ACCESS_CPU,
        reserved: 0,
        resource: FrameDescriptor {
            kind: ENCODER_INPUT_RESOURCE_CPU_WRITABLE,
            version: 1,
            size: size_of::<EncoderInputCpuWritableCapabilities>() as u32,
            reserved: 0,
            namespace: [0; 16],
            data: (&raw const s.caps).cast(),
        },
        acquire_sync: none_descriptor(),
        completion_sync: none_descriptor(),
    };
    unsafe {
        *generation = 9;
        (sink.write)(sink.context, &offer)
    }
}
unsafe extern "C" fn configure(
    ctx: *mut c_void,
    _: u64,
    cfg: *const EncoderInputPoolConfig,
    out: *mut u64,
) -> Status {
    let s = unsafe { state(ctx) };
    let cfg = unsafe { &*cfg };
    if cfg.offer_generation != 9 || cfg.offer_id != 8 {
        return STATUS_INVALID_ARGUMENT;
    }
    s.generation += 1;
    unsafe { *out = s.generation };
    STATUS_OK
}
unsafe extern "C" fn acquire(
    ctx: *mut c_void,
    _: u64,
    generation: u64,
    out: *mut EncoderInputTarget,
) -> Status {
    let s = unsafe { state(ctx) };
    if s.busy || s.leased {
        return MEDIA_BUSY;
    }
    s.next += 1;
    s.leased = true;
    s.target.data = s.pixels.as_mut_ptr();
    let resource = FrameDescriptor {
        kind: if s.bad_descriptor {
            FRAME_RESOURCE_CPU
        } else {
            ENCODER_INPUT_RESOURCE_CPU_WRITABLE
        },
        version: 1,
        size: size_of::<EncoderInputCpuWritableFrame>() as u32,
        reserved: 0,
        namespace: [0; 16],
        data: (&raw const s.target).cast(),
    };
    let mut sync = none_descriptor();
    if s.acquire_fence {
        sync.kind = FRAME_SYNC_SYNC_FILE;
    }
    unsafe {
        *out = EncoderInputTarget {
            generation,
            target_id: 1,
            lease_id: s.next,
            resource,
            acquire_sync: sync,
        };
    }
    STATUS_OK
}
unsafe extern "C" fn commit(
    ctx: *mut c_void,
    _: u64,
    submission: *const EncoderInputSubmission,
) -> Status {
    let s = unsafe { state(ctx) };
    if s.reject {
        return MEDIA_BUSY;
    }
    s.submitted = Some(unsafe { *submission });
    s.leased = false;
    STATUS_OK
}
unsafe extern "C" fn abandon(
    ctx: *mut c_void,
    _: u64,
    _: u64,
    _: u64,
    _: u64,
    completion: *const FrameDescriptor,
) -> Status {
    let s = unsafe { state(ctx) };
    assert!(none(unsafe { &*completion }));
    if s.reject_abandon {
        return MEDIA_BUSY;
    }
    s.leased = false;
    s.abandoned += 1;
    STATUS_OK
}
unsafe extern "C" fn begin(ctx: *mut c_void, _: u64) -> Status {
    unsafe { state(ctx) }.stopped = true;
    STATUS_OK
}
unsafe extern "C" fn retirement(
    ctx: *mut c_void,
    _: u64,
    generation: u64,
    out: *mut EncoderInputPoolRetirement,
) -> Status {
    let s = unsafe { state(ctx) };
    if s.leased {
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
fn fixture() -> (Rc<Fixture>, InputPool<Rc<Fixture>>, CpuPoolSelection) {
    fixture_format(PIXEL_BGRA8)
}

fn fixture_format(format: u32) -> (Rc<Fixture>, InputPool<Rc<Fixture>>, CpuPoolSelection) {
    let state = Box::new(UnsafeCell::new(State {
        caps: EncoderInputCpuWritableCapabilities {
            format,
            min_width: 2,
            max_width: 2,
            min_height: 2,
            max_height: 2,
            stride_alignment: 1,
            data_alignment: 1,
        },
        pixels: vec![0; 24],
        target: EncoderInputCpuWritableFrame {
            width: 2,
            height: 2,
            format,
            stride: if format == PIXEL_P010 { 8 } else { 12 },
            data: ptr::null_mut(),
            len: 24,
        },
        generation: 0,
        next: 0,
        leased: false,
        busy: false,
        reject: false,
        reject_abandon: false,
        bad_descriptor: false,
        acquire_fence: false,
        abandoned: 0,
        submitted: None,
        stopped: false,
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
    let fixture = Rc::new(Fixture {
        state,
        table,
        dropped: Rc::new(Cell::new(false)),
    });
    let pool = unsafe { InputPool::new(&*fixture.table, fixture.clone(), 1) }.unwrap();
    let selection = unsafe { super::configure(&pool, 2, 2, format, 1) }.unwrap();
    (fixture, pool, selection)
}
unsafe extern "C" fn retain(_: *mut c_void) -> Status {
    MEDIA_UNSUPPORTED
}
unsafe extern "C" fn release(_: *mut c_void, _: *const FrameDescriptor) {}
fn capture(cpu: &CpuFrame) -> CaptureResourceFrame {
    CaptureResourceFrame {
        offer_generation: 3,
        offer_id: 4,
        timestamp_us: cpu.timestamp_us,
        capture_pacing_timestamp_us: 0,
        sequence: cpu.sequence,
        width: cpu.width,
        height: cpu.height,
        freshness: CAPTURE_FRAME_FRESH,
        reserved: 0,
        color: FrameColorMetadata::default(),
        clock: FrameClockMetadata::default(),
        pacing_clock: FrameClockMetadata::default(),
        resource: FrameDescriptor {
            kind: FRAME_RESOURCE_CPU,
            version: 1,
            size: size_of::<CpuFrame>() as u32,
            reserved: 0,
            namespace: [0; 16],
            data: (cpu as *const CpuFrame).cast(),
        },
        acquire_sync: none_descriptor(),
        context: ptr::null_mut(),
        retain,
        release,
    }
}
fn source(bytes: &[u8]) -> CpuFrame {
    CpuFrame {
        width: 2,
        height: 2,
        format: PIXEL_BGRA8,
        stride: 8,
        data: bytes.as_ptr(),
        len: bytes.len(),
        timestamp_us: 11,
        sequence: 12,
    }
}
fn finish(pool: InputPool<Rc<Fixture>>, generation: u64) {
    pool.begin_retirement().unwrap();
    assert!(matches!(
        pool.retirement(generation),
        Ok(crate::encode::pool::Retirement::Retired)
    ));
    assert!(pool.finish().is_ok());
}
#[test]
fn copies_rows_and_metadata_and_retires() {
    let (fixture, pool, selection) = fixture();
    let data: Vec<u8> = (0..16).collect();
    let cpu = source(&data);
    let mut frame = capture(&cpu);
    frame.color = FrameColorMetadata {
        version: 1,
        size: size_of::<FrameColorMetadata>() as u32,
        primaries: 1,
        ..Default::default()
    };
    assert!(unsafe { write(&pool, &selection, &frame, 3, 4, 12) }.is_ok());
    let s = unsafe { &*fixture.state.get() };
    assert_eq!(&s.pixels[0..8], &data[0..8]);
    assert_eq!(&s.pixels[12..20], &data[8..16]);
    let submitted = s.submitted.unwrap();
    assert_eq!(
        (
            submitted.sequence,
            submitted.timestamp_us,
            submitted.control_id,
            submitted.color.primaries
        ),
        (12, 11, 12, 1)
    );
    assert!(none(&submitted.completion_sync));
    finish(pool, selection.generation);
}

#[test]
fn p010_copy_preserves_both_planes_with_independent_source_and_target_padding() {
    let (fixture, pool, selection) = fixture_format(PIXEL_P010);
    // SAFETY: no callback or lease is active while the test changes its provider fixture.
    let state = unsafe { &mut *fixture.state.get() };
    state.caps.format = PIXEL_P010;
    state.target.format = PIXEL_P010;
    state.target.stride = 8;
    state.target.len = 24;
    state.pixels = vec![0xee; 24];
    let mut bytes = [0xaa; 18];
    for row in 0..3 {
        let code = if row < 2 { 64u16 + row as u16 } else { 512u16 };
        for column in 0..2 {
            bytes[row * 6 + column * 2..row * 6 + column * 2 + 2]
                .copy_from_slice(&(code << 6).to_le_bytes());
        }
    }
    let cpu = CpuFrame {
        width: 2,
        height: 2,
        format: PIXEL_P010,
        stride: 6,
        data: bytes.as_ptr(),
        len: bytes.len(),
        timestamp_us: 11,
        sequence: 12,
    };
    let frame = capture(&cpu);
    // SAFETY: provider's input pool and immutable capture pixels remain live for the copy.
    assert!(unsafe { write(&pool, &selection, &frame, 3, 4, 12) }.is_ok());
    let state = unsafe { &*fixture.state.get() };
    for row in 0..3 {
        assert_eq!(
            &state.pixels[row * 8..row * 8 + 4],
            &bytes[row * 6..row * 6 + 4]
        );
        assert_eq!(&state.pixels[row * 8 + 4..row * 8 + 8], &[0xee; 4]);
    }
    assert_eq!(state.submitted.unwrap().sequence, 12);
    finish(pool, selection.generation());
}
#[test]
fn busy_rejected_commit_and_failed_abandon_are_safe() {
    let (fixture, pool, selection) = fixture();
    let data = [1; 16];
    let cpu = source(&data);
    let frame = capture(&cpu);
    let s = fixture.state.get();
    unsafe { (*s).busy = true };
    assert!(matches!(
        unsafe { write(&pool, &selection, &frame, 3, 4, 0) },
        Err(WriteError::Status(MEDIA_BUSY))
    ));
    unsafe {
        (*s).busy = false;
        (*s).reject = true;
    }
    assert!(matches!(
        unsafe { write(&pool, &selection, &frame, 3, 4, 0) },
        Err(WriteError::Status(MEDIA_BUSY))
    ));
    assert_eq!(unsafe { (*s).abandoned }, 1);
    unsafe {
        (*s).reject_abandon = true;
    }
    let lease = match unsafe { write(&pool, &selection, &frame, 3, 4, 0) } {
        Err(WriteError::Abandon {
            status: MEDIA_BUSY,
            lease,
        }) => lease,
        _ => panic!("expected outstanding lease"),
    };
    assert!(unsafe { (*s).leased });
    unsafe {
        (*s).reject_abandon = false;
    }
    assert!(unsafe { lease.abandon(&none_descriptor()) }.is_ok());
    finish(pool, selection.generation);
}
#[test]
fn malformed_target_capacity_sync_and_generation_are_abandoned() {
    let (fixture, pool, selection) = fixture();
    let data = [1; 16];
    let cpu = source(&data);
    let frame = capture(&cpu);
    let s = fixture.state.get();
    unsafe {
        (*s).bad_descriptor = true;
    }
    assert!(matches!(
        unsafe { write(&pool, &selection, &frame, 3, 4, 0) },
        Err(WriteError::Status(STATUS_INVALID_ARGUMENT))
    ));
    unsafe {
        (*s).bad_descriptor = false;
        (*s).target.len = 7;
    }
    assert!(matches!(
        unsafe { write(&pool, &selection, &frame, 3, 4, 0) },
        Err(WriteError::Status(STATUS_INVALID_ARGUMENT))
    ));
    unsafe {
        (*s).target.len = 24;
        (*s).acquire_fence = true;
    }
    assert!(matches!(
        unsafe { write(&pool, &selection, &frame, 3, 4, 0) },
        Err(WriteError::Status(STATUS_INVALID_ARGUMENT))
    ));
    unsafe {
        (*s).acquire_fence = false;
    }
    let stale = CpuPoolSelection {
        generation: selection.generation + 1,
        ..selection
    };
    assert!(matches!(
        unsafe { write(&pool, &stale, &frame, 3, 4, 0) },
        Err(WriteError::Status(STATUS_INVALID_ARGUMENT))
    ));
    assert_eq!(unsafe { (*s).abandoned }, 4);
    finish(pool, selection.generation);
}

#[test]
fn overlap_is_rejected_and_guard_survives_until_retirement() {
    let (fixture, pool, selection) = fixture();
    let weak = Rc::downgrade(&fixture);
    let data = unsafe { (*fixture.state.get()).pixels.as_ptr() };
    let cpu = CpuFrame {
        width: 2,
        height: 2,
        format: PIXEL_BGRA8,
        stride: 8,
        data,
        len: 16,
        timestamp_us: 11,
        sequence: 12,
    };
    let frame = capture(&cpu);
    assert!(matches!(
        unsafe { write(&pool, &selection, &frame, 3, 4, 0) },
        Err(WriteError::Status(STATUS_INVALID_ARGUMENT))
    ));
    assert_eq!(unsafe { (*fixture.state.get()).abandoned }, 1);
    drop(fixture);
    assert!(weak.upgrade().is_some());
    finish(pool, selection.generation);
    assert!(weak.upgrade().is_none());
}

#[test]
fn nv12_copy_preserves_both_planes_with_padded_target_stride() {
    let (fixture, pool, first) = fixture();
    let state = fixture.state.get();
    unsafe {
        (*state).caps.format = PIXEL_NV12;
        (*state).target.format = PIXEL_NV12;
        (*state).target.stride = 4;
        (*state).target.len = 12;
    }
    let selection = unsafe { super::configure(&pool, 2, 2, PIXEL_NV12, 1) }.unwrap();
    let pixels = [1, 2, 3, 4, 5, 6];
    let cpu = CpuFrame {
        width: 2,
        height: 2,
        format: PIXEL_NV12,
        stride: 2,
        data: pixels.as_ptr(),
        len: pixels.len(),
        timestamp_us: 11,
        sequence: 12,
    };
    assert!(unsafe { write(&pool, &selection, &capture(&cpu), 3, 4, 0) }.is_ok());
    let state = unsafe { &*fixture.state.get() };
    assert_eq!(&state.pixels[0..2], &pixels[0..2]);
    assert_eq!(&state.pixels[4..6], &pixels[2..4]);
    assert_eq!(&state.pixels[8..10], &pixels[4..6]);
    pool.begin_retirement().unwrap();
    assert!(matches!(
        pool.retirement(first.generation),
        Ok(crate::encode::pool::Retirement::Retired)
    ));
    assert!(matches!(
        pool.retirement(selection.generation),
        Ok(crate::encode::pool::Retirement::Retired)
    ));
    assert!(pool.finish().is_ok());
}
