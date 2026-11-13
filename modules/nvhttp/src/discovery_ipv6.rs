//! Scoped IPv6 DNS-SD for the active Moonlight HTTP listener.
use crate::discovery::{self, Advertisement, Target};
use amaterasu_module_sdk::{
    LogEmitter, LogLevel,
    network::{Network, UdpSocket},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    io,
    net::{IpAddr, Ipv6Addr, SocketAddr, SocketAddrV6},
    sync::mpsc::{self, Sender, TryRecvError},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const GROUP: Ipv6Addr = Ipv6Addr::new(0xff02, 0, 0, 0, 0, 0, 0, 0xfb);
const PORT: u16 = 5353;

enum Command {
    Update(Option<Target>),
    Stop,
}
pub(crate) struct DiscoveryIpv6 {
    sender: Sender<Command>,
    worker: Option<JoinHandle<()>>,
    target: Option<Target>,
}
impl DiscoveryIpv6 {
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
impl Drop for DiscoveryIpv6 {
    fn drop(&mut self) {
        self.stop();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Route {
    index: u32,
    advertisement: Advertisement,
}
fn routes(target: &Target, suffix: &str, interfaces: Vec<(IpAddr, u32)>) -> Vec<Route> {
    let listeners: Vec<_> = target
        .listeners
        .iter()
        .filter_map(|address| match address {
            SocketAddr::V6(address) if !address.ip().is_loopback() => Some(*address),
            _ => None,
        })
        .collect();
    let Some(port) = listeners.first().map(|address| address.port()) else {
        return Vec::new();
    };
    let mut grouped: BTreeMap<u32, Vec<Ipv6Addr>> = BTreeMap::new();
    for (ip, index) in interfaces {
        if let IpAddr::V6(ip) = ip
            && index != 0
            && !ip.is_loopback()
            && !ip.is_unspecified()
            && !ip.is_multicast()
            && listeners.iter().any(|bind| {
                (bind.ip().is_unspecified() || *bind.ip() == ip)
                    && (bind.scope_id() == 0 || bind.scope_id() == index)
            })
        {
            grouped.entry(index).or_default().push(ip);
        }
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
    grouped
        .into_iter()
        .map(|(index, mut addresses)| {
            addresses.sort();
            addresses.dedup();
            Route {
                index,
                advertisement: Advertisement {
                    instance: format!("{label}-{suffix}.{}", discovery::SERVICE),
                    host: format!("amaterasu-{suffix}.local"),
                    port,
                    addresses: vec![],
                    ipv6_addresses: addresses,
                },
            }
        })
        .collect()
}
fn multicast(socket: &UdpSocket, route: &Route, ttl: u32) {
    let destination = SocketAddr::V6(SocketAddrV6::new(GROUP, PORT, 0, route.index));
    let _ = socket.send_to(
        &discovery::packet(&route.advertisement, ttl, 0),
        destination,
    );
}
fn run(network: Network, receiver: mpsc::Receiver<Command>, log: LogEmitter) {
    let mut random = [0u8; 8];
    if getrandom::fill(&mut random).is_err() {
        let _ = log.emit(
            LogLevel::Warning,
            "nvhttp.discovery",
            "IPv6 discovery random identity unavailable",
        );
        return;
    }
    let suffix = random
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    let mut target: Option<Target> = None;
    let mut socket: Option<UdpSocket> = None;
    let mut active: Vec<Route> = Vec::new();
    let mut joined = BTreeSet::new();
    let mut next = Instant::now();
    loop {
        loop {
            match receiver.try_recv() {
                Ok(Command::Update(value)) => {
                    target = value;
                    next = Instant::now();
                }
                Ok(Command::Stop) | Err(TryRecvError::Disconnected) => {
                    if let Some(socket) = &socket {
                        for route in &active {
                            multicast(socket, route, 0);
                        }
                    }
                    return;
                }
                Err(TryRecvError::Empty) => break,
            }
        }
        if Instant::now() >= next {
            next = Instant::now() + Duration::from_secs(30);
            let desired = target
                .as_ref()
                .and_then(|target| {
                    network
                        .interface_addresses()
                        .ok()
                        .map(|interfaces| routes(target, &suffix, interfaces))
                })
                .unwrap_or_default();
            if !desired.is_empty() && socket.is_none() {
                match network
                    .bind_udp_shared("[::]:5353".parse().unwrap())
                    .and_then(|socket| {
                        socket.set_multicast_hops_v6(255)?;
                        Ok(socket)
                    }) {
                    Ok(value) => socket = Some(value),
                    Err(error) => {
                        let _ = log.emit(
                            LogLevel::Warning,
                            "nvhttp.discovery",
                            &format!("IPv6 mDNS listener unavailable: {error}"),
                        );
                    }
                }
            }
            if let Some(socket) = &socket {
                let desired_indices: BTreeSet<_> =
                    desired.iter().map(|route| route.index).collect();
                for route in &active {
                    if !desired.contains(route) {
                        multicast(socket, route, 0);
                    }
                }
                for index in joined
                    .difference(&desired_indices)
                    .copied()
                    .collect::<Vec<_>>()
                {
                    let _ = socket.leave_multicast_v6(&GROUP, index);
                    joined.remove(&index);
                }
                for index in desired_indices
                    .difference(&joined)
                    .copied()
                    .collect::<Vec<_>>()
                {
                    match socket.join_multicast_v6(&GROUP, index) {
                        Ok(()) => {
                            joined.insert(index);
                        }
                        Err(error) => {
                            let _ = log.emit(
                                LogLevel::Warning,
                                "nvhttp.discovery",
                                &format!("IPv6 mDNS interface {index} unavailable: {error}"),
                            );
                        }
                    }
                }
                active = desired
                    .into_iter()
                    .filter(|route| joined.contains(&route.index))
                    .collect();
                for route in &active {
                    multicast(socket, route, 120);
                }
            } else {
                active.clear();
            }
            if joined.is_empty() {
                socket = None;
            }
        }
        if let Some(receive_socket) = &socket {
            let mut bytes = [0; 9000];
            for _ in 0..32 {
                match receive_socket.recv_from(&mut bytes) {
                    Ok((size, peer)) => {
                        for route in &active {
                            if let SocketAddr::V6(peer) = peer
                                && peer.scope_id() != 0
                                && peer.scope_id() != route.index
                            {
                                continue;
                            }
                            if let Some((id, unicast)) =
                                discovery::query(&bytes[..size], &route.advertisement)
                            {
                                if peer.port() != PORT || unicast {
                                    let _ = receive_socket.send_to(
                                        &discovery::packet(
                                            &route.advertisement,
                                            120,
                                            if peer.port() != PORT { id } else { 0 },
                                        ),
                                        peer,
                                    );
                                    break;
                                }
                                multicast(receive_socket, route, 120);
                            }
                        }
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                    Err(_) => {
                        socket = None;
                        active.clear();
                        joined.clear();
                        next = Instant::now() + Duration::from_secs(5);
                        break;
                    }
                }
            }
        }
        thread::sleep(Duration::from_millis(100));
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ipv6_routes_follow_listeners_and_are_scoped_per_interface() {
        let target = Target {
            hostname: "PC".into(),
            listeners: vec!["[::]:48050".parse().unwrap()],
        };
        let interfaces = vec![
            ("fd00::10".parse().unwrap(), 2),
            ("fe80::10".parse().unwrap(), 2),
            ("fd00::20".parse().unwrap(), 3),
            ("::1".parse().unwrap(), 1),
        ];
        let all_routes = routes(&target, "123", interfaces.clone());
        assert_eq!(all_routes.len(), 2);
        assert_eq!(all_routes[0].index, 2);
        assert_eq!(all_routes[0].advertisement.port, 48050);
        assert_eq!(all_routes[0].advertisement.ipv6_addresses.len(), 2);
        assert_eq!(all_routes[1].advertisement.ipv6_addresses.len(), 1);
        let response = discovery::packet(&all_routes[0].advertisement, 120, 0);
        assert!(
            response
                .windows(16)
                .any(|value| value == "fd00::10".parse::<Ipv6Addr>().unwrap().octets())
        );
        assert!(
            response
                .windows(2)
                .any(|value| value == 48050u16.to_be_bytes())
        );
        let specific = Target {
            listeners: vec!["[fd00::20]:49000".parse().unwrap()],
            ..target
        };
        let routes = routes(&specific, "123", interfaces);
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].index, 3);
        assert_eq!(routes[0].advertisement.port, 49000);
    }
    #[test]
    #[ignore = "binds the shared IPv6 mDNS port and sends multicast on an active interface"]
    fn ipv6_socket_receives_scoped_multicast() {
        let network = Network::test_provider();
        let index = network
            .interface_addresses()
            .unwrap()
            .into_iter()
            .find_map(|(address, index)| match address {
                IpAddr::V6(ip) if !ip.is_loopback() && !ip.is_unspecified() && index != 0 => {
                    Some(index)
                }
                _ => None,
            })
            .expect("an active IPv6 interface is required");
        let socket = network
            .bind_udp_shared("[::]:5353".parse().unwrap())
            .unwrap();
        socket.join_multicast_v6(&GROUP, index).unwrap();
        socket.set_multicast_loop_v6(true).unwrap();
        let target = Target {
            hostname: "IPv6 Test".into(),
            listeners: vec!["[::]:48050".parse().unwrap()],
        };
        let route = routes(
            &target,
            "test123",
            vec![("fd00::1".parse().unwrap(), index)],
        )
        .pop()
        .unwrap();
        let bytes = discovery::packet(&route.advertisement, 0, 0);
        let destination = SocketAddr::V6(SocketAddrV6::new(GROUP, PORT, 0, index));
        socket.send_to(&bytes, destination).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut buffer = [0; 9000];
        loop {
            match socket.recv_from(&mut buffer) {
                Ok((size, _)) if buffer[..size] == bytes => break,
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                Err(error) => panic!("IPv6 mDNS receive failed: {error}"),
            }
            assert!(Instant::now() < deadline, "IPv6 multicast was not received");
            thread::sleep(Duration::from_millis(10));
        }
    }
}
