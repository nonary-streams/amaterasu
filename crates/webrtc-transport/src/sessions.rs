//! Bounded, owner-scoped lifecycle and nonblocking UDP driving for WebRTC peers.
use std::collections::{BTreeMap, VecDeque};
use std::io;
use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

use amaterasu_network_policy::valid_peer;
use sha2::{Digest, Sha256};
use str0m::channel::ChannelId;
use str0m::net::{Protocol, Transmit};
use str0m::{Event, IceConnectionState, Output};

use super::{BrowserTransport, TransportError};

const MAX_SESSIONS: usize = 2;
const MAX_OPERATIONS: usize = 128;
const MAX_INPUT_BYTES: usize = 4096;
const MAX_EVENTS: usize = 64;
const MAX_TICK_ACTIONS: usize = 64;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);
const PEER_TIMEOUT: Duration = Duration::from_secs(45);

/// This value must come from a verified authentication context, not request
/// payload supplied by the browser. It is not a bearer credential.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct OwnerContext(String);

impl OwnerContext {
    pub fn from_verified_id(id: &str) -> Result<Self, SessionError> {
        if id.len() != 64
            || !id
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(SessionError::InvalidOwner);
        }
        Ok(Self(id.to_owned()))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionState {
    Connecting,
    Connected,
}

#[derive(Debug, PartialEq, Eq)]
pub enum SessionEvent {
    Connected,
    Input { binary: bool, bytes: Vec<u8> },
    InputClosed,
    KeyframeRequested,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionError {
    InvalidOwner,
    InvalidOperation,
    InvalidAddress,
    InvalidOffer,
    OperationConflict,
    SessionClosed,
    NotFound,
    Capacity,
    Randomness,
    Network,
    Transport,
    InputOverflow,
    UnexpectedChannel,
    Expired,
    ClockReversed,
}

impl std::fmt::Display for SessionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{self:?}")
    }
}
impl std::error::Error for SessionError {}

/// Contains signaling secrets; do not log this value. `created` lets a caller
/// undo a newly created session if completing the parent request fails.
pub struct OpenedSession {
    pub id: String,
    pub answer_sdp: String,
    pub created: bool,
}

struct OperationRecord {
    digest: [u8; 32],
    session_id: String,
}
struct Session {
    owner: OwnerContext,
    answer_sdp: String,
    peer: BrowserTransport,
    socket: UdpSocket,
    state: SessionState,
    started: Instant,
    last_packet: Instant,
    last_tick: Instant,
    deadline: Option<Instant>,
    pending: Option<Transmit>,
    channel: Option<ChannelId>,
    events: VecDeque<SessionEvent>,
}

pub struct BrowserSessionSet {
    bind_ip: IpAddr,
    allow_remote: bool,
    sessions: BTreeMap<String, Session>,
    operations: BTreeMap<(OwnerContext, String), OperationRecord>,
}

impl BrowserSessionSet {
    /// Only concrete unicast addresses are supported; remote use requires an
    /// explicit policy choice. This constructor does not open a socket.
    pub fn new(bind_ip: IpAddr, allow_remote: bool) -> Result<Self, SessionError> {
        if !valid_peer(bind_ip) || (!allow_remote && !bind_ip.is_loopback()) {
            return Err(SessionError::InvalidAddress);
        }
        Ok(Self {
            bind_ip,
            allow_remote,
            sessions: BTreeMap::new(),
            operations: BTreeMap::new(),
        })
    }

    /// Accept exactly one offer for an owner-scoped operation. Exact retries
    /// return the same session and answer; closed operation IDs are tombstones.
    pub fn open(
        &mut self,
        owner: &OwnerContext,
        operation: &str,
        sdp: &str,
        now: Instant,
    ) -> Result<OpenedSession, SessionError> {
        if operation.is_empty() || operation.len() > 128 || operation.chars().any(char::is_control)
        {
            return Err(SessionError::InvalidOperation);
        }
        if sdp.is_empty() || sdp.len() > 256 * 1024 {
            return Err(SessionError::InvalidOffer);
        }
        let digest: [u8; 32] = Sha256::digest(sdp.as_bytes()).into();
        let key = (owner.clone(), operation.to_owned());
        if let Some(previous) = self.operations.get(&key) {
            if previous.digest != digest {
                return Err(SessionError::OperationConflict);
            }
            let session = self
                .sessions
                .get(&previous.session_id)
                .ok_or(SessionError::SessionClosed)?;
            return Ok(OpenedSession {
                id: previous.session_id.clone(),
                answer_sdp: session.answer_sdp.clone(),
                created: false,
            });
        }
        if self.sessions.len() >= MAX_SESSIONS || self.operations.len() >= MAX_OPERATIONS {
            return Err(SessionError::Capacity);
        }
        let socket =
            UdpSocket::bind(SocketAddr::new(self.bind_ip, 0)).map_err(|_| SessionError::Network)?;
        socket
            .set_nonblocking(true)
            .map_err(|_| SessionError::Network)?;
        let mut peer =
            BrowserTransport::new(socket.local_addr().map_err(|_| SessionError::Network)?, now)
                .map_err(|_| SessionError::Transport)?;
        let answer = peer
            .accept_offer(sdp)
            .map_err(|_| SessionError::InvalidOffer)?;
        let id = self.random_id()?;
        let session = Session {
            owner: owner.clone(),
            answer_sdp: answer.clone(),
            peer,
            socket,
            state: SessionState::Connecting,
            started: now,
            last_packet: now,
            last_tick: now,
            deadline: None,
            pending: None,
            channel: None,
            events: VecDeque::new(),
        };
        self.sessions.insert(id.clone(), session);
        self.operations.insert(
            key,
            OperationRecord {
                digest,
                session_id: id.clone(),
            },
        );
        Ok(OpenedSession {
            id,
            answer_sdp: answer,
            created: true,
        })
    }

    pub fn state(&self, owner: &OwnerContext, id: &str) -> Result<SessionState, SessionError> {
        Ok(self.owned(owner, id)?.state)
    }

    /// Advance one peer with bounded nonblocking network work. An error drops
    /// its socket and transport; its operation remains a closed tombstone.
    pub fn tick(
        &mut self,
        owner: &OwnerContext,
        id: &str,
        now: Instant,
    ) -> Result<(), SessionError> {
        let remote = self.allow_remote;
        let result = self.owned_mut(owner, id)?.tick(now, remote);
        if result.is_err() {
            self.sessions.remove(id);
        }
        result
    }

    pub fn next_event(
        &mut self,
        owner: &OwnerContext,
        id: &str,
    ) -> Result<Option<SessionEvent>, SessionError> {
        Ok(self.owned_mut(owner, id)?.events.pop_front())
    }

    pub fn close(&mut self, owner: &OwnerContext, id: &str) -> Result<(), SessionError> {
        self.owned(owner, id)?;
        self.sessions.remove(id);
        Ok(())
    }

    /// Invoked by the embedding authority after a verified context is revoked.
    pub fn close_owner(&mut self, owner: &OwnerContext) {
        self.sessions.retain(|_, session| &session.owner != owner);
    }

    fn owned(&self, owner: &OwnerContext, id: &str) -> Result<&Session, SessionError> {
        self.sessions
            .get(id)
            .filter(|session| &session.owner == owner)
            .ok_or(SessionError::NotFound)
    }

    fn owned_mut(&mut self, owner: &OwnerContext, id: &str) -> Result<&mut Session, SessionError> {
        self.sessions
            .get_mut(id)
            .filter(|session| &session.owner == owner)
            .ok_or(SessionError::NotFound)
    }

    fn random_id(&self) -> Result<String, SessionError> {
        for _ in 0..16 {
            let mut random = [0u8; 16];
            getrandom::fill(&mut random).map_err(|_| SessionError::Randomness)?;
            let id: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
            if !self
                .operations
                .values()
                .any(|operation| operation.session_id == id)
            {
                return Ok(id);
            }
        }
        Err(SessionError::Randomness)
    }
}

impl Session {
    fn tick(&mut self, now: Instant, allow_remote: bool) -> Result<(), SessionError> {
        if now < self.last_tick {
            return Err(SessionError::ClockReversed);
        }
        self.last_tick = now;
        let limit = if self.state == SessionState::Connecting {
            HANDSHAKE_TIMEOUT
        } else {
            PEER_TIMEOUT
        };
        let since = if self.state == SessionState::Connecting {
            self.started
        } else {
            self.last_packet
        };
        if now.saturating_duration_since(since) >= limit {
            return Err(SessionError::Expired);
        }
        let mut buffer = [0u8; 65_535];
        for _ in 0..MAX_TICK_ACTIONS {
            if let Some(packet) = self.pending.take() {
                if packet.source
                    != self
                        .socket
                        .local_addr()
                        .map_err(|_| SessionError::Network)?
                {
                    return Err(SessionError::Transport);
                }
                if packet.proto != Protocol::Udp
                    || !valid_peer(packet.destination.ip())
                    || (!allow_remote && !packet.destination.ip().is_loopback())
                {
                    continue;
                }
                match self.socket.send_to(&packet.contents, packet.destination) {
                    Ok(written) if written == packet.contents.len() => {}
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        self.pending = Some(packet);
                        break;
                    }
                    _ => return Err(SessionError::Network),
                }
                continue;
            }
            match self
                .peer
                .poll_output()
                .map_err(|_| SessionError::Transport)?
            {
                Output::Transmit(packet) => {
                    self.pending = Some(packet);
                    continue;
                }
                Output::Event(event) => {
                    self.handle_event(event)?;
                    continue;
                }
                Output::Timeout(deadline) => self.deadline = Some(deadline),
            }
            if self.deadline.is_some_and(|deadline| deadline <= now) {
                self.deadline = None;
                self.peer
                    .handle_timeout(now)
                    .map_err(|_| SessionError::Transport)?;
                continue;
            }
            match self.socket.recv_from(&mut buffer) {
                Ok((length, source))
                    if valid_peer(source.ip()) && (allow_remote || source.ip().is_loopback()) =>
                {
                    match self.peer.receive_datagram(now, source, &buffer[..length]) {
                        Ok(()) => self.last_packet = now,
                        Err(
                            TransportError::InvalidDatagram | TransportError::UnrelatedDatagram,
                        ) => {}
                        Err(_) => return Err(SessionError::Transport),
                    }
                }
                Ok(_) => {}
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) =>
                {
                    break;
                }
                Err(_) => return Err(SessionError::Network),
            }
        }
        Ok(())
    }

    fn handle_event(&mut self, event: Event) -> Result<(), SessionError> {
        match event {
            Event::Connected => {
                self.state = SessionState::Connected;
                self.push(SessionEvent::Connected)?;
            }
            Event::IceConnectionStateChange(IceConnectionState::Disconnected) => {
                return Err(SessionError::Expired);
            }
            Event::ChannelOpen(id, label) => {
                if label != "input" || self.channel.is_some() {
                    return Err(SessionError::UnexpectedChannel);
                }
                self.channel = Some(id);
            }
            Event::ChannelData(data) => {
                if self.channel != Some(data.id) {
                    return Err(SessionError::UnexpectedChannel);
                }
                if data.data.len() > MAX_INPUT_BYTES {
                    return Err(SessionError::InputOverflow);
                }
                self.push(SessionEvent::Input {
                    binary: data.binary,
                    bytes: data.data,
                })?;
            }
            Event::ChannelClose(id) if self.channel == Some(id) => {
                self.channel = None;
                self.push(SessionEvent::InputClosed)?;
            }
            Event::KeyframeRequest(_)
                if !self
                    .events
                    .iter()
                    .any(|event| matches!(event, SessionEvent::KeyframeRequested)) =>
            {
                self.push(SessionEvent::KeyframeRequested)?;
            }
            _ => {}
        }
        Ok(())
    }

    fn push(&mut self, event: SessionEvent) -> Result<(), SessionError> {
        if self.events.len() >= MAX_EVENTS {
            return Err(SessionError::InputOverflow);
        }
        self.events.push_back(event);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, UdpSocket};
    use std::sync::Arc;
    use str0m::Rtc;
    use str0m::media::{Direction, MediaKind};

    fn owner(character: char) -> OwnerContext {
        OwnerContext::from_verified_id(&character.to_string().repeat(64)).unwrap()
    }

    fn offer(now: Instant) -> String {
        let mut peer = Rtc::builder()
            .set_crypto_provider(Arc::new(str0m::crypto::from_feature_flags()))
            .build(now);
        let mut changes = peer.sdp_api();
        changes.add_media(MediaKind::Video, Direction::RecvOnly, None, None, None);
        changes.apply().unwrap().0.to_sdp_string()
    }

    #[test]
    fn retries_are_owner_scoped_and_closed_operations_cannot_reopen() {
        let now = Instant::now();
        let offer = offer(now);
        let alice = owner('a');
        let bob = owner('b');
        let mut sessions = BrowserSessionSet::new(Ipv4Addr::LOCALHOST.into(), false).unwrap();
        let first = sessions.open(&alice, "operation", &offer, now).unwrap();
        assert!(first.created);
        let retry = sessions.open(&alice, "operation", &offer, now).unwrap();
        assert!(!retry.created);
        assert_eq!(first.id, retry.id);
        assert_eq!(first.answer_sdp, retry.answer_sdp);
        assert!(matches!(
            sessions.open(&alice, "operation", "different", now),
            Err(SessionError::OperationConflict)
        ));
        assert_eq!(sessions.state(&bob, &first.id), Err(SessionError::NotFound));
        assert_eq!(sessions.close(&bob, &first.id), Err(SessionError::NotFound));
        let other = sessions.open(&bob, "operation", &offer, now).unwrap();
        assert!(matches!(
            sessions.open(&alice, "another", &offer, now),
            Err(SessionError::Capacity)
        ));
        sessions.close_owner(&alice);
        assert_eq!(
            sessions.state(&alice, &first.id),
            Err(SessionError::NotFound)
        );
        assert_eq!(
            sessions.state(&bob, &other.id),
            Ok(SessionState::Connecting)
        );
        assert!(matches!(
            sessions.open(&alice, "operation", &offer, now),
            Err(SessionError::SessionClosed)
        ));
    }

    #[test]
    fn timeout_and_clock_reversal_remove_only_the_affected_session() {
        let now = Instant::now();
        let offer = offer(now);
        let alice = owner('a');
        let bob = owner('b');
        let mut sessions = BrowserSessionSet::new(Ipv4Addr::LOCALHOST.into(), false).unwrap();
        let first = sessions.open(&alice, "first", &offer, now).unwrap();
        let other = sessions.open(&bob, "other", &offer, now).unwrap();
        assert_eq!(
            sessions.tick(&alice, &first.id, now + HANDSHAKE_TIMEOUT),
            Err(SessionError::Expired)
        );
        assert_eq!(
            sessions.state(&alice, &first.id),
            Err(SessionError::NotFound)
        );
        assert_eq!(
            sessions.state(&bob, &other.id),
            Ok(SessionState::Connecting)
        );
        assert_eq!(
            sessions.tick(&bob, &other.id, now - Duration::from_millis(1)),
            Err(SessionError::ClockReversed)
        );
        assert_eq!(sessions.state(&bob, &other.id), Err(SessionError::NotFound));
    }

    #[test]
    fn malformed_loopback_packets_do_not_end_the_session() {
        let now = Instant::now();
        let alice = owner('a');
        let mut sessions = BrowserSessionSet::new(Ipv4Addr::LOCALHOST.into(), false).unwrap();
        let opened = sessions.open(&alice, "first", &offer(now), now).unwrap();
        let destination = sessions.sessions[&opened.id].socket.local_addr().unwrap();
        UdpSocket::bind("127.0.0.1:0")
            .unwrap()
            .send_to(b"invalid", destination)
            .unwrap();
        sessions.tick(&alice, &opened.id, now).unwrap();
        assert_eq!(
            sessions.state(&alice, &opened.id),
            Ok(SessionState::Connecting)
        );
    }

    #[test]
    fn validates_owner_and_bind_policy_without_opening_remote_sockets() {
        assert!(matches!(
            OwnerContext::from_verified_id("bad"),
            Err(SessionError::InvalidOwner)
        ));
        assert!(matches!(
            OwnerContext::from_verified_id(&"A".repeat(64)),
            Err(SessionError::InvalidOwner)
        ));
        assert!(matches!(
            BrowserSessionSet::new(Ipv4Addr::UNSPECIFIED.into(), true),
            Err(SessionError::InvalidAddress)
        ));
        assert!(matches!(
            BrowserSessionSet::new(Ipv4Addr::new(192, 0, 2, 1).into(), false),
            Err(SessionError::InvalidAddress)
        ));
        assert!(BrowserSessionSet::new(Ipv4Addr::new(192, 0, 2, 1).into(), true).is_ok());
    }
}
