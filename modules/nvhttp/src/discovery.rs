//! Moonlight DNS-SD advertisements. Socket and interface work stays on a worker.
use amaterasu_module_sdk::network::{Network, UdpSocket};
use amaterasu_module_sdk::{LogEmitter, LogLevel};
use std::{
    io,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    sync::mpsc::{self, Sender, TryRecvError},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const GROUP: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 251);
const MDNS: SocketAddr = SocketAddr::new(IpAddr::V4(GROUP), 5353);
pub(super) const SERVICE: &str = "_nvstream._tcp.local";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Target {
    pub hostname: String,
    pub listeners: Vec<SocketAddr>,
}

enum Command {
    Update(Option<Target>),
    Stop,
}
pub(crate) struct Discovery {
    sender: Sender<Command>,
    worker: Option<JoinHandle<()>>,
    target: Option<Target>,
}
impl Discovery {
    pub fn start(network: Network, log: LogEmitter) -> Self {
        let (sender, receiver) = mpsc::channel();
        let worker = thread::spawn(move || run(network, receiver, log));
        Self {
            sender,
            worker: Some(worker),
            target: None,
        }
    }
    pub fn update(&mut self, target: Option<Target>) {
        if self.target != target {
            self.target = target.clone();
            let _ = self.sender.send(Command::Update(target));
        }
    }
    pub fn stop(&mut self) {
        let _ = self.sender.send(Command::Stop);
    }
    pub fn retired(&mut self) -> bool {
        if self.worker.as_ref().is_some_and(JoinHandle::is_finished)
            && let Some(worker) = self.worker.take()
        {
            let _ = worker.join();
        }
        self.worker.is_none()
    }
}
impl Drop for Discovery {
    fn drop(&mut self) {
        self.stop();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn run(network: Network, receiver: mpsc::Receiver<Command>, log: LogEmitter) {
    let mut random = [0u8; 8];
    if getrandom::fill(&mut random).is_err() {
        let _ = log.emit(
            LogLevel::Warning,
            "nvhttp.discovery",
            "discovery random identity unavailable",
        );
        return;
    }
    let suffix = random
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    let mut target: Option<Target> = None;
    let mut socket: Option<UdpSocket> = None;
    let mut current: Option<Advertisement> = None;
    let mut next_refresh = Instant::now();
    loop {
        loop {
            match receiver.try_recv() {
                Ok(Command::Update(value)) => {
                    target = value;
                    next_refresh = Instant::now();
                }
                Ok(Command::Stop) | Err(TryRecvError::Disconnected) => {
                    if let (Some(socket), Some(ad)) = (&socket, &current) {
                        send(socket, ad, 0, MDNS, 0);
                    }
                    return;
                }
                Err(TryRecvError::Empty) => break,
            }
        }
        if Instant::now() >= next_refresh {
            next_refresh = Instant::now() + Duration::from_secs(30);
            if socket.is_none() && target.as_ref().is_some_and(has_remote_ipv4_listener) {
                match network
                    .bind_udp_shared("0.0.0.0:5353".parse().unwrap())
                    .and_then(|s| {
                        s.join_multicast_v4(&GROUP)?;
                        s.set_multicast_ttl_v4(255)?;
                        Ok(s)
                    }) {
                    Ok(s) => socket = Some(s),
                    Err(error) => {
                        let _ = log.emit(
                            LogLevel::Warning,
                            "nvhttp.discovery",
                            &format!("mDNS listener unavailable: {error}"),
                        );
                    }
                }
            }
            let next = target
                .as_ref()
                .and_then(|t| make_advertisement(&network, t, &suffix));
            if let (Some(s), Some(old)) = (&socket, &current)
                && next.as_ref() != Some(old)
            {
                send(s, old, 0, MDNS, 0);
            }
            current = next;
            if !target.as_ref().is_some_and(has_remote_ipv4_listener) {
                socket = None;
            }
            if let (Some(s), Some(ad)) = (&socket, &current) {
                send(s, ad, 120, MDNS, 0);
            }
        }
        if let (Some(s), Some(ad)) = (&socket, &current) {
            let mut buf = [0u8; 9000];
            for _ in 0..32 {
                match s.recv_from(&mut buf) {
                    Ok((n, peer)) => {
                        if let Some((id, unicast)) = query(&buf[..n], ad) {
                            let destination = if peer.port() != 5353 || unicast {
                                peer
                            } else {
                                MDNS
                            };
                            send(
                                s,
                                ad,
                                120,
                                destination,
                                if peer.port() != 5353 { id } else { 0 },
                            );
                        }
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                    Err(_) => {
                        socket = None;
                        next_refresh = Instant::now() + Duration::from_secs(5);
                        break;
                    }
                }
            }
        }
        thread::sleep(Duration::from_millis(100));
    }
}
fn has_remote_ipv4_listener(target: &Target) -> bool {
    target
        .listeners
        .iter()
        .any(|address| matches!(address.ip(), IpAddr::V4(ip) if !ip.is_loopback()))
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Advertisement {
    pub(super) instance: String,
    pub(super) host: String,
    pub(super) port: u16,
    pub(super) addresses: Vec<Ipv4Addr>,
    pub(super) ipv6_addresses: Vec<Ipv6Addr>,
}
fn make_advertisement(network: &Network, target: &Target, suffix: &str) -> Option<Advertisement> {
    advertisement(target, suffix, network.interface_addresses().ok()?)
}
fn advertisement(
    target: &Target,
    suffix: &str,
    interfaces: Vec<(IpAddr, u32)>,
) -> Option<Advertisement> {
    let listeners: Vec<_> = target
        .listeners
        .iter()
        .filter_map(|a| match a.ip() {
            IpAddr::V4(ip) if !ip.is_loopback() => Some((ip, a.port())),
            _ => None,
        })
        .collect();
    let port = listeners.first()?.1;
    let mut addresses: Vec<_> = interfaces
        .into_iter()
        .filter_map(|(ip, _)| match ip {
            IpAddr::V4(ip)
                if !ip.is_loopback()
                    && !ip.is_unspecified()
                    && !ip.is_link_local()
                    && listeners
                        .iter()
                        .any(|(bind, _)| bind.is_unspecified() || *bind == ip) =>
            {
                Some(ip)
            }
            _ => None,
        })
        .collect();
    addresses.sort();
    addresses.dedup();
    if addresses.is_empty() {
        return None;
    }
    let mut label: String = target
        .hostname
        .chars()
        .filter(|c| !c.is_control() && *c != '.')
        .take(40)
        .collect();
    while label.len() > 45 {
        label.pop();
    }
    if label.is_empty() {
        label = "Amaterasu".into();
    }
    Some(Advertisement {
        instance: format!("{label}-{suffix}.{SERVICE}"),
        host: format!("amaterasu-{suffix}.local"),
        port,
        addresses,
        ipv6_addresses: Vec::new(),
    })
}
fn name(out: &mut Vec<u8>, value: &str) {
    for label in value.split('.') {
        out.push(label.len() as u8);
        out.extend_from_slice(label.as_bytes());
    }
    out.push(0);
}
fn record(out: &mut Vec<u8>, owner: &str, kind: u16, class: u16, ttl: u32, data: &[u8]) {
    name(out, owner);
    out.extend_from_slice(&kind.to_be_bytes());
    out.extend_from_slice(&class.to_be_bytes());
    out.extend_from_slice(&ttl.to_be_bytes());
    out.extend_from_slice(&(data.len() as u16).to_be_bytes());
    out.extend_from_slice(data);
}
pub(super) fn packet(ad: &Advertisement, ttl: u32, id: u16) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&id.to_be_bytes());
    out.extend_from_slice(&0x8400u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(
        &(3u16 + ad.addresses.len() as u16 + ad.ipv6_addresses.len() as u16).to_be_bytes(),
    );
    out.extend_from_slice(&[0; 4]);
    let mut value = Vec::new();
    name(&mut value, &ad.instance);
    record(&mut out, SERVICE, 12, 1, ttl, &value);
    value.clear();
    value.extend_from_slice(&[0, 0, 0, 0]);
    value.extend_from_slice(&ad.port.to_be_bytes());
    name(&mut value, &ad.host);
    record(&mut out, &ad.instance, 33, 0x8001, ttl, &value);
    record(&mut out, &ad.instance, 16, 0x8001, ttl, &[0]);
    for ip in &ad.addresses {
        record(&mut out, &ad.host, 1, 0x8001, ttl, &ip.octets());
    }
    for ip in &ad.ipv6_addresses {
        record(&mut out, &ad.host, 28, 0x8001, ttl, &ip.octets());
    }
    out
}
fn send(socket: &UdpSocket, ad: &Advertisement, ttl: u32, destination: SocketAddr, id: u16) {
    let _ = socket.send_to(&packet(ad, ttl, id), destination);
}
fn read_name(data: &[u8], cursor: &mut usize) -> Option<String> {
    let mut position = *cursor;
    let mut end = None;
    let mut labels = Vec::new();
    for _ in 0..128 {
        let len = *data.get(position)?;
        position += 1;
        if len & 0xc0 == 0xc0 {
            let low = *data.get(position)?;
            position += 1;
            end.get_or_insert(position);
            position = (((len & 0x3f) as usize) << 8) | low as usize;
            continue;
        }
        if len & 0xc0 != 0 {
            return None;
        }
        if len == 0 {
            *cursor = end.unwrap_or(position);
            return Some(labels.join("."));
        }
        let bytes = data.get(position..position + len as usize)?;
        labels.push(std::str::from_utf8(bytes).ok()?.to_owned());
        position += len as usize;
    }
    None
}
pub(super) fn query(data: &[u8], ad: &Advertisement) -> Option<(u16, bool)> {
    if data.len() < 12 || data[2] & 0x80 != 0 {
        return None;
    }
    let count = u16::from_be_bytes([data[4], data[5]]);
    if count > 64 {
        return None;
    }
    let mut pos = 12;
    for _ in 0..count {
        let owner = read_name(data, &mut pos)?;
        let q = data.get(pos..pos + 4)?;
        pos += 4;
        let kind = u16::from_be_bytes([q[0], q[1]]);
        let class = u16::from_be_bytes([q[2], q[3]]);
        if class & 0x7fff == 1
            && (kind == 255
                || (owner.eq_ignore_ascii_case(SERVICE) && kind == 12)
                || (owner.eq_ignore_ascii_case(&ad.instance) && (kind == 33 || kind == 16))
                || (owner.eq_ignore_ascii_case(&ad.host) && (kind == 1 || kind == 28)))
            && (owner.eq_ignore_ascii_case(SERVICE)
                || owner.eq_ignore_ascii_case(&ad.instance)
                || owner.eq_ignore_ascii_case(&ad.host))
        {
            return Some((u16::from_be_bytes([data[0], data[1]]), class & 0x8000 != 0));
        }
    }
    None
}
#[cfg(test)]
mod tests {
    use super::*;
    fn ad(port: u16) -> Advertisement {
        Advertisement {
            instance: format!("PC-123.{SERVICE}"),
            host: "amaterasu-123.local".into(),
            port,
            addresses: vec![Ipv4Addr::new(192, 168, 1, 2)],
            ipv6_addresses: vec![],
        }
    }
    #[test]
    fn advertises_the_actual_custom_port_and_goodbye() {
        let bytes = packet(&ad(50000), 120, 0);
        let goodbye = packet(&ad(50000), 0, 0);
        for (bytes, expected_ttl) in [(&bytes, 120u32), (&goodbye, 0)] {
            assert_eq!(&bytes[2..4], &[0x84, 0]);
            assert_eq!(u16::from_be_bytes([bytes[6], bytes[7]]), 4);
            let mut cursor = 12;
            let mut srv_port = None;
            for _ in 0..4 {
                let _owner = read_name(bytes, &mut cursor).unwrap();
                let kind = u16::from_be_bytes([bytes[cursor], bytes[cursor + 1]]);
                let ttl = u32::from_be_bytes(bytes[cursor + 4..cursor + 8].try_into().unwrap());
                let len = u16::from_be_bytes([bytes[cursor + 8], bytes[cursor + 9]]) as usize;
                cursor += 10;
                assert_eq!(ttl, expected_ttl);
                if kind == 33 {
                    srv_port = Some(u16::from_be_bytes([bytes[cursor + 4], bytes[cursor + 5]]));
                }
                cursor += len;
            }
            assert_eq!(srv_port, Some(50000));
            assert_eq!(cursor, bytes.len());
        }
    }
    #[test]
    fn answers_discovery_and_rejects_bad_queries() {
        let mut q = vec![0; 12];
        q[5] = 1;
        name(&mut q, SERVICE);
        q.extend_from_slice(&[0, 12, 0, 1]);
        assert_eq!(query(&q, &ad(50000)), Some((0, false)));
        q.truncate(15);
        assert_eq!(query(&q, &ad(50000)), None);
    }
    #[test]
    fn loopback_only_listeners_are_never_advertised() {
        let target = Target {
            hostname: "PC".into(),
            listeners: vec!["127.0.0.1:50000".parse().unwrap()],
        };
        assert!(!has_remote_ipv4_listener(&target));
        let target = Target {
            listeners: vec!["0.0.0.0:50000".parse().unwrap()],
            ..target
        };
        assert!(has_remote_ipv4_listener(&target));
    }
    #[test]
    fn advertisement_tracks_the_actual_listener_and_restricts_addresses() {
        let mut target = Target {
            hostname: "Gaming PC".into(),
            listeners: vec!["192.168.1.10:50000".parse().unwrap()],
        };
        let interfaces = vec![
            ("192.168.1.10".parse().unwrap(), 1),
            ("10.0.0.4".parse().unwrap(), 2),
            ("127.0.0.1".parse().unwrap(), 3),
        ];
        let first = advertisement(&target, "0123", interfaces.clone()).unwrap();
        assert_eq!(first.port, 50000);
        assert_eq!(first.addresses, [Ipv4Addr::new(192, 168, 1, 10)]);
        target.listeners[0].set_port(51000);
        let second = advertisement(&target, "0123", interfaces).unwrap();
        assert_eq!(second.port, 51000);
        assert_ne!(packet(&first, 120, 0), packet(&second, 120, 0));
    }
    #[test]
    #[ignore = "binds the shared mDNS port and sends a multicast query on the local network"]
    fn shared_mdns_socket_receives_a_real_multicast_query() {
        let network = Network::test_provider();
        let socket = network
            .bind_udp_shared("0.0.0.0:5353".parse().unwrap())
            .unwrap();
        socket.join_multicast_v4(&GROUP).unwrap();
        socket.set_multicast_loop_v4(true).unwrap();
        let mut request = vec![0; 12];
        request[5] = 1;
        name(&mut request, SERVICE);
        request.extend_from_slice(&[0, 12, 0, 1]);
        socket.send_to(&request, MDNS).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut buffer = [0; 9000];
        loop {
            match socket.recv_from(&mut buffer) {
                Ok((size, _)) if buffer[..size] == request => break,
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                Err(error) => panic!("mDNS receive failed: {error}"),
            }
            assert!(
                Instant::now() < deadline,
                "multicast query was not received"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }
}
