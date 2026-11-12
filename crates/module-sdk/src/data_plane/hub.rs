use super::{Channel, abi::*, native};
use amaterasu_abi::{NativeServiceFactory, STATUS_INTERNAL_ERROR, STATUS_INVALID_ARGUMENT, Status};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

struct Entry {
    owner: String,
    claimed: Option<u64>,
    closing: bool,
    channel: Channel,
}
struct State {
    entries: BTreeMap<ChannelId, Entry>,
    reserved_bytes: u64,
    next_channel: u64,
    next_binding: u64,
}
struct Shared {
    state: Mutex<State>,
    incarnation: u64,
    max_channels: usize,
    max_reserved_bytes: u64,
}

/// Provider-wide budget and ownership registry. Creation is for an already
/// authorized control operation; never copy an untrusted payload's owner field.
/// A channel reserves its full byte budget until close, even when empty. This
/// bounds aggregate retained bytes across all bindings and slow consumers.
#[derive(Clone)]
pub struct Hub(Arc<Shared>);
impl Hub {
    pub fn new(max_channels: usize, max_reserved_bytes: u64) -> Result<Self, Status> {
        if max_channels == 0 || max_reserved_bytes == 0 {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        let mut random = [0; 8];
        getrandom::fill(&mut random).map_err(|_| STATUS_INTERNAL_ERROR)?;
        let incarnation = u64::from_le_bytes(random);
        if incarnation == 0 {
            return Err(STATUS_INTERNAL_ERROR);
        }
        Ok(Self(Arc::new(Shared {
            state: Mutex::new(State {
                entries: BTreeMap::new(),
                reserved_bytes: 0,
                next_channel: 0,
                next_binding: 0,
            }),
            incarnation,
            max_channels,
            max_reserved_bytes,
        })))
    }
    /// The service's domain authorization and schema negotiation precede this.
    /// Returns a local producer/consumer handle and an opaque native channel ID.
    pub fn create(
        &self,
        owner: &str,
        description: Description,
    ) -> Result<(ChannelId, Channel), Status> {
        if !amaterasu_contracts::valid_name(owner)
            || description.size != size_of::<Description>() as u32
            || description.version != 1
            || description.reserved != 0
        {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        super::description(
            description.direction,
            description.format,
            description.metadata_schema,
            description.limits,
        )?;
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.entries.len() >= self.0.max_channels
            || description.limits.max_queued_bytes
                > self.0.max_reserved_bytes - state.reserved_bytes
        {
            return Err(BUSY);
        }
        let sequence = state.next_channel.checked_add(1).ok_or(CLOSED)?;
        state.next_channel = sequence;
        let id = ChannelId {
            incarnation: self.0.incarnation,
            sequence,
        };
        let channel = Channel::new(description);
        state.reserved_bytes += description.limits.max_queued_bytes;
        state.entries.insert(
            id,
            Entry {
                owner: owner.into(),
                claimed: None,
                closing: false,
                channel: channel.clone(),
            },
        );
        Ok((id, channel))
    }
    // Reject new native claims and signal channel waiters without a retirement
    // barrier. Only the pair owner uses this before closing both directions.
    pub(super) fn begin_close(&self, id: ChannelId) {
        let channel = {
            let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
            state.entries.get_mut(&id).map(|entry| {
                entry.closing = true;
                entry.channel.clone()
            })
        };
        if let Some(channel) = channel {
            channel.begin_cancel();
        }
    }
    /// Domain revocation/timeout calls close. Remove authority first; retire
    /// callbacks outside registry locks; return the reserved budget last.
    pub fn close(&self, id: ChannelId) {
        let channel = {
            let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
            state.entries.get_mut(&id).map(|entry| {
                entry.closing = true;
                entry.channel.clone()
            })
        };
        if let Some(channel) = channel {
            channel.cancel();
            let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.entries.remove(&id).is_some() {
                state.reserved_bytes -= channel.description().limits.max_queued_bytes;
            }
        }
    }
    /// Whether a channel still owns its reservation. Binding release removes it.
    pub fn contains(&self, id: ChannelId) -> bool {
        self.0
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entries
            .contains_key(&id)
    }
    /// Only provider destruction should call this, after stopping control admission.
    pub fn close_all(&self) {
        let ids: Vec<_> = self
            .0
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entries
            .keys()
            .copied()
            .collect();
        for id in ids {
            self.close(id);
        }
    }
    pub fn factory(&self) -> Box<NativeFactory> {
        NativeFactory::new(self.clone())
    }
    pub(super) fn new_binding(&self) -> Result<u64, Status> {
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        state.next_binding = state.next_binding.checked_add(1).ok_or(CLOSED)?;
        Ok(state.next_binding)
    }
    pub(super) fn channel(
        &self,
        owner: &str,
        binding: u64,
        id: ChannelId,
    ) -> Result<Channel, Status> {
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        let entry = state.entries.get_mut(&id).ok_or(CLOSED)?;
        if entry.owner != owner || entry.claimed.is_some_and(|id| id != binding) {
            return Err(DENIED);
        }
        if entry.closing {
            return Err(CLOSED);
        }
        entry.claimed = Some(binding);
        Ok(entry.channel.clone())
    }
    pub(super) fn release_binding(&self, binding: u64) {
        let ids: Vec<_> = self
            .0
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entries
            .iter()
            .filter_map(|(id, entry)| (entry.claimed == Some(binding)).then_some(*id))
            .collect();
        for id in ids {
            self.close(id);
        }
    }
}

/// Keep the boxed factory and provider DLL alive until the runtime stops and
/// all consumer bindings have been released. Register with `register_transfer`.
pub struct NativeFactory {
    pub api: NativeServiceFactory,
    pub(super) hub: Hub,
}
impl NativeFactory {
    fn new(hub: Hub) -> Box<Self> {
        let mut factory = Box::new(Self {
            api: NativeServiceFactory {
                header: amaterasu_abi::AbiHeader::of::<NativeServiceFactory>(),
                context: std::ptr::null_mut(),
                bind: Some(native::bind),
            },
            hub,
        });
        factory.api.context = (&mut *factory as *mut Self).cast();
        factory
    }
}
