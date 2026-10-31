use super::abi::{self, BufferLease, Chunk, Limits, Segment};
use amaterasu_abi::{STATUS_INVALID_ARGUMENT, Status};

/// Ownership stays within the provider library; descriptors are borrowed at the
/// C boundary. Boxed slices make retained storage match charged payload bytes.
#[derive(Debug)]
pub struct OwnedChunk {
    pub record_id: u64,
    pub offset: u64,
    pub total_bytes: u64,
    pub segments: Box<[Box<[u8]>]>,
    pub metadata: Box<[u8]>,
}

impl OwnedChunk {
    pub fn with_bytes(record_id: u64, bytes: Vec<u8>) -> Self {
        Self {
            record_id,
            offset: 0,
            total_bytes: bytes.len() as u64,
            segments: vec![bytes.into_boxed_slice()].into_boxed_slice(),
            metadata: Box::default(),
        }
    }
    pub(super) fn descriptor(&self, segments: &[Segment]) -> Chunk {
        Chunk {
            size: size_of::<Chunk>() as u32,
            reserved: 0,
            record_id: self.record_id,
            offset: self.offset,
            total_bytes: self.total_bytes,
            segments: segments.as_ptr(),
            segment_count: segments.len() as u32,
            metadata_len: self.metadata.len() as u32,
            metadata: self.metadata.as_ptr(),
            lease: std::ptr::null(),
        }
    }
    pub(super) fn segments(&self) -> Vec<Segment> {
        self.segments
            .iter()
            .map(|s| Segment {
                data: s.as_ptr(),
                len: s.len(),
            })
            .collect()
    }
    pub(super) fn validate(&self, limits: Limits) -> Result<Shape, Status> {
        if self.segments.len() > limits.max_segments as usize
            || self.metadata.len() > limits.max_metadata_bytes as usize
        {
            return Err(abi::TOO_LARGE);
        }
        let segments = self.segments();
        let descriptor = self.descriptor(&segments);
        shape(&descriptor, &segments, limits)
    }
}

#[derive(Clone, Copy)]
pub(super) struct Shape {
    pub record_id: u64,
    pub offset: u64,
    pub total: u64,
    pub payload: u64,
    pub charge: u64,
}

fn shape(chunk: &Chunk, segments: &[Segment], limits: Limits) -> Result<Shape, Status> {
    if chunk.size < size_of::<Chunk>() as u32
        || chunk.reserved != 0
        || chunk.record_id == 0
        || chunk.total_bytes == 0
        || segments.is_empty()
        || chunk.metadata_len != 0 && (chunk.metadata.is_null() || chunk.offset != 0)
    {
        return Err(STATUS_INVALID_ARGUMENT);
    }
    if segments.len() > limits.max_segments as usize
        || chunk.metadata_len > limits.max_metadata_bytes
        || chunk.total_bytes > limits.max_record_bytes
    {
        return Err(abi::TOO_LARGE);
    }
    let mut payload = 0u64;
    for segment in segments {
        if segment.data.is_null() || segment.len == 0 || segment.len > isize::MAX as usize {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        payload = payload
            .checked_add(segment.len as u64)
            .ok_or(abi::TOO_LARGE)?;
    }
    if payload > limits.max_chunk_bytes {
        return Err(abi::TOO_LARGE);
    }
    if chunk
        .offset
        .checked_add(payload)
        .is_none_or(|end| end > chunk.total_bytes)
    {
        return Err(STATUS_INVALID_ARGUMENT);
    }
    let charge = payload
        .checked_add(u64::from(chunk.metadata_len))
        .ok_or(abi::TOO_LARGE)?;
    Ok(Shape {
        record_id: chunk.record_id,
        offset: chunk.offset,
        total: chunk.total_bytes,
        payload,
        charge,
    })
}

/// Validated synchronous native borrow. Does not retain or allocate payload.
pub(super) struct BorrowedChunk<'a> {
    chunk: &'a Chunk,
    segments: &'a [Segment],
    pub shape: Shape,
    lease: Option<BufferLease>,
}
impl<'a> BorrowedChunk<'a> {
    /// # Safety
    /// Descriptor and arrays are readable, aligned synchronous borrows. Each
    /// allocation obeys Chunk/BufferLease and remains immutable for this call.
    pub unsafe fn new(chunk: &'a Chunk, limits: Limits) -> Result<Self, Status> {
        if chunk.segment_count == 0
            || chunk.segment_count > limits.max_segments
            || chunk.segments.is_null()
            || !(chunk.segments as usize).is_multiple_of(align_of::<Segment>())
        {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        // SAFETY: caller guarantees readable descriptor array; count is bounded.
        let segments =
            unsafe { std::slice::from_raw_parts(chunk.segments, chunk.segment_count as usize) };
        let mut shape = shape(chunk, segments, limits)?;
        let lease = if chunk.lease.is_null() {
            None
        } else {
            if !(chunk.lease as usize).is_multiple_of(align_of::<BufferLease>()) {
                return Err(STATUS_INVALID_ARGUMENT);
            }
            // SAFETY: native caller supplies a readable lease descriptor.
            let lease = unsafe { *chunk.lease };
            if lease.retained_bytes < shape.charge {
                return Err(STATUS_INVALID_ARGUMENT);
            }
            shape.charge = lease.retained_bytes;
            Some(lease)
        };
        if shape.charge > limits.max_queued_bytes {
            return Err(abi::TOO_LARGE);
        }
        Ok(Self {
            chunk,
            segments,
            shape,
            lease,
        })
    }
    /// Called only after reserving capacity, so BUSY never retains or copies.
    pub fn retain_or_copy(self) -> StoredChunk {
        if let Some(lease) = self.lease {
            let segments = self.segments.to_vec();
            // SAFETY: constructor established lease lifetime/threading contract.
            unsafe { (lease.retain)(lease.context) };
            StoredChunk::Leased(LeasedChunk {
                descriptor: *self.chunk,
                segments,
                lease,
            })
        } else {
            // SAFETY: constructor's caller promises readable payload/metadata.
            let segments = self
                .segments
                .iter()
                .map(|segment| unsafe {
                    std::slice::from_raw_parts(segment.data, segment.len).into()
                })
                .collect();
            let metadata = if self.chunk.metadata_len == 0 {
                Box::default()
            } else {
                unsafe {
                    std::slice::from_raw_parts(
                        self.chunk.metadata,
                        self.chunk.metadata_len as usize,
                    )
                    .into()
                }
            };
            StoredChunk::Owned(OwnedChunk {
                record_id: self.shape.record_id,
                offset: self.shape.offset,
                total_bytes: self.shape.total,
                segments,
                metadata,
            })
        }
    }
}

pub(super) struct LeasedChunk {
    descriptor: Chunk,
    segments: Vec<Segment>,
    lease: BufferLease,
}
// SAFETY: constructor requires immutable allocations and thread-safe lease
// callbacks. Storage is private and dropped only after synchronous sinks finish.
unsafe impl Send for LeasedChunk {}
impl Drop for LeasedChunk {
    fn drop(&mut self) {
        // SAFETY: exactly one retained reference, no sink borrows remain.
        unsafe { (self.lease.release)(self.lease.context) };
    }
}
pub(super) enum StoredChunk {
    Owned(OwnedChunk),
    Leased(LeasedChunk),
}
impl StoredChunk {
    pub fn with_descriptor<T>(&self, callback: impl FnOnce(&Chunk) -> T) -> T {
        match self {
            Self::Owned(chunk) => {
                let segments = chunk.segments();
                callback(&chunk.descriptor(&segments))
            }
            Self::Leased(chunk) => {
                let descriptor = Chunk {
                    segments: chunk.segments.as_ptr(),
                    lease: std::ptr::null(),
                    ..chunk.descriptor
                };
                callback(&descriptor)
            }
        }
    }
}
