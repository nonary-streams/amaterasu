//! Independent binary provider. Synthetic assets/frames are not codec output.
use amaterasu_module_sdk::{
    Command, ManagedModule, ModuleResult, Response, RuntimeClient,
    abi::STATUS_OK,
    data_plane::{abi::*, *},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;

const SERVICE: &str = "example.binary/1";
const MAX_BYTES: u64 = 40 * 1024 * 1024;

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum Action {
    OpenSource { bytes: u64 },
    OpenSink {},
    Consume { channel: ChannelId },
    Close { channel: ChannelId },
}
struct Fixture {
    runtime: RuntimeClient,
    hub: Hub,
    _factory: Box<NativeFactory>,
    channels: BTreeMap<ChannelId, (String, Channel)>,
}
impl Fixture {
    fn open(
        &mut self,
        owner: &str,
        direction: u32,
        bytes: u64,
    ) -> Result<(Value, Option<ChannelId>), String> {
        if self.channels.len() >= 4 || bytes > MAX_BYTES || bytes == 0 {
            return Err("invalid_size_or_capacity".into());
        }
        let format = Schema {
            namespace: 0x7b3f_4a99_5983_a16f,
            value: 88,
            version: 3,
        };
        let description = description(
            direction,
            format,
            Schema {
                value: 89,
                ..format
            },
            Limits {
                max_chunk_bytes: MAX_BYTES,
                max_queued_bytes: MAX_BYTES,
                max_record_bytes: u64::MAX,
                max_queued_chunks: 16,
                max_segments: 8,
                max_metadata_bytes: 4096,
                reserved: 0,
            },
        )
        .map_err(|_| "invalid_limits")?;
        let (id, channel) = self
            .hub
            .create(owner, description)
            .map_err(|_| "channel_capacity")?;
        if direction == SOURCE
            && (channel
                .push(OwnedChunk::with_bytes(1, vec![0xa5; bytes as usize]))
                .is_err()
                || channel.finish().is_err())
        {
            self.hub.close(id);
            return Err("source_failed".into());
        }
        self.channels.insert(id, (owner.into(), channel));
        Ok((
            json!({"ok":true,"channel":id,"description":description}),
            Some(id),
        ))
    }
    fn dispatch(
        &mut self,
        owner: &str,
        payload: Value,
    ) -> Result<(Value, Option<ChannelId>), String> {
        let action: Action = serde_json::from_value(payload).map_err(|_| "invalid_request")?;
        match action {
            Action::OpenSource { bytes } => self.open(owner, SOURCE, bytes),
            Action::OpenSink {} => self.open(owner, SINK, 1),
            Action::Consume { channel: id } | Action::Close { channel: id } => {
                let (expected_owner, channel) = self.channels.get(&id).ok_or("unknown_channel")?;
                if expected_owner != owner {
                    return Err("forbidden".into());
                }
                if matches!(action, Action::Close { .. }) {
                    self.hub.close(id);
                    self.channels.remove(&id);
                    return Ok((json!({"ok":true}), None));
                }
                if channel.description().direction != SINK {
                    return Err("wrong_direction".into());
                }
                let mut length = 0usize;
                let mut valid = true;
                // SAFETY: callback borrows input only for validation. No pointer
                // or allocator ownership is transferred into the JSON reply.
                let status = unsafe {
                    channel.receive(|chunk| {
                        for segment in
                            std::slice::from_raw_parts(chunk.segments, chunk.segment_count as usize)
                        {
                            let bytes = std::slice::from_raw_parts(segment.data, segment.len);
                            valid &= bytes.iter().all(|byte| *byte == 0xa5);
                            length += bytes.len();
                        }
                        STATUS_OK
                    })
                };
                Ok((
                    json!({"ok":status==STATUS_OK,"status":status,"bytes":length,"valid":valid}),
                    None,
                ))
            }
        }
    }
}
impl ManagedModule for Fixture {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        let hub = Hub::new(4, 128 * 1024 * 1024).map_err(|status| format!("hub: {status}"))?;
        let factory = hub.factory();
        runtime.require_success(Command::Register {
            service: SERVICE.into(),
        })?;
        // SAFETY: boxed factory lives through module destruction; packaged
        // dependencies must destroy all consumers before this provider module.
        unsafe {
            amaterasu_module_sdk::data_plane::register_transfer(&runtime, SERVICE, &factory.api)?
        };
        Ok(Self {
            runtime,
            hub,
            _factory: factory,
            channels: BTreeMap::new(),
        })
    }
    fn tick(&mut self) -> ModuleResult<()> {
        self.channels
            .retain(|_, (_, channel)| channel.snapshot().phase != CANCELLED);
        let Response::Request {
            id,
            caller,
            service,
            payload,
            ..
        } = self.runtime.send(Command::Poll {})?
        else {
            return Ok(());
        };
        if !matches!(
            self.runtime.send(Command::CheckRequest { request: id })?,
            Response::Ok
        ) {
            return Ok(());
        }
        let (reply, created) = if service == SERVICE {
            self.dispatch(&caller, payload)
                .unwrap_or_else(|error| (json!({"ok":false,"error":error}), None))
        } else {
            (json!({"ok":false,"error":"invalid_service"}), None)
        };
        if !matches!(
            self.runtime.send(Command::Complete {
                request: id,
                payload: reply
            }),
            Ok(Response::Ok)
        ) && let Some(channel) = created
        {
            self.hub.close(channel);
            self.channels.remove(&channel);
        }
        Ok(())
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.hub.close_all();
    }
}
amaterasu_module_sdk::export_module!(Fixture);
