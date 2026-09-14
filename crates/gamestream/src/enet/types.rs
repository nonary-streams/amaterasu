//! Wire values use host integer order except the deliberately opaque connect ID.

pub const FLAG_ACKNOWLEDGE: u8 = 0x80;
pub const FLAG_UNSEQUENCED: u8 = 0x40;
pub const CONTROL_CHANNEL: u8 = 255;
pub const UNCONNECTED_PEER: u16 = 4095;
pub const MAX_COMMANDS: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub mtu: usize,
    pub channels: u8,
    pub max_fragments: u32,
    pub max_message_bytes: u32,
    /// No on-wire flag identifies checksums; reject configured checksum profiles.
    pub checksum: bool,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            mtu: 4096,
            channels: 48,
            max_fragments: 1024,
            max_message_bytes: 65536,
            checksum: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    InvalidLimits,
    DatagramTooLarge,
    Truncated,
    UnsupportedCompression,
    UnsupportedChecksum,
    InvalidHeader,
    UnknownCommand,
    InvalidFlags,
    InvalidChannel,
    InvalidHandshake,
    InvalidFragment,
    TooManyCommands,
    EmptyDatagram,
    PayloadTooLarge,
}

impl std::fmt::Display for Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "ENet codec error: {self:?}")
    }
}
impl std::error::Error for Error {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub peer_id: u16,
    pub session_id: u8,
    pub sent_time: Option<u16>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Datagram {
    pub header: Header,
    pub commands: Vec<Command>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Command {
    pub channel_id: u8,
    pub reliable_sequence: u16,
    pub flags: u8,
    pub body: CommandBody,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommandBody {
    Acknowledge {
        received_sequence: u16,
        received_sent_time: u16,
    },
    Connect(Connect),
    VerifyConnect(VerifyConnect),
    Disconnect {
        data: u32,
    },
    Ping,
    SendReliable {
        data: Vec<u8>,
    },
    SendUnreliable {
        sequence: u16,
        data: Vec<u8>,
    },
    SendFragment(Fragment),
    SendUnsequenced {
        group: u16,
        data: Vec<u8>,
    },
    BandwidthLimit {
        incoming: u32,
        outgoing: u32,
    },
    ThrottleConfigure {
        interval: u32,
        acceleration: u32,
        deceleration: u32,
    },
    SendUnreliableFragment(Fragment),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Connect {
    pub outgoing_peer_id: u16,
    pub incoming_session_id: u8,
    pub outgoing_session_id: u8,
    pub mtu: u32,
    pub window_size: u32,
    pub channel_count: u32,
    pub incoming_bandwidth: u32,
    pub outgoing_bandwidth: u32,
    pub packet_throttle_interval: u32,
    pub packet_throttle_acceleration: u32,
    pub packet_throttle_deceleration: u32,
    /// ENet copies these four bytes without endian conversion. Echo them exactly.
    pub connect_id: [u8; 4],
    pub data: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifyConnect {
    pub outgoing_peer_id: u16,
    pub incoming_session_id: u8,
    pub outgoing_session_id: u8,
    pub mtu: u32,
    pub window_size: u32,
    pub channel_count: u32,
    pub incoming_bandwidth: u32,
    pub outgoing_bandwidth: u32,
    pub packet_throttle_interval: u32,
    pub packet_throttle_acceleration: u32,
    pub packet_throttle_deceleration: u32,
    pub connect_id: [u8; 4],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fragment {
    pub start_sequence: u16,
    pub fragment_count: u32,
    pub fragment_number: u32,
    pub total_length: u32,
    pub fragment_offset: u32,
    pub data: Vec<u8>,
}
