use crate::model::{
    DISCOVERY_TOKEN_BYTES, DiscoveryTokens, MAX_DATAGRAMS, MAX_QUEUE_BYTES, SENDS_PER_STREAM,
};
use std::{
    collections::VecDeque,
    io,
    net::{IpAddr, SocketAddr, UdpSocket},
    time::{Duration, Instant},
};

const DISCOVERY_DEADLINE: Duration = Duration::from_secs(5);
const RECEIVES_PER_LANE: usize = 16;
const SS_PING_BYTES: usize = DISCOVERY_TOKEN_BYTES + size_of::<u32>();
const MAX_UDP_DATAGRAM_BYTES: usize = u16::MAX as usize;
const MAX_VIDEO_BATCH_BYTES: usize = 64 * 1024;
const MAX_VIDEO_BATCH_PACKETS: usize = 64;

#[derive(Clone, Copy)]
pub enum Medium {
    Video,
    Audio,
}

struct Datagram {
    medium: Medium,
    video_frame: Option<u32>,
    bytes: Vec<u8>,
}

struct Lane {
    socket: UdpSocket,
    peer: Option<SocketAddr>,
    ping_payload: Option<[u8; DISCOVERY_TOKEN_BYTES]>,
    receive_buffer: Option<Box<[u8]>>,
}

impl Lane {
    fn fixed(local: SocketAddr, peer: SocketAddr) -> io::Result<Self> {
        let socket = bound_socket(local)?;
        socket.connect(peer)?;
        Ok(Self {
            socket,
            peer: Some(peer),
            ping_payload: None,
            receive_buffer: None,
        })
    }

    fn discovering(
        local: SocketAddr,
        ping_payload: [u8; DISCOVERY_TOKEN_BYTES],
    ) -> io::Result<Self> {
        Ok(Self {
            socket: bound_socket(local)?,
            peer: None,
            ping_payload: Some(ping_payload),
            // A full-size buffer makes oversized UDP payloads malformed on all
            // platforms instead of surfacing WSAEMSGSIZE on Windows.
            receive_buffer: Some(vec![0; MAX_UDP_DATAGRAM_BYTES].into_boxed_slice()),
        })
    }

    fn receive_ping(&mut self, peer_ip: IpAddr) -> io::Result<()> {
        if self.peer.is_some() {
            return Ok(());
        }
        let expected = self.ping_payload.expect("discovering lane has payload");
        let buffer = self
            .receive_buffer
            .as_mut()
            .expect("discovering lane has receive buffer");
        for _ in 0..RECEIVES_PER_LANE {
            match self.socket.recv_from(buffer) {
                Ok((length, source)) => {
                    if source.ip() != peer_ip
                        || length != SS_PING_BYTES
                        || buffer[..DISCOVERY_TOKEN_BYTES] != expected
                        || u32::from_be_bytes(
                            buffer[DISCOVERY_TOKEN_BYTES..SS_PING_BYTES]
                                .try_into()
                                .expect("SS_PING sequence bytes"),
                        ) == 0
                    {
                        continue;
                    }
                    self.socket.connect(source)?;
                    self.peer = Some(source);
                    self.ping_payload = None;
                    self.receive_buffer = None;
                    break;
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) =>
                {
                    break;
                }
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }
}

pub struct Delivery {
    video: Lane,
    audio: Lane,
    discovery_peer_ip: Option<IpAddr>,
    discovery_deadline: Option<Instant>,
    queue: VecDeque<Datagram>,
    bytes: usize,
    video_max_batch_size_kb: u8,
    last_progress: Instant,
    pub sent_datagrams: u64,
    pub discarded_datagrams: u64,
    pub failure: Option<&'static str>,
}

impl Delivery {
    pub fn bind(
        video_local: SocketAddr,
        audio_local: SocketAddr,
        video_peer: SocketAddr,
        audio_peer: SocketAddr,
    ) -> io::Result<Self> {
        Ok(Self::new(
            Lane::fixed(video_local, video_peer)?,
            Lane::fixed(audio_local, audio_peer)?,
            None,
        ))
    }

    pub fn discover(
        video_local: SocketAddr,
        audio_local: SocketAddr,
        peer_ip: IpAddr,
        tokens: DiscoveryTokens,
    ) -> io::Result<Self> {
        Ok(Self::new(
            Lane::discovering(video_local, tokens.video)?,
            Lane::discovering(audio_local, tokens.audio)?,
            Some(peer_ip),
        ))
    }

    fn new(video: Lane, audio: Lane, discovery_peer_ip: Option<IpAddr>) -> Self {
        let discovery_deadline = discovery_peer_ip.map(|_| Instant::now() + DISCOVERY_DEADLINE);
        Self {
            video,
            audio,
            discovery_peer_ip,
            discovery_deadline,
            queue: VecDeque::new(),
            bytes: 0,
            video_max_batch_size_kb: 64,
            last_progress: Instant::now(),
            sent_datagrams: 0,
            discarded_datagrams: 0,
            failure: None,
        }
    }

    pub fn addresses(&self) -> io::Result<(SocketAddr, SocketAddr)> {
        Ok((
            self.video.socket.local_addr()?,
            self.audio.socket.local_addr()?,
        ))
    }

    pub fn peer_port(&self, medium: Medium) -> Option<u16> {
        match medium {
            Medium::Video => self.video.peer,
            Medium::Audio => self.audio.peer,
        }
        .map(|peer| peer.port())
    }

    pub fn is_discovery(&self) -> bool {
        self.discovery_peer_ip.is_some()
    }

    pub fn ready(&self) -> bool {
        self.video.peer.is_some() && self.audio.peer.is_some()
    }

    pub fn can_queue(&self, count: usize, bytes: usize) -> bool {
        self.queue
            .len()
            .checked_add(count)
            .is_some_and(|total| total <= MAX_DATAGRAMS)
            && self
                .bytes
                .checked_add(bytes)
                .is_some_and(|total| total <= MAX_QUEUE_BYTES)
    }

    #[cfg(test)]
    pub fn push(&mut self, medium: Medium, packets: Vec<Vec<u8>>) {
        self.push_batch(medium, None, packets);
    }

    pub fn push_video(&mut self, frame: u32, packets: Vec<Vec<u8>>) {
        self.push_batch(Medium::Video, Some(frame), packets);
    }

    pub fn push_audio(&mut self, packets: Vec<Vec<u8>>) {
        self.push_batch(Medium::Audio, None, packets);
    }

    pub fn set_video_max_batch_size_kb(&mut self, value: u8) {
        self.video_max_batch_size_kb = match value {
            16 | 32 | 64 => value,
            _ => 64,
        };
    }

    fn push_batch(&mut self, medium: Medium, video_frame: Option<u32>, packets: Vec<Vec<u8>>) {
        if self.queue.is_empty() {
            self.last_progress = Instant::now();
        }
        for bytes in packets {
            self.bytes += bytes.len();
            self.queue.push_back(Datagram {
                medium,
                video_frame,
                bytes,
            });
        }
    }

    pub fn queued(&self) -> usize {
        self.queue.len()
    }
    pub fn queued_bytes(&self) -> usize {
        self.bytes
    }

    #[cfg(test)]
    pub fn tick(&mut self) {
        self.tick_with_limit(SENDS_PER_STREAM);
    }

    pub fn tick_with_limit(&mut self, send_limit: usize) -> usize {
        if self.failure.is_some() {
            return 0;
        }
        if !self.ready() {
            self.tick_discovery();
            if !self.ready() || self.failure.is_some() {
                return 0;
            }
        }
        if !self.queue.is_empty() && self.last_progress.elapsed() >= Duration::from_secs(5) {
            self.fail("udp_stalled");
            return 0;
        }
        let mut sent = 0;
        let mut audio_sent = 0;
        let mut active_video_batch: Option<(Option<u32>, usize, usize)> = None;
        while sent < send_limit.min(MAX_VIDEO_BATCH_PACKETS) {
            let Some(packet) = self.queue.front() else {
                break;
            };
            let (medium, video_frame, packet_bytes) =
                (packet.medium, packet.video_frame, packet.bytes.len());
            if matches!(medium, Medium::Video) {
                if active_video_batch.is_none_or(|(active_frame, _, _)| active_frame != video_frame)
                {
                    active_video_batch =
                        Some((video_frame, self.video_batch_packet_limit(packet_bytes), 0));
                }
                if active_video_batch
                    .is_some_and(|(_, batch_limit, batch_sent)| batch_sent >= batch_limit)
                {
                    break;
                }
            } else {
                active_video_batch = None;
                if audio_sent >= SENDS_PER_STREAM {
                    break;
                }
            }
            let socket = match packet.medium {
                Medium::Video => &self.video.socket,
                Medium::Audio => &self.audio.socket,
            };
            let result = socket.send(&packet.bytes);
            if !self.record_send(result) {
                break;
            }
            sent += 1;
            if matches!(medium, Medium::Video) {
                if let Some((_, _, batch_sent)) = &mut active_video_batch {
                    *batch_sent += 1;
                }
            } else {
                audio_sent += 1;
            }
        }
        sent
    }

    fn video_batch_packet_limit(&self, packet_bytes: usize) -> usize {
        let requested = usize::from(self.video_max_batch_size_kb) * 1024;
        let cap = requested.min(MAX_VIDEO_BATCH_BYTES);
        (cap / packet_bytes.max(1)).clamp(1, MAX_VIDEO_BATCH_PACKETS)
    }

    fn tick_discovery(&mut self) {
        let Some(peer_ip) = self.discovery_peer_ip else {
            return;
        };
        if self
            .discovery_deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            self.fail("udp_discovery_timeout");
            return;
        }
        if self.video.receive_ping(peer_ip).is_err() || self.audio.receive_ping(peer_ip).is_err() {
            self.fail("udp_discovery_failed");
            return;
        }
        if self.ready() {
            self.discovery_deadline = None;
        }
    }

    fn record_send(&mut self, result: io::Result<usize>) -> bool {
        match result {
            Ok(bytes)
                if self
                    .queue
                    .front()
                    .is_some_and(|packet| packet.bytes.len() == bytes) =>
            {
                self.bytes -= self.queue.pop_front().unwrap().bytes.len();
                self.sent_datagrams = self.sent_datagrams.saturating_add(1);
                self.last_progress = Instant::now();
                true
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) =>
            {
                false
            }
            _ => {
                self.fail("udp_send_failed");
                false
            }
        }
    }

    pub fn fail(&mut self, code: &'static str) {
        self.failure = Some(code);
        self.discarded_datagrams = self
            .discarded_datagrams
            .saturating_add(self.queue.len() as u64);
        self.queue.clear();
        self.bytes = 0;
    }
}

fn bound_socket(local: SocketAddr) -> io::Result<UdpSocket> {
    let socket = UdpSocket::bind(local)?;
    socket.set_nonblocking(true)?;
    Ok(socket)
}

#[cfg(test)]
mod tests;
