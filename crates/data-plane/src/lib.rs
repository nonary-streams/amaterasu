//! Versioned, protocol-neutral CPU binary transfer contracts for trusted plugins.
//! Control authorization/negotiation is separate from these direct native calls.
//! No allocator, collection, Rust reference, network socket or codec crosses this ABI.
use amaterasu_abi::{NativeInterface, Status};
use serde::{Deserialize, Serialize};
use std::ffi::c_void;

pub const INTERFACE_MAJOR: u32 = 1;
pub const INTERFACE_MINOR: u32 = 0;
pub const API_SIZE: u32 = size_of::<TransferApi>() as u32;
pub const BUSY: Status = 30;
pub const CLOSED: Status = 31;
pub const UNSUPPORTED: Status = 32;
pub const DENIED: Status = 33;
pub const TOO_LARGE: Status = 34;
pub const EMPTY: Status = 35;
pub const TIMED_OUT: Status = 36;
pub const MAX_SEGMENTS: u32 = 256;
pub const MAX_METADATA_BYTES: u32 = 64 * 1024;
pub const MAX_WAIT_MS: u32 = 60_000;
pub const SOURCE: u32 = 1;
pub const SINK: u32 = 2;
pub const ACTIVE: u32 = 1;
pub const DRAINING: u32 = 2;
pub const ENDED: u32 = 3;
pub const CANCELLED: u32 = 4;

/// IDs are negotiated, not a closed enum. Unknown namespaces require agreement
/// between the producer and consumer; the runtime never interprets their bytes.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Schema {
    pub namespace: u64,
    pub value: u64,
    pub version: u64,
}

/// Incarnation prevents reuse after provider restart. Neither field is an
/// authorization credential: every access is checked against the bound principal.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChannelId {
    pub incarnation: u64,
    pub sequence: u64,
}

/// Byte limits are negotiated independently of the JSON/control message limit.
/// Queued bytes count payload AND metadata, including an in-flight receive.
/// Logical records can span many chunks; offsets and lengths are checked u64s.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    pub max_chunk_bytes: u64,
    pub max_queued_bytes: u64,
    pub max_record_bytes: u64,
    pub max_queued_chunks: u32,
    pub max_segments: u32,
    pub max_metadata_bytes: u32,
    pub reserved: u32,
}

/// Immutable result of authorized control negotiation. SOURCE permits native
/// receive; SINK permits native submit. Schemas must agree exactly before open.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Description {
    pub size: u32,
    pub version: u32,
    pub direction: u32,
    pub reserved: u32,
    pub format: Schema,
    pub metadata_schema: Schema,
    pub limits: Limits,
}

/// CPU-readable immutable allocation, independent of BorrowedBytes/MAX_MESSAGE.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Segment {
    pub data: *const u8,
    pub len: usize,
}

/// Optional zero-copy input ownership. All descriptor arrays, metadata and
/// payload bytes in the submitted chunk stay immutable/live while retained.
/// Callbacks are thread-safe, nonblocking, infallible, and cannot unwind or
/// reenter transfer operations. Every successful retain has exactly one release.
/// Producer code/DLL and allocations must outlive the references. Cancelling
/// or releasing the consuming binding is a synchronous retirement barrier;
/// finish that barrier BEFORE releasing/unloading the producing binding.
/// This is CPU storage, not a GPU completion fence or transferable OS handle.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct BufferLease {
    pub context: *mut c_void,
    /// Total storage pinned by this reference, including unused capacity. Must
    /// cover payload + metadata; charged against the negotiated byte budget.
    pub retained_bytes: u64,
    pub retain: unsafe extern "C" fn(*mut c_void),
    pub release: unsafe extern "C" fn(*mut c_void),
}

/// A fragment of one logical record, not a wire datagram. IDs strictly increase
/// between records; fragments are contiguous, ordered and share total_bytes.
/// Metadata is schema-defined, pointer-free and allowed only at offset zero.
/// All pointers are synchronous borrows unless `lease` is supplied to submit.
/// Receive always provides synchronous borrows with a null lease; consumers
/// cannot retain them after their callback. They may submit synchronously to
/// another service. A future GPU/retained-output extension needs negotiation.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Chunk {
    pub size: u32,
    pub reserved: u32,
    pub record_id: u64,
    pub offset: u64,
    pub total_bytes: u64,
    pub segments: *const Segment,
    pub segment_count: u32,
    pub metadata_len: u32,
    pub metadata: *const u8,
    pub lease: *const BufferLease,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct ChunkSink {
    pub context: *mut c_void,
    pub write: unsafe extern "C" fn(*mut c_void, *const Chunk) -> Status,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct Snapshot {
    pub generation: u64,
    pub queued_bytes: u64,
    pub queued_chunks: u32,
    pub phase: u32,
}

/// Calls can run concurrently on provider-approved worker threads. No hot-path
/// broker round trips or module ticks are required. No callback may unwind.
/// Do not reenter receive, cancel, or release for the same channel from a sink.
/// Release is called once, after all table calls finish; it cancels and retires
/// every channel claimed by this binding before returning. IDs cannot be rebound.
#[repr(C)]
pub struct TransferApi {
    pub base: NativeInterface,
    pub describe: unsafe extern "C" fn(*mut c_void, ChannelId, *mut Description) -> Status,
    /// OK accepts the entire chunk locally, never proves peer delivery. BUSY
    /// accepts nothing, retains nothing, and changes no ordering state. With no
    /// lease, copies into provider-owned memory; with a lease, retains once.
    pub submit: unsafe extern "C" fn(*mut c_void, ChannelId, *const Chunk) -> Status,
    /// At most one synchronous callback. Non-OK leaves the front chunk for
    /// retry unless cancelled. Concurrent receives return BUSY. Never waits.
    pub receive: unsafe extern "C" fn(*mut c_void, ChannelId, ChunkSink) -> Status,
    pub snapshot: unsafe extern "C" fn(*mut c_void, ChannelId, *mut Snapshot) -> Status,
    /// Waits on a state generation, avoiding lost wakeups. Run on a worker,
    /// not the module tick. timeout_ms <= MAX_WAIT_MS, zero is a snapshot.
    /// Terminal states return immediately, even at the same generation.
    pub wait: unsafe extern "C" fn(*mut c_void, ChannelId, u64, u32, *mut Snapshot) -> Status,
    /// Stop admission. Reject an unfinished logical record; queued chunks are
    /// readable until empty. No invented peer acknowledgment or on-wire EOS.
    pub finish: unsafe extern "C" fn(*mut c_void, ChannelId) -> Status,
    /// Stop admission, wake waiters, discard pending chunks, and wait for any
    /// in-flight sink and retained input release callbacks before returning.
    pub cancel: unsafe extern "C" fn(*mut c_void, ChannelId) -> Status,
}

#[cfg(all(test, target_pointer_width = "64"))]
mod tests {
    use super::*;
    #[test]
    fn public_c_layout_is_stable() {
        assert_eq!(size_of::<Schema>(), 24);
        assert_eq!(size_of::<ChannelId>(), 16);
        assert_eq!(size_of::<Limits>(), 40);
        assert_eq!(size_of::<Description>(), 104);
        assert_eq!(size_of::<Segment>(), 16);
        assert_eq!(size_of::<BufferLease>(), 32);
        assert_eq!(size_of::<Chunk>(), 64);
        assert_eq!(size_of::<Snapshot>(), 24);
        assert_eq!(size_of::<TransferApi>(), 88);
        assert_eq!(std::mem::offset_of!(TransferApi, describe), 32);
        assert_eq!(std::mem::offset_of!(TransferApi, cancel), 80);
        assert_eq!(std::mem::offset_of!(Chunk, segments), 32);
        assert_eq!(std::mem::offset_of!(Chunk, lease), 56);
    }
}
