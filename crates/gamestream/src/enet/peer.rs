//! Bounded, socket-independent ENet server peer for an explicitly authorized endpoint.
mod incoming;
mod outgoing;
#[cfg(test)]
mod qualification;
#[cfg(test)]
mod tests;

use super::*;
use std::collections::{BTreeMap, VecDeque};

pub const MAX_MESSAGE_BYTES: usize = 65539;
const MAX_DATAGRAMS: usize = 128;
const MAX_QUEUE_BYTES: usize = 256 * 1024;
const MAX_EVENTS: usize = 256;
const MAX_EVENT_BYTES: usize = 1024 * 1024;
const MAX_PENDING: usize = 128;
const MAX_FRAGMENTS: usize = 128;
const RECEIVE_WINDOW: u16 = 128;

#[derive(Clone, Debug)]
pub struct Config {
    pub expected_connect_data: u32,
    pub channels: u8,
    pub mtu: usize,
    pub local_peer_id: u16,
    pub handshake_timeout_ms: u64,
    pub idle_timeout_ms: u64,
    pub retransmit_ms: u64,
    pub ping_interval_ms: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            expected_connect_data: 0,
            channels: 48,
            mtu: 1392,
            local_peer_id: 0,
            handshake_timeout_ms: 5000,
            idle_timeout_ms: 15000,
            retransmit_ms: 250,
            ping_interval_ms: 1000,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Listening,
    AwaitingVerifyAck,
    Connected,
    Disconnecting,
    Disconnected,
    TimedOut,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Delivery {
    Reliable,
    Unreliable,
    Unsequenced,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Connected {
        channels: u8,
        mtu: usize,
    },
    Receive {
        channel: u8,
        delivery: Delivery,
        data: Vec<u8>,
    },
    Disconnected {
        data: u32,
    },
    TimedOut,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PeerError {
    InvalidConfig,
    InvalidChannel,
    MessageTooLarge,
    Busy,
    NotConnected,
    InvalidPacket,
    ClockRegression,
    Unsupported,
}

impl std::fmt::Display for PeerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ENet peer error: {self:?}")
    }
}
impl std::error::Error for PeerError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReceiveStatus {
    pub associated: bool,
}

#[derive(Clone)]
struct Pending {
    command: Command,
    first_sent: u64,
    last_sent: u64,
    attempts: u8,
    sent_times: Vec<u16>,
}

#[derive(Clone, Default)]
struct Channel {
    received_reliable: u16,
    received_unreliable: u16,
    sent_reliable: u16,
    sent_unreliable: u16,
    reliable: BTreeMap<u16, incoming::Assembly>,
    unreliable: BTreeMap<(u16, u16), incoming::Assembly>,
}

/// All memory, including retransmission copies, is bounded. The caller must pin
/// the authorized UDP endpoint before feeding any subsequent associated packet.
#[derive(Clone)]
pub struct Peer {
    config: Config,
    state: State,
    now: u64,
    started: u64,
    last_received: u64,
    last_sent: u64,
    remote_peer: u16,
    incoming_session: u8,
    outgoing_session: u8,
    connect: Option<Connect>,
    mtu: usize,
    channels: Vec<Channel>,
    control_sequence: u16,
    unsequenced_sent: u16,
    unsequenced_latest: Option<u16>,
    unsequenced_seen: VecDeque<u16>,
    pending: Vec<Pending>,
    retry_cursor: usize,
    datagrams: VecDeque<Vec<u8>>,
    datagram_bytes: usize,
    events: VecDeque<Event>,
    event_bytes: usize,
}

impl Peer {
    pub fn new(config: Config, now_ms: u64) -> Result<Self, PeerError> {
        if config.channels == 0
            || config.channels > 48
            || !(576..=4096).contains(&config.mtu)
            || config.local_peer_id >= UNCONNECTED_PEER
            || config.retransmit_ms == 0
            || config.retransmit_ms > 2000
            || config.ping_interval_ms == 0
            || config.handshake_timeout_ms < config.retransmit_ms
            || config.idle_timeout_ms < config.ping_interval_ms
        {
            return Err(PeerError::InvalidConfig);
        }
        Ok(Self {
            mtu: config.mtu,
            channels: vec![Channel::default(); config.channels as usize],
            config,
            state: State::Listening,
            now: now_ms,
            started: now_ms,
            last_received: now_ms,
            last_sent: now_ms,
            remote_peer: UNCONNECTED_PEER,
            incoming_session: 0,
            outgoing_session: 0,
            connect: None,
            control_sequence: 0,
            unsequenced_sent: 0,
            unsequenced_latest: None,
            unsequenced_seen: VecDeque::new(),
            pending: Vec::new(),
            retry_cursor: 0,
            datagrams: VecDeque::new(),
            datagram_bytes: 0,
            events: VecDeque::new(),
            event_bytes: 0,
        })
    }

    pub fn state(&self) -> State {
        self.state
    }

    pub fn pop_datagram(&mut self) -> Option<Vec<u8>> {
        let bytes = self.datagrams.pop_front()?;
        self.datagram_bytes -= bytes.len();
        Some(bytes)
    }

    pub fn pop_event(&mut self) -> Option<Event> {
        let event = self.events.pop_front()?;
        if let Event::Receive { data, .. } = &event {
            self.event_bytes -= data.len();
        }
        Some(event)
    }

    fn clock(&mut self, now: u64) -> Result<(), PeerError> {
        if now < self.now {
            return Err(PeerError::ClockRegression);
        }
        self.now = now;
        Ok(())
    }

    fn limits(&self) -> Limits {
        Limits {
            mtu: self.mtu,
            channels: self.channels.len() as u8,
            max_fragments: MAX_FRAGMENTS as u32,
            max_message_bytes: MAX_MESSAGE_BYTES as u32,
            ..Limits::default()
        }
    }

    fn event(&mut self, event: Event) -> Result<(), PeerError> {
        let bytes = match &event {
            Event::Receive { data, .. } => data.len(),
            _ => 0,
        };
        if self.events.len() == MAX_EVENTS || self.event_bytes + bytes > MAX_EVENT_BYTES {
            return Err(PeerError::Busy);
        }
        self.event_bytes += bytes;
        self.events.push_back(event);
        Ok(())
    }

    fn terminal(&mut self, state: State, event: Event) -> Result<(), PeerError> {
        self.event(event)?;
        if state == State::TimedOut {
            self.datagrams.clear();
            self.datagram_bytes = 0;
        }
        self.state = state;
        self.pending.clear();
        for channel in &mut self.channels {
            channel.reliable.clear();
            channel.unreliable.clear();
        }
        Ok(())
    }
}
