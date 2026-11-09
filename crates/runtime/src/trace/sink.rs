//! C-callable trace sink. This is an ABI boundary: producers are trusted
//! native code that must pass readable records and a live sink context.
use super::hub::TraceHub;
use super::ring::Observation;
use amaterasu_abi::{
    AbiHeader, BorrowedBytes, MAX_TRACE_RECORDS_PER_CALL, STATUS_INVALID_ARGUMENT, STATUS_OK,
    STATUS_UNAVAILABLE, Status, TraceRecord, TraceSinkApi,
};
use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

/// Identifies the endpoint whose records these are. The runtime chooses it;
/// a record cannot name its own producer.
struct Source {
    hub: Arc<TraceHub>,
    source: u16,
}

/// One endpoint's sink. Both allocations are boxed so the published table and
/// its context keep their addresses while the endpoint lives.
pub(crate) struct Binding {
    api: Box<TraceSinkApi>,
    _source: Box<Source>,
}

// SAFETY: the table is immutable after construction and its context points at
// a `Source`, which holds only an `Arc` to the thread-safe hub and an integer.
unsafe impl Send for Binding {}
// SAFETY: as above; shared access never mutates either allocation.
unsafe impl Sync for Binding {}

impl Binding {
    pub fn new(hub: Arc<TraceHub>, principal: &str) -> Self {
        let source = Box::new(Source {
            source: hub.source_id(principal),
            hub,
        });
        let api = Box::new(TraceSinkApi {
            header: AbiHeader::of::<TraceSinkApi>(),
            context: std::ptr::from_ref::<Source>(&source) as *mut c_void,
            now,
            intern,
            record,
        });
        Self {
            api,
            _source: source,
        }
    }

    /// Process-local descriptor returned to the bound endpoint.
    pub fn address(&self) -> usize {
        std::ptr::from_ref::<TraceSinkApi>(&self.api) as usize
    }
}

unsafe extern "C" fn now(context: *mut c_void) -> u64 {
    if context.is_null() {
        return 0;
    }
    // SAFETY: the context is this module's live `Source` until runtime destruction.
    let source = unsafe { &*context.cast::<Source>() };
    source.hub.now_us()
}

unsafe extern "C" fn intern(context: *mut c_void, name: BorrowedBytes, output: *mut u16) -> Status {
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        if context.is_null() || output.is_null() {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: the producer lends readable name bytes for this call.
        let Ok(bytes) = (unsafe { name.read() }) else {
            return STATUS_INVALID_ARGUMENT;
        };
        let Ok(name) = std::str::from_utf8(bytes) else {
            return STATUS_INVALID_ARGUMENT;
        };
        // SAFETY: the context is this module's live `Source`.
        let source = unsafe { &*context.cast::<Source>() };
        match source.hub.intern(name) {
            Some(identifier) => {
                // SAFETY: the producer supplied writable storage for one identifier.
                unsafe { *output = identifier };
                STATUS_OK
            }
            None => STATUS_UNAVAILABLE,
        }
    }));
    outcome.unwrap_or(STATUS_INVALID_ARGUMENT)
}

unsafe extern "C" fn record(
    context: *mut c_void,
    records: *const TraceRecord,
    count: usize,
) -> Status {
    if context.is_null() || (records.is_null() && count != 0) || count > MAX_TRACE_RECORDS_PER_CALL
    {
        return STATUS_INVALID_ARGUMENT;
    }
    // SAFETY: the context is this module's live `Source`; the producer lends
    // `count` initialized records for this synchronous call.
    let (source, records) = unsafe {
        (
            &*context.cast::<Source>(),
            std::slice::from_raw_parts(records, count),
        )
    };
    // One clock read serves the whole call. Nothing below allocates, locks or
    // can panic: the queue is fixed-size and indexes through a mask.
    let mut arrival_us = 0;
    for record in records {
        let timestamp_us = if record.timestamp_us != 0 {
            record.timestamp_us
        } else {
            if arrival_us == 0 {
                arrival_us = source.hub.now_us();
            }
            arrival_us
        };
        source.hub.push(Observation {
            flow: record.flow,
            value: record.value,
            timestamp_us,
            track: record.track,
            stage: record.stage,
            kind: record.kind,
            source: source.source,
        });
    }
    STATUS_OK
}
