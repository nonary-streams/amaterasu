//! Revision-pinned artwork over the shared binary-transfer ABI.
//!
//! Control replies contain only metadata and an owner-bound channel descriptor.
//! The provider retains bounded immutable bytes until binding release or expiry.
use crate::data_plane::{self, Hub, NativeFactory, OwnedChunk, TransferClient, abi::*};
use crate::{ModuleResult, RuntimeClient};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

pub const MAX_BYTES: usize = 16 * 1024 * 1024;
pub const CHUNK_BYTES: usize = 256 * 1024;
const MAX_CHANNELS: usize = 4;
/// Schema for one immutable artwork byte record.
pub const ARTWORK_FORMAT: Schema = Schema {
    namespace: 0x616d615f617274,
    value: 1,
    version: 1,
};
/// Reserved empty metadata schema; revision and MIME are pinned by the offer.
pub const ARTWORK_METADATA: Schema = Schema {
    namespace: 0x616d615f617274,
    value: 2,
    version: 1,
};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Offer {
    pub revision: String,
    pub mime_type: String,
    pub total_bytes: usize,
    pub channel: ChannelId,
    pub description: Description,
}
impl Offer {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.revision.is_empty()
            || self.revision.len() > 128
            || self.revision.chars().any(char::is_control)
            || !matches!(
                self.mime_type.as_str(),
                "image/png" | "image/jpeg" | "image/webp"
            )
            || self.total_bytes == 0
            || self.total_bytes > MAX_BYTES
            || self.channel.incarnation == 0
            || self.channel.sequence == 0
            || self.description.direction != SOURCE
            || self.description.format != ARTWORK_FORMAT
            || self.description.metadata_schema != ARTWORK_METADATA
            || self.description.version != 1
            || self.description.size != size_of::<Description>() as u32
            || self.description.reserved != 0
            || data_plane::validate_limits(self.description.limits).is_err()
            || self.description.limits.max_chunk_bytes > CHUNK_BYTES as u64
            || self.description.limits.max_queued_bytes > MAX_BYTES as u64
            || self.description.limits.max_record_bytes > MAX_BYTES as u64
            || self.description.limits.max_record_bytes < self.total_bytes as u64
            || self.description.limits.max_queued_bytes < self.total_bytes as u64
            || self.description.limits.max_metadata_bytes != 0
            || self.description.limits.max_queued_chunks > 1024
        {
            return Err("invalid_artwork_offer");
        }
        Ok(())
    }
}
fn description(chunk_bytes: usize) -> Description {
    data_plane::description(
        SOURCE,
        ARTWORK_FORMAT,
        ARTWORK_METADATA,
        Limits {
            max_chunk_bytes: chunk_bytes as u64,
            max_queued_bytes: MAX_BYTES as u64,
            max_record_bytes: MAX_BYTES as u64,
            max_queued_chunks: MAX_BYTES.div_ceil(chunk_bytes) as u32,
            max_segments: 1,
            max_metadata_bytes: 0,
            reserved: 0,
        },
    )
    .expect("valid artwork transfer limits")
}

pub struct Source {
    hub: Hub,
    description: Description,
    factory: Box<NativeFactory>,
    expires: BTreeMap<ChannelId, Instant>,
}
impl Source {
    pub fn new() -> ModuleResult<Self> {
        Self::with_chunk_bytes(CHUNK_BYTES)
    }
    pub fn with_chunk_bytes(chunk_bytes: usize) -> ModuleResult<Self> {
        if !(16 * 1024..=CHUNK_BYTES).contains(&chunk_bytes) {
            return Err("invalid artwork chunk budget".into());
        }
        let hub = Hub::new(MAX_CHANNELS, (MAX_CHANNELS * MAX_BYTES) as u64)
            .map_err(|status| format!("artwork hub: {status}"))?;
        let factory = hub.factory();
        Ok(Self {
            hub,
            description: description(chunk_bytes),
            factory,
            expires: BTreeMap::new(),
        })
    }
    /// # Safety
    /// This source and its module must outlive registered consumers. The host
    /// destroys consumers before providers according to startup dependencies.
    pub unsafe fn register(&self, runtime: &RuntimeClient, service: &str) -> ModuleResult<()> {
        unsafe { data_plane::register_transfer(runtime, service, &self.factory.api) }
    }
    /// Revoke all outstanding offers and synchronously retire their callbacks.
    pub fn clear(&mut self) {
        self.hub.close_all();
        self.expires.clear();
    }
    pub fn reap(&mut self) {
        let now = Instant::now();
        self.expires.retain(|id, deadline| {
            if now >= *deadline || !self.hub.contains(*id) {
                self.hub.close(*id);
                false
            } else {
                true
            }
        });
    }
    pub fn publish(
        &mut self,
        owner: &str,
        revision: String,
        mime_type: String,
        bytes: Vec<u8>,
    ) -> Result<Offer, &'static str> {
        self.reap();
        let (channel, queue) = self
            .hub
            .create(owner, self.description)
            .map_err(|_| "artwork_busy")?;
        let offer = Offer {
            revision,
            mime_type,
            total_bytes: bytes.len(),
            channel,
            description: self.description,
        };
        if let Err(error) = offer.validate() {
            self.hub.close(channel);
            return Err(error);
        }
        let total = bytes.len() as u64;
        for (index, bytes) in bytes
            .chunks(self.description.limits.max_chunk_bytes as usize)
            .enumerate()
        {
            let mut chunk = OwnedChunk::with_bytes(1, bytes.to_vec());
            chunk.offset = index as u64 * self.description.limits.max_chunk_bytes;
            chunk.total_bytes = total;
            if queue.push(chunk).is_err() {
                self.hub.close(channel);
                return Err("artwork_busy");
            }
        }
        if queue.finish().is_err() {
            self.hub.close(channel);
            return Err("artwork_unavailable");
        }
        self.expires
            .insert(channel, Instant::now() + Duration::from_secs(30));
        Ok(offer)
    }
}
impl Drop for Source {
    fn drop(&mut self) {
        self.hub.close_all();
    }
}

pub struct Image {
    pub revision: String,
    pub mime_type: String,
    pub bytes: Vec<u8>,
}
/// Consume a fully published immutable record. No polling or external I/O occurs
/// in receive callbacks. Drop releases the binding on success and every error.
/// # Safety
/// The selected service must remain loaded until this call returns, as enforced
/// by the consumer's startup dependency on the provider.
pub unsafe fn receive(runtime: &RuntimeClient, service: &str, offer: Offer) -> ModuleResult<Image> {
    offer.validate()?;
    let client =
        unsafe { TransferClient::bind(runtime, service, offer.channel, offer.description)? };
    let mut bytes = Vec::with_capacity(offer.total_bytes);
    while bytes.len() < offer.total_bytes {
        let status = client.receive(|chunk| {
            let length: usize = chunk.segments.iter().map(|s| s.len()).sum();
            if chunk.record_id != 1
                || chunk.offset != bytes.len() as u64
                || chunk.total_bytes != offer.total_bytes as u64
                || !chunk.metadata.is_empty()
                || length == 0
                || length as u64 > offer.description.limits.max_chunk_bytes
                || length > offer.total_bytes - bytes.len()
            {
                return crate::abi::STATUS_INVALID_ARGUMENT;
            }
            for segment in chunk.segments {
                bytes.extend_from_slice(segment);
            }
            crate::abi::STATUS_OK
        });
        if status != crate::abi::STATUS_OK {
            return Err(format!("artwork transfer rejected: {status}"));
        }
    }
    let state = client
        .snapshot()
        .map_err(|status| format!("artwork snapshot: {status}"))?;
    if state.queued_bytes != 0 || state.phase != ENDED {
        return Err("incomplete artwork record".into());
    }
    Ok(Image {
        revision: offer.revision,
        mime_type: offer.mime_type,
        bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn abandoned_offers_expire_and_release_all_capacity() {
        let mut source = Source::new().unwrap();
        for _ in 0..MAX_CHANNELS {
            let offer = source
                .publish("consumer", "revision".into(), "image/png".into(), vec![1])
                .unwrap();
            source
                .expires
                .insert(offer.channel, Instant::now() - Duration::from_secs(1));
        }
        source.reap();
        assert!(source.expires.is_empty());
        for _ in 0..MAX_CHANNELS {
            assert!(
                source
                    .publish("consumer", "revision".into(), "image/png".into(), vec![1])
                    .is_ok()
            );
        }
    }
}
