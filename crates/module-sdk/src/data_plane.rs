//! Reusable binary-transfer SDK. Provider control services authorize creation;
//! the native factory binds the runtime-attested caller to those channels.
//! Grants route trusted native code and do not sandbox it. Providers must
//! register the transfer family for all aliases; legacy untyped factories are
//! rejected because matching table size and version do not identify a contract.
pub use amaterasu_data_plane as abi;
mod client;
mod duplex;
mod hub;
mod native;
mod queue;
mod storage;
pub use client::{ChunkView, TransferClient};
pub use duplex::DuplexPair;
pub use hub::{Hub, NativeFactory};
pub use queue::Channel;
pub use storage::OwnedChunk;

/// Identity of the reusable binary transfer interface, independent of service alias.
pub const TRANSFER_CONTRACT: &str = "amaterasu.binary-transfer.native/1";

/// Register a binary transfer provider.
///
/// # Safety
/// The factory must implement `TransferApi` and satisfy native factory lifetime,
/// callback, ownership and concurrency requirements.
pub unsafe fn register_transfer(
    runtime: &crate::RuntimeClient,
    service: &str,
    factory: &crate::abi::NativeServiceFactory,
) -> crate::ModuleResult<()> {
    unsafe { runtime.register_native_typed(service, TRANSFER_CONTRACT, factory) }
}

#[cfg(test)]
mod tests;

use abi::{Description, Limits, SINK, SOURCE, Schema};
use amaterasu_abi::{STATUS_INVALID_ARGUMENT, Status};

pub fn validate_limits(limits: Limits) -> Result<(), Status> {
    if limits.max_chunk_bytes == 0
        || limits.max_chunk_bytes > isize::MAX as u64
        || limits.max_queued_bytes < limits.max_chunk_bytes
        || limits.max_record_bytes < limits.max_chunk_bytes
        || limits.max_queued_chunks == 0
        || limits.max_segments == 0
        || limits.max_segments > abi::MAX_SEGMENTS
        || limits.max_metadata_bytes > abi::MAX_METADATA_BYTES
        || limits.reserved != 0
    {
        return Err(STATUS_INVALID_ARGUMENT);
    }
    Ok(())
}

/// Intersects numerical limits without guessing format/framing compatibility.
/// The provider control service must agree the exact nonzero schemas separately.
pub fn negotiate_limits(provider: Limits, consumer: Limits) -> Result<Limits, Status> {
    validate_limits(provider)?;
    validate_limits(consumer)?;
    let negotiated = Limits {
        max_chunk_bytes: provider.max_chunk_bytes.min(consumer.max_chunk_bytes),
        max_queued_bytes: provider.max_queued_bytes.min(consumer.max_queued_bytes),
        max_record_bytes: provider.max_record_bytes.min(consumer.max_record_bytes),
        max_queued_chunks: provider.max_queued_chunks.min(consumer.max_queued_chunks),
        max_segments: provider.max_segments.min(consumer.max_segments),
        max_metadata_bytes: provider.max_metadata_bytes.min(consumer.max_metadata_bytes),
        reserved: 0,
    };
    validate_limits(negotiated)?;
    Ok(negotiated)
}

pub fn description(
    direction: u32,
    format: Schema,
    metadata_schema: Schema,
    limits: Limits,
) -> Result<Description, Status> {
    validate_limits(limits)?;
    if !matches!(direction, SOURCE | SINK)
        || [format, metadata_schema]
            .iter()
            .any(|id| id.namespace == 0 || id.value == 0 || id.version == 0)
    {
        return Err(STATUS_INVALID_ARGUMENT);
    }
    Ok(Description {
        size: size_of::<Description>() as u32,
        version: 1,
        direction,
        reserved: 0,
        format,
        metadata_schema,
        limits,
    })
}

#[cfg(test)]
mod duplex_conformance;
