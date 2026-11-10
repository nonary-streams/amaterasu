//! Flow trace sink: fixed-size observations recorded without locks or allocation.
//!
//! A producer obtains the table once through its authenticated control endpoint
//! (`trace_bind`) and keeps it until the runtime is destroyed. Names are interned
//! once on a cold path; the hot path passes plain integers. Analysis and
//! reporting belong to the sink, never to the producer's thread.
use crate::{AbiHeader, BorrowedBytes, STATUS_OK, Status};
use std::ffi::c_void;

/// The flow reached a stage. The first record of a flow starts it.
pub const TRACE_MARK: u8 = 1;
/// The flow reached its final stage. `value` is the slow-flow budget in
/// microseconds; zero selects the sink's default.
pub const TRACE_END: u8 = 2;
/// A completed span of work on one thread. `track` names the span, `stage` is
/// an optional label, `value` its duration and `timestamp_us` its end.
pub const TRACE_SPAN: u8 = 3;
/// The flow turned out not to belong to its track, such as a control message
/// that carried no input. It is forgotten: no statistics and no report.
pub const TRACE_DISCARD: u8 = 4;

pub const MAX_TRACE_NAME_BYTES: usize = 96;
pub const MAX_TRACE_RECORDS_PER_CALL: usize = 64;

/// One observation. It holds no pointers and can be copied between threads.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TraceRecord {
    /// Identity of the traced item within `track`. Producers on both sides of
    /// a boundary derive the same value from data they already exchange.
    pub flow: u64,
    /// Kind-specific: a stage detail for marks, the budget for an end, the
    /// duration in microseconds for a span.
    pub value: u64,
    /// Microseconds on the sink's clock, as returned by `TraceSinkApi::now`.
    /// Zero asks the sink to stamp the record on arrival.
    pub timestamp_us: u64,
    /// Interned name of the flow family, or of the span.
    pub track: u16,
    /// Interned stage name, or the span's label (zero for none).
    pub stage: u16,
    pub kind: u8,
    pub reserved: [u8; 3],
}

/// Runtime-owned table. Every function is thread-safe and may be called from
/// any producer thread until the runtime is destroyed.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct TraceSinkApi {
    pub header: AbiHeader,
    pub context: *mut c_void,
    /// The clock every nonzero `timestamp_us` must come from.
    pub now: unsafe extern "C" fn(*mut c_void) -> u64,
    /// Cold path. Writes a stable nonzero identifier for a printable name of
    /// at most `MAX_TRACE_NAME_BYTES`. The same name always yields the same id.
    pub intern: unsafe extern "C" fn(*mut c_void, BorrowedBytes, *mut u16) -> Status,
    /// Hot path. Copies up to `MAX_TRACE_RECORDS_PER_CALL` records without
    /// blocking or allocating. A full buffer drops records and counts them;
    /// that is not an error for the producer.
    pub record: unsafe extern "C" fn(*mut c_void, *const TraceRecord, usize) -> Status,
}

/// A validated copy of the sink table.
#[derive(Clone, Copy)]
pub struct TraceSink {
    api: TraceSinkApi,
}

// SAFETY: the sink contract makes every table function callable from any
// thread, and the context is owned by the runtime, which outlives its clients.
unsafe impl Send for TraceSink {}
// SAFETY: as above; the wrapper holds no thread-affine state of its own.
unsafe impl Sync for TraceSink {}

impl TraceSink {
    /// # Safety
    /// `address` must be the table returned by this process's runtime for a
    /// `trace_bind` request, and that runtime must outlive every use of the sink.
    pub unsafe fn from_address(address: usize) -> Option<Self> {
        if address == 0 || !address.is_multiple_of(align_of::<TraceSinkApi>()) {
            return None;
        }
        let pointer = address as *const TraceSinkApi;
        // SAFETY: the runtime promises a readable header at a bound address.
        let header = unsafe { *pointer.cast::<AbiHeader>() };
        if !header.supports::<TraceSinkApi>() {
            return None;
        }
        // SAFETY: the validated header covers the complete table.
        Some(Self {
            api: unsafe { *pointer },
        })
    }

    pub fn now(&self) -> u64 {
        // SAFETY: the table and its context are live for the sink's lifetime.
        unsafe { (self.api.now)(self.api.context) }
    }

    /// Returns zero when the sink cannot name another item; records that use
    /// identifier zero are ignored rather than misattributed.
    pub fn intern(&self, name: &str) -> u16 {
        let mut identifier = 0;
        // SAFETY: the name is borrowed for this call and the output is writable.
        let status = unsafe {
            (self.api.intern)(
                self.api.context,
                BorrowedBytes::new(name.as_bytes()),
                &mut identifier,
            )
        };
        if status == STATUS_OK { identifier } else { 0 }
    }

    pub fn record(&self, record: &TraceRecord) {
        // SAFETY: one initialized record is borrowed for this synchronous call.
        unsafe { (self.api.record)(self.api.context, record, 1) };
    }
}
