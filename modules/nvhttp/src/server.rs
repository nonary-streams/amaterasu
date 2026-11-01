//! Single-threaded nonblocking sockets with bounded request and response buffers.
use crate::{
    duplex_owner::{DuplexOwner, OutputProgress},
    settings::Settings,
    wire::{Reply, Request},
};
use amaterasu_module_sdk::network::{
    Network, TcpListenOptions, TcpListener as NetworkTcpListener, TcpStream as NetworkTcpStream,
};
use rustls::{ServerConfig, ServerConnection};
#[cfg(test)]
use std::net::{Ipv4Addr, Ipv6Addr};
use std::{
    collections::BTreeMap,
    io::{self, Read, Write},
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::{Duration, Instant},
};

const MAX_CONNECTIONS: usize = 32;
const MAX_PER_PEER: usize = 8;
const MAX_REQUEST: usize = 32 * 1024;
const MAX_REPLY: usize = 16 * 1024 * 1024;
const IO_BUDGET: usize = 16 * 1024;
const IO_TIMEOUT: Duration = Duration::from_secs(10);
const ROUTE_TIMEOUT: Duration = Duration::from_secs(130);

pub struct Incoming {
    pub id: u64,
    pub request: Request,
    pub secure: bool,
    pub remote: IpAddr,
    pub peer_certificate: Option<Vec<u8>>,
}

pub struct Events {
    pub requests: Vec<Incoming>,
    pub closed: Vec<Closed>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Closed {
    pub id: u64,
    pub response_delivered: bool,
}

pub struct Server {
    network: Network,
    http: Vec<NetworkTcpListener>,
    https: Vec<NetworkTcpListener>,
    connections: BTreeMap<u64, Connection>,
    next_id: u64,
    configuration: Option<Arc<ServerConfig>>,
    tls_ready: bool,
}

impl Server {
    #[cfg(test)]
    pub fn bind(settings: &Settings) -> Result<Self, String> {
        Self::bind_with_network(Network::test_provider(), settings)
    }

    pub fn bind_with_network(network: Network, settings: &Settings) -> Result<Self, String> {
        let http = bind_listeners(
            &network,
            settings.resolved_http_binds().map_err(str::to_owned)?,
            "HTTP",
        )?;
        let https = bind_listeners(
            &network,
            settings.resolved_https_binds().map_err(str::to_owned)?,
            "HTTPS",
        )?;
        Ok(Self {
            network,
            http,
            https,
            connections: BTreeMap::new(),
            next_id: 0,
            configuration: None,
            tls_ready: false,
        })
    }

    pub fn http_address(&self) -> SocketAddr {
        self.http_addresses()[0]
    }
    pub fn https_address(&self) -> SocketAddr {
        self.https_addresses()[0]
    }

    pub fn http_addresses(&self) -> Vec<SocketAddr> {
        self.http
            .iter()
            .map(|listener| listener.local_addr().expect("bound HTTP listener"))
            .collect()
    }

    pub fn https_addresses(&self) -> Vec<SocketAddr> {
        self.https
            .iter()
            .map(|listener| listener.local_addr().expect("bound HTTPS listener"))
            .collect()
    }

    /// Rebinds the HTTP pair while retaining existing accepted connections.
    /// Both replacement sockets are prepared before either current listener is dropped.
    #[cfg(test)]
    pub fn rebind_port(&mut self, http_port: u16) -> Result<(), String> {
        crate::settings::validate_port(http_port).map_err(str::to_owned)?;
        let https_port = crate::settings::https_port(http_port);
        let old_http_addresses = self.http_addresses();
        let old_https_addresses = self.https_addresses();
        if old_http_addresses
            .iter()
            .all(|address| address.port() == http_port)
            && old_https_addresses
                .iter()
                .all(|address| address.port() == https_port)
        {
            return Ok(());
        }

        let http_addresses = old_http_addresses
            .iter()
            .map(|address| SocketAddr::new(address.ip(), http_port))
            .collect::<Vec<_>>();
        let https_addresses = old_https_addresses
            .iter()
            .map(|address| SocketAddr::new(address.ip(), https_port))
            .collect::<Vec<_>>();
        let http = replacement_listeners(
            &self.network,
            &http_addresses,
            &self.http,
            &self.https,
            "HTTP",
        )?;
        let https = replacement_listeners(
            &self.network,
            &https_addresses,
            &self.http,
            &self.https,
            "HTTPS",
        )?;

        self.http = http;
        self.https = https;
        Ok(())
    }

    /// Rebinds both protocols and all requested address families as one swap.
    /// Existing accepted connections keep their original sockets and state.
    pub fn rebind_settings(&mut self, settings: &Settings) -> Result<(), String> {
        settings.validate().map_err(str::to_owned)?;
        let http_addresses = settings.resolved_http_binds().map_err(str::to_owned)?;
        let https_addresses = settings.resolved_https_binds().map_err(str::to_owned)?;
        let old_http_addresses = self.http_addresses();
        let old_https_addresses = self.https_addresses();
        if old_http_addresses == http_addresses && old_https_addresses == https_addresses {
            return Ok(());
        }

        let preflight = (|| {
            let http = replacement_listeners(
                &self.network,
                &http_addresses,
                &self.http,
                &self.https,
                "HTTP",
            )?;
            let https = replacement_listeners(
                &self.network,
                &https_addresses,
                &self.http,
                &self.https,
                "HTTPS",
            )?;
            Ok::<_, String>((http, https))
        })();
        match preflight {
            Ok((http, https)) => {
                self.http = http;
                self.https = https;
                Ok(())
            }
            Err(preflight_error) => {
                if !listener_sets_overlap(
                    [&old_http_addresses, &old_https_addresses],
                    [&http_addresses, &https_addresses],
                ) {
                    Err(preflight_error)
                } else {
                    // Some operating systems reject a wildcard bind while a more
                    // specific old listener owns the same port. Release both old
                    // groups together, then either install the complete replacement
                    // or restore the complete old pair before returning.
                    drop(std::mem::take(&mut self.http));
                    drop(std::mem::take(&mut self.https));
                    let replacement = (|| {
                        let http = bind_listeners(&self.network, http_addresses, "HTTP")?;
                        let https = bind_listeners(&self.network, https_addresses, "HTTPS")?;
                        Ok::<_, String>((http, https))
                    })();
                    match replacement {
                        Ok((http, https)) => {
                            self.http = http;
                            self.https = https;
                            Ok(())
                        }
                        Err(replacement_error) => {
                            let restored = (|| {
                                let http = bind_listeners(
                                    &self.network,
                                    old_http_addresses,
                                    "HTTP rollback",
                                )?;
                                let https = bind_listeners(
                                    &self.network,
                                    old_https_addresses,
                                    "HTTPS rollback",
                                )?;
                                Ok::<_, String>((http, https))
                            })();
                            match restored {
                                Ok((http, https)) => {
                                    self.http = http;
                                    self.https = https;
                                    Err(replacement_error)
                                }
                                Err(rollback_error) => Err(format!(
                                    "listener replacement failed ({replacement_error}); rollback failed ({rollback_error}); preflight was ({preflight_error})"
                                )),
                            }
                        }
                    }
                }
            }
        }
    }

    pub fn connections(&self) -> usize {
        self.connections.len()
    }
    pub fn tls_ready(&self) -> bool {
        self.tls_ready
    }

    pub fn set_ready(&mut self, ready: bool) {
        self.tls_ready = ready && self.configuration.is_some();
    }

    pub fn replace_tls(&mut self, configuration: Arc<ServerConfig>) -> Vec<u64> {
        self.configuration = Some(configuration);
        self.tls_ready = true;
        let closed: Vec<_> = self
            .connections
            .iter()
            .filter(|(_, connection)| connection.tls.is_some())
            .map(|(id, _)| *id)
            .collect();
        for id in &closed {
            self.connections.remove(id);
        }
        closed
    }

    #[cfg(test)]
    pub fn tick(&mut self) -> Events {
        self.tick_checked(|_, _| true)
    }

    #[cfg(test)]
    pub fn tick_checked(
        &mut self,
        verify: impl FnMut(&[u8], Option<crate::wire::Permission>) -> bool,
    ) -> Events {
        self.tick_checked_with_upgrades(verify, |_, _| None)
    }

    // The selector comes only from trusted startup composition. No ordinary
    // route may opt itself in by returning a particular HTTP status.
    pub fn tick_checked_with_upgrades(
        &mut self,
        mut verify: impl FnMut(&[u8], Option<crate::wire::Permission>) -> bool,
        select_upgrade: impl Fn(&str, &str) -> Option<String>,
    ) -> Events {
        self.accept(false);
        self.accept(true);
        let mut events = Events {
            requests: Vec::new(),
            closed: Vec::new(),
        };
        let now = Instant::now();
        for (id, connection) in &mut self.connections {
            if now >= connection.deadline {
                events.closed.push(Closed {
                    id: *id,
                    response_delivered: false,
                });
                continue;
            }
            if let Some(upgrade) = connection
                .upgrade
                .as_mut()
                .or(connection.pending_upgrade.as_mut())
                && (!connection
                    .tls
                    .as_ref()
                    .and_then(|tls| tls.peer_certificates())
                    .and_then(|certificates| certificates.first())
                    .is_some_and(|certificate| {
                        verify(certificate.as_ref(), Some(upgrade.permission.clone()))
                    })
                    || upgrade.owner.check_channels().is_err())
            {
                events.closed.push(Closed {
                    id: *id,
                    response_delivered: false,
                });
                continue;
            }
            if let Some(output) = &connection.output
                && output.requires_paired_tls
                && !connection
                    .peer_certificate()
                    .is_some_and(|certificate| verify(&certificate, output.permission.clone()))
            {
                events.closed.push(Closed {
                    id: *id,
                    response_delivered: false,
                });
                continue;
            }
            match connection.tick(&mut verify, &select_upgrade) {
                Ok(Some(request)) => events.requests.push(Incoming {
                    id: *id,
                    request,
                    secure: connection.tls.is_some(),
                    remote: connection.remote,
                    peer_certificate: connection.peer_certificate(),
                }),
                Ok(None) if connection.finished => events.closed.push(Closed {
                    id: *id,
                    response_delivered: connection.response_delivered,
                }),
                Ok(None) => {}
                Err(_) => events.closed.push(Closed {
                    id: *id,
                    response_delivered: false,
                }),
            }
        }
        for closed in &events.closed {
            self.connections.remove(&closed.id);
        }
        events
    }

    fn accept(&mut self, secure: bool) {
        let listeners = if secure { &self.https } else { &self.http };
        for listener in listeners {
            for _ in 0..4 {
                let (socket, remote) = match listener.accept() {
                    Ok(value) => value,
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                    Err(_) => break,
                };
                let remote = normalize_ip(remote.ip());
                let peer_count = self
                    .connections
                    .values()
                    .filter(|connection| connection.remote == remote)
                    .count();
                if self.connections.len() >= MAX_CONNECTIONS
                    || peer_count >= MAX_PER_PEER
                    || (secure && !self.tls_ready)
                {
                    continue;
                }
                if socket.set_nonblocking(true).is_err() {
                    continue;
                }
                let _ = socket.set_nodelay(true);
                let tls = if secure {
                    let Some(configuration) = &self.configuration else {
                        continue;
                    };
                    let Ok(mut connection) = ServerConnection::new(configuration.clone()) else {
                        continue;
                    };
                    connection.set_buffer_limit(Some(MAX_REQUEST));
                    Some(connection)
                } else {
                    None
                };
                let Some(id) = self.next_id.checked_add(1) else {
                    break;
                };
                self.next_id = id;
                self.connections.insert(
                    id,
                    Connection {
                        socket,
                        remote,
                        tls,
                        input: Vec::new(),
                        output: None,
                        upgrade: None,
                        pending_upgrade: None,
                        upgrade_candidate: false,
                        peer_tls_closed: false,
                        waiting: false,
                        finished: false,
                        closing: false,
                        response_delivered: false,
                        deadline: Instant::now() + IO_TIMEOUT,
                    },
                );
            }
        }
    }

    pub fn respond_checked(
        &mut self,
        id: u64,
        reply: Reply,
        verify: impl FnOnce(&[u8], Option<crate::wire::Permission>) -> bool,
    ) -> bool {
        let paired = if reply.requires_paired_tls {
            self.connections
                .get(&id)
                .and_then(|connection| connection.tls.as_ref())
                .filter(|tls| !tls.is_handshaking())
                .and_then(|tls| tls.peer_certificates())
                .and_then(|certificates| certificates.first())
                .is_some_and(|certificate| verify(certificate.as_ref(), reply.permission.clone()))
        } else {
            false
        };
        let authorized = !reply.requires_paired_tls || paired;
        self.respond(id, reply.authorize(paired)) && authorized
    }

    /// Reserve the pair for a selected handler but keep the connection waiting
    /// until a distinct broker acceptance is received. Failure closes the socket.
    pub fn prepare_upgrade(
        &mut self,
        id: u64,
        entitlement: String,
        verify: impl FnOnce(&[u8], Option<crate::wire::Permission>) -> bool,
        build: impl FnOnce(Vec<u8>) -> Result<DuplexOwner, amaterasu_module_sdk::abi::Status>,
    ) -> Option<(
        amaterasu_module_sdk::data_plane::abi::ChannelId,
        amaterasu_module_sdk::data_plane::abi::ChannelId,
    )> {
        let connection = self.connections.get_mut(&id)?;
        let valid_entitlement = amaterasu_identity::valid_entitlement(&entitlement);
        let permission = crate::wire::Permission::Entitlement(entitlement);
        let certificate = connection
            .tls
            .as_ref()
            .filter(|tls| !tls.is_handshaking())
            .and_then(|tls| tls.peer_certificates())
            .and_then(|certs| certs.first())
            .map(|cert| cert.as_ref().to_vec());
        let valid = valid_entitlement
            && connection.waiting
            && connection.upgrade_candidate
            && connection.output.is_none()
            && connection.upgrade.is_none()
            && connection.pending_upgrade.is_none()
            && certificate
                .as_ref()
                .is_some_and(|cert| verify(cert, Some(permission.clone())));
        if !valid {
            self.connections.remove(&id);
            return None;
        }
        let owner = match build(std::mem::take(&mut connection.input)) {
            Ok(owner)
                if owner.binding().connection_id == id
                    && Some(owner.binding().peer_certificate.as_slice())
                        == certificate.as_deref() =>
            {
                owner
            }
            _ => {
                self.connections.remove(&id);
                return None;
            }
        };
        let ids = (owner.pair().input_id(), owner.pair().output_id());
        connection.pending_upgrade = Some(Upgrade {
            owner,
            permission,
            response_flushed: false,
            output_eof: false,
            output_close_sent: false,
            input_eof_observed: connection.peer_tls_closed,
            input_finished: false,
        });
        Some(ids)
    }

    /// Called only after a validated distinct acceptance from the approved
    /// broker method. Recheck the same peer and entitlement at activation.
    pub fn activate_upgrade(
        &mut self,
        id: u64,
        verify: impl FnOnce(&[u8], Option<crate::wire::Permission>) -> bool,
    ) -> bool {
        let Some(connection) = self.connections.get_mut(&id) else {
            return false;
        };
        let valid = connection.waiting
            && connection.upgrade_candidate
            && connection.pending_upgrade.as_ref().is_some_and(|pending| {
                connection
                    .tls
                    .as_ref()
                    .filter(|tls| !tls.is_handshaking())
                    .and_then(|tls| tls.peer_certificates())
                    .and_then(|certs| certs.first())
                    .is_some_and(|cert| {
                        cert.as_ref() == pending.owner.binding().peer_certificate.as_slice()
                            && verify(cert.as_ref(), Some(pending.permission.clone()))
                    })
            });
        if !valid {
            self.connections.remove(&id);
            return false;
        }
        connection.waiting = false;
        connection.upgrade_candidate = false;
        connection.upgrade = connection.pending_upgrade.take();
        connection.deadline = Instant::now() + ROUTE_TIMEOUT;
        true
    }

    pub fn respond(&mut self, id: u64, reply: Reply) -> bool {
        if let Some(connection) = self.connections.get_mut(&id) {
            drop(connection.pending_upgrade.take());
            connection.respond(reply);
            true
        } else {
            false
        }
    }

    pub fn close_all(&mut self) -> Vec<u64> {
        let ids = self.connections.keys().copied().collect();
        self.connections.clear();
        ids
    }

    pub fn close(&mut self, id: u64) -> bool {
        self.connections.remove(&id).is_some()
    }
}

fn bind_network_listener(
    network: &Network,
    address: SocketAddr,
    name: &str,
) -> Result<NetworkTcpListener, String> {
    network
        .bind_tcp_with_options(
            address,
            TcpListenOptions {
                backlog: 128,
                ..TcpListenOptions::default()
            },
        )
        .map_err(|_| format!("{name} listener unavailable"))
}

#[cfg(test)]
fn bind_listener(address: SocketAddr, name: &str) -> Result<NetworkTcpListener, String> {
    bind_network_listener(&Network::test_provider(), address, name)
}

fn bind_listeners(
    network: &Network,
    addresses: Vec<SocketAddr>,
    name: &str,
) -> Result<Vec<NetworkTcpListener>, String> {
    let mut listeners = Vec::with_capacity(addresses.len());
    let mut selected_port = None;
    for mut address in addresses {
        if address.port() == 0
            && let Some(port) = selected_port
        {
            address.set_port(port);
        }
        let listener = bind_network_listener(network, address, name)?;
        selected_port.get_or_insert_with(|| listener.local_addr().expect("bound listener").port());
        listeners.push(listener);
    }
    Ok(listeners)
}

fn replacement_listeners(
    network: &Network,
    addresses: &[SocketAddr],
    old_http: &[NetworkTcpListener],
    old_https: &[NetworkTcpListener],
    name: &str,
) -> Result<Vec<NetworkTcpListener>, String> {
    let mut replacements = Vec::with_capacity(addresses.len());
    for address in addresses {
        // A TCP listener is protocol-neutral; TLS/plain handling is chosen by
        // the accept call. Reusing a same-address listener for the other role is
        // safe while existing accepted streams retain their own connection state.
        let existing = old_http
            .iter()
            .chain(old_https)
            .find(|listener| listener.local_addr().ok() == Some(*address));
        let listener = match existing {
            Some(listener) => listener
                .try_clone()
                .map_err(|_| format!("{name} listener clone failed"))?,
            None => bind_network_listener(network, *address, name)?,
        };
        replacements.push(listener);
    }
    Ok(replacements)
}

fn listener_sets_overlap<const OLD: usize, const NEW: usize>(
    old_groups: [&[SocketAddr]; OLD],
    new_groups: [&[SocketAddr]; NEW],
) -> bool {
    let old_addresses = old_groups
        .iter()
        .flat_map(|group| group.iter())
        .collect::<Vec<_>>();
    new_groups
        .iter()
        .flat_map(|group| group.iter())
        .filter(|new| !old_addresses.contains(new))
        .any(|new| {
            old_addresses.iter().any(|old| {
                old.port() == new.port()
                    && old.is_ipv4() == new.is_ipv4()
                    && (old.ip() == new.ip()
                        || old.ip().is_unspecified()
                        || new.ip().is_unspecified())
            })
        })
}

fn normalize_ip(address: IpAddr) -> IpAddr {
    match address {
        IpAddr::V6(address) => address
            .to_ipv4_mapped()
            .map(IpAddr::V4)
            .unwrap_or(IpAddr::V6(address)),
        IpAddr::V4(address) => IpAddr::V4(address),
    }
}

struct Output {
    requires_paired_tls: bool,
    permission: Option<crate::wire::Permission>,
    bytes: Vec<u8>,
    offset: usize,
}
struct Upgrade {
    owner: DuplexOwner,
    permission: crate::wire::Permission,
    response_flushed: bool,
    output_eof: bool,
    output_close_sent: bool,
    input_eof_observed: bool,
    input_finished: bool,
}
struct Connection {
    socket: NetworkTcpStream,
    remote: IpAddr,
    tls: Option<ServerConnection>,
    input: Vec<u8>,
    output: Option<Output>,
    upgrade: Option<Upgrade>,
    pending_upgrade: Option<Upgrade>,
    upgrade_candidate: bool,
    peer_tls_closed: bool,
    waiting: bool,
    finished: bool,
    closing: bool,
    response_delivered: bool,
    deadline: Instant,
}

impl Connection {
    fn tick(
        &mut self,
        verify: &mut impl FnMut(&[u8], Option<crate::wire::Permission>) -> bool,
        select_upgrade: &impl Fn(&str, &str) -> Option<String>,
    ) -> io::Result<Option<Request>> {
        if let Some(mut upgrade) = self.upgrade.take() {
            let result = self.tick_upgrade(&mut upgrade, verify);
            if result.is_ok() && !self.finished {
                self.upgrade = Some(upgrade);
            } else {
                upgrade.owner.signal_cancel();
            }
            return result.map(|()| None);
        }
        if self.tls.is_some() {
            self.tls_io()?;
        }
        if self.finished {
            return Ok(None);
        }
        if self.output.is_some() {
            self.write_response()?;
            return Ok(None);
        }
        if self.waiting {
            if matches!(self.socket.peek(&mut [0; 1]), Ok(0)) {
                if let Some(pending) = &mut self.pending_upgrade {
                    // Preserve a peer half-close until acceptance and input drain.
                    pending.input_eof_observed = true;
                } else {
                    self.finished = true;
                }
            }
            return Ok(None);
        }
        if self.tls.as_ref().is_some_and(|tls| tls.is_handshaking()) {
            return Ok(None);
        }
        let mut buffer = [0u8; IO_BUDGET];
        let read = match &mut self.tls {
            Some(tls) => tls.reader().read(&mut buffer),
            None => self.socket.read(&mut buffer),
        };
        match read {
            Ok(0) => {
                self.finished = true;
                return Ok(None);
            }
            Ok(length) => {
                if self.input.len() + length > MAX_REQUEST {
                    self.respond(simple_reply(413, "request too large"));
                    return Ok(None);
                }
                self.input.extend_from_slice(&buffer[..length]);
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(None),
            Err(error) => return Err(error),
        }
        match parse_request_prefix(&self.input, select_upgrade) {
            Ok(Some((request, consumed, candidate))) => {
                if self.peer_tls_closed && !candidate {
                    self.finished = true;
                    return Ok(None);
                }
                if candidate {
                    self.input.drain(..consumed);
                } else {
                    self.input.clear();
                }
                self.upgrade_candidate = candidate;
                self.waiting = true;
                self.deadline = Instant::now() + ROUTE_TIMEOUT;
                Ok(Some(request))
            }
            Ok(None) => Ok(None),
            Err(status) => {
                self.respond(simple_reply(status, "invalid request"));
                Ok(None)
            }
        }
    }

    fn tick_upgrade(
        &mut self,
        upgrade: &mut Upgrade,
        verify: &mut impl FnMut(&[u8], Option<crate::wire::Permission>) -> bool,
    ) -> io::Result<()> {
        use amaterasu_module_sdk::data_plane::abi::BUSY;
        let certificate = upgrade.owner.binding().peer_certificate.clone();
        let permission = upgrade.permission.clone();
        let mut allowed = || -> io::Result<()> {
            if verify(&certificate, Some(permission.clone())) {
                Ok(())
            } else {
                Err(io::ErrorKind::PermissionDenied.into())
            }
        };
        allowed()?;
        let tls = self
            .tls
            .as_mut()
            .ok_or_else(|| io::Error::other("upgrade requires TLS"))?;
        let mut socket_progress = false;
        let mut can_read = match upgrade.owner.flush_input() {
            Ok(()) => !upgrade.input_finished,
            Err(status) if status == BUSY => false,
            Err(_) => return Err(io::Error::other("upgrade input closed")),
        };
        // Consume plaintext already held by rustls before reading another TLS
        // batch. Otherwise a small transfer chunk could grow the TLS input
        // buffer on every tick even while the consumer makes slow progress.
        if can_read {
            allowed()?;
            match read_upgrade_plaintext(tls, &mut upgrade.owner, upgrade.input_eof_observed)? {
                PlaintextRead::Data => can_read = false,
                PlaintextRead::Eof => {
                    upgrade
                        .owner
                        .finish_input()
                        .map_err(|_| io::Error::other("upgrade input finish"))?;
                    upgrade.input_finished = true;
                    can_read = false;
                }
                PlaintextRead::Pending => {}
            }
        }
        if !can_read && !upgrade.input_finished && !upgrade.input_eof_observed {
            allowed()?;
            if matches!(self.socket.peek(&mut [0; 1]), Ok(0)) {
                upgrade.input_eof_observed = true;
            }
        }
        if can_read && !upgrade.input_eof_observed && tls.wants_read() {
            allowed()?;
            match tls.read_tls(&mut (&mut self.socket).take(IO_BUDGET as u64)) {
                Ok(0) => {
                    upgrade.input_eof_observed = true;
                }
                Ok(_) => {
                    socket_progress = true;
                    let state = tls
                        .process_new_packets()
                        .map_err(|_| io::Error::other("TLS protocol failure"))?;
                    if state.peer_has_closed() {
                        upgrade.input_eof_observed = true;
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                Err(error) => return Err(error),
            }
        }
        if tls.wants_write() {
            allowed()?;
            let mut output = BoundedWriter {
                socket: &mut self.socket,
                remaining: IO_BUDGET,
            };
            match tls.write_tls(&mut output) {
                Ok(length) => socket_progress |= length > 0,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                Err(error) => return Err(error),
            }
        }
        if socket_progress {
            self.deadline = Instant::now() + ROUTE_TIMEOUT;
        }
        if upgrade.owner.response_written() && !upgrade.response_flushed && !tls.wants_write() {
            upgrade
                .owner
                .response_flushed()
                .map_err(|_| io::Error::other("upgrade flush state"))?;
            upgrade.response_flushed = true;
        }
        if can_read {
            allowed()?;
            if matches!(
                read_upgrade_plaintext(tls, &mut upgrade.owner, upgrade.input_eof_observed)?,
                PlaintextRead::Eof
            ) {
                upgrade
                    .owner
                    .finish_input()
                    .map_err(|_| io::Error::other("upgrade input finish"))?;
                upgrade.input_finished = true;
            }
        }
        if !upgrade.output_eof {
            allowed()?;
            let mut writer = NonzeroWriter(tls.writer());
            if upgrade.owner.write_step(&mut writer, |_| true)? == OutputProgress::Eof {
                upgrade.output_eof = true;
            }
        }
        if upgrade.output_eof && !upgrade.output_close_sent {
            tls.send_close_notify();
            upgrade.output_close_sent = true;
        }
        if upgrade.output_eof
            && upgrade.input_finished
            && upgrade.owner.pair().input().snapshot().phase
                == amaterasu_module_sdk::data_plane::abi::ENDED
            && !tls.wants_write()
        {
            self.finished = true;
        }
        Ok(())
    }

    fn tls_io(&mut self) -> io::Result<()> {
        let tls = self.tls.as_mut().expect("TLS connection");
        // One bounded socket read and write per tick, never complete_io's loop.
        // Application plaintext is read only while parsing the single request.
        if tls.wants_read()
            && !self.peer_tls_closed
            && self.output.is_none()
            && !(self.waiting && self.upgrade_candidate)
        {
            let read = tls.read_tls(&mut (&mut self.socket).take(IO_BUDGET as u64));
            match read {
                Ok(0) => {
                    if self.waiting {
                        self.finished = true;
                    } else {
                        self.peer_tls_closed = true;
                    }
                }
                Ok(_) => {
                    let state = tls
                        .process_new_packets()
                        .map_err(|_| io::Error::other("TLS protocol failure"))?;
                    if state.peer_has_closed() {
                        if self.waiting {
                            self.finished = true;
                        } else {
                            self.peer_tls_closed = true;
                        }
                    }
                    if self.waiting && state.plaintext_bytes_to_read() > 0 {
                        return Err(io::Error::other("HTTP pipelining unsupported"));
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                Err(error) => return Err(error),
            }
        }
        if tls.wants_write() {
            let mut output = BoundedWriter {
                socket: &mut self.socket,
                remaining: IO_BUDGET,
            };
            match tls.write_tls(&mut output) {
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    fn peer_certificate(&self) -> Option<Vec<u8>> {
        self.tls
            .as_ref()?
            .peer_certificates()?
            .first()
            .map(|certificate| certificate.as_ref().to_vec())
    }

    fn respond(&mut self, reply: Reply) {
        let reply = if reply.body.len() > MAX_REPLY || reply.content_type.contains(['\r', '\n']) {
            simple_reply(500, "response unavailable")
        } else {
            reply
        };
        if reply.status >= 400 {
            eprintln!("amaterasu nvhttp response status={}", reply.status);
        }
        let reason = match reply.status {
            200 => "OK",
            400 => "Bad Request",
            401 => "Unauthorized",
            403 => "Forbidden",
            404 => "Not Found",
            405 => "Method Not Allowed",
            408 => "Request Timeout",
            409 => "Conflict",
            413 => "Content Too Large",
            429 => "Too Many Requests",
            501 => "Not Implemented",
            502 => "Bad Gateway",
            503 => "Service Unavailable",
            504 => "Gateway Timeout",
            _ => "Error",
        };
        let mut bytes=format!("HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\n\r\n",reply.status,reason,reply.content_type,reply.body.len()).into_bytes();
        bytes.extend_from_slice(&reply.body);
        self.output = Some(Output {
            bytes,
            offset: 0,
            requires_paired_tls: reply.requires_paired_tls,
            permission: reply.permission,
        });
        self.input.clear();
        self.waiting = false;
        self.response_delivered = false;
        self.deadline = Instant::now() + IO_TIMEOUT;
    }

    fn write_response(&mut self) -> io::Result<()> {
        let output = self.output.as_mut().expect("pending response");
        if output.offset < output.bytes.len() {
            let end = (output.offset + IO_BUDGET).min(output.bytes.len());
            let result = match &mut self.tls {
                Some(tls) => tls.writer().write(&output.bytes[output.offset..end]),
                None => self.socket.write(&output.bytes[output.offset..end]),
            };
            match result {
                Ok(0) if self.tls.is_some() => {}
                Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(length) => output.offset += length,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                Err(error) => return Err(error),
            }
        }
        if output.offset == output.bytes.len() {
            if let Some(tls) = &mut self.tls {
                if !tls.wants_write() {
                    if self.closing {
                        self.response_delivered = true;
                        self.finished = true;
                    } else {
                        tls.send_close_notify();
                        self.closing = true;
                    }
                }
            } else {
                self.response_delivered = true;
                self.finished = true;
            }
        }
        Ok(())
    }
}

// One bounded plaintext chunk per tick. EOF is reported only after rustls
// has returned all previously accepted plaintext and pending input was flushed.
enum PlaintextRead {
    Data,
    Pending,
    Eof,
}
fn read_upgrade_plaintext(
    tls: &mut ServerConnection,
    owner: &mut DuplexOwner,
    transport_eof: bool,
) -> io::Result<PlaintextRead> {
    use amaterasu_module_sdk::data_plane::abi::BUSY;
    let max_input = owner
        .pair()
        .input()
        .description()
        .limits
        .max_chunk_bytes
        .min(IO_BUDGET as u64) as usize;
    let mut bytes = vec![0; max_input];
    match tls.reader().read(&mut bytes) {
        Ok(0) => Ok(PlaintextRead::Eof),
        Ok(length) => {
            bytes.truncate(length);
            owner
                .input(bytes)
                .map_err(|_| io::Error::other("upgrade input rejected"))?;
            match owner.flush_input() {
                Ok(()) => Ok(PlaintextRead::Data),
                Err(status) if status == BUSY => Ok(PlaintextRead::Data),
                Err(_) => Err(io::Error::other("upgrade input closed")),
            }
        }
        Err(error) if error.kind() == io::ErrorKind::WouldBlock && transport_eof => {
            Ok(PlaintextRead::Eof)
        }
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof && transport_eof => {
            Ok(PlaintextRead::Eof)
        }
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(PlaintextRead::Pending),
        Err(error) => Err(error),
    }
}

// Rustls can return zero when its bounded plaintext buffer is full; treat it
// as backpressure, not as a terminal socket write of zero bytes.
struct NonzeroWriter<W>(W);
impl<W: Write> Write for NonzeroWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        match self.0.write(bytes) {
            Ok(0) if !bytes.is_empty() => Err(io::ErrorKind::WouldBlock.into()),
            result => result,
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

struct BoundedWriter<'a> {
    socket: &'a mut NetworkTcpStream,
    remaining: usize,
}
impl Write for BoundedWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.remaining == 0 {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        let length = self
            .socket
            .write(&bytes[..bytes.len().min(self.remaining)])?;
        self.remaining -= length;
        Ok(length)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
fn parse_request(bytes: &[u8]) -> Result<Option<Request>, u16> {
    parse_request_prefix(bytes, |_, _| None).map(|parsed| parsed.map(|(request, _, _)| request))
}

// Only a trusted, exact method/path selection may permit bytes beyond the
// request. The returned offset identifies their original order in `bytes`.
fn parse_request_prefix(
    bytes: &[u8],
    select_upgrade: impl FnOnce(&str, &str) -> Option<String>,
) -> Result<Option<(Request, usize, bool)>, u16> {
    if bytes.len() > MAX_REQUEST {
        return Err(413);
    }
    let mut headers = [httparse::EMPTY_HEADER; 32];
    let mut request = httparse::Request::new(&mut headers);
    let offset = match request.parse(bytes).map_err(|_| 400u16)? {
        httparse::Status::Partial => return Ok(None),
        httparse::Status::Complete(offset) => offset,
    };
    let method = request.method.ok_or(400u16)?;
    let target = request.path.ok_or(400u16)?;
    if !target.starts_with('/') || target.len() > 24 * 1024 {
        return Err(400);
    }
    let mut length = None;
    let mut host = false;
    let mut connection_upgrade = false;
    let mut upgrade_protocol = None;
    for header in request.headers.iter() {
        if header.name.eq_ignore_ascii_case("transfer-encoding") {
            return Err(400);
        }
        if header.name.eq_ignore_ascii_case("content-length") {
            if length.is_some() {
                return Err(400);
            }
            let text = std::str::from_utf8(header.value)
                .map_err(|_| 400u16)?
                .trim();
            if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err(400);
            }
            length = Some(text.parse::<usize>().map_err(|_| 413u16)?);
        }
        if header.name.eq_ignore_ascii_case("connection") {
            let value = std::str::from_utf8(header.value).map_err(|_| 400u16)?;
            connection_upgrade |= value
                .split(',')
                .any(|token| token.trim().eq_ignore_ascii_case("upgrade"));
        }
        if header.name.eq_ignore_ascii_case("upgrade") {
            if upgrade_protocol.is_some() {
                return Err(400);
            }
            upgrade_protocol = Some(
                std::str::from_utf8(header.value)
                    .map_err(|_| 400u16)?
                    .trim(),
            );
        }
        if header.name.eq_ignore_ascii_case("host") {
            if host {
                return Err(400);
            }
            host = true;
        }
    }
    if request.version == Some(1) && !host {
        return Err(400);
    }
    let total = offset
        .checked_add(length.unwrap_or(0))
        .filter(|total| *total <= MAX_REQUEST)
        .ok_or(413u16)?;
    if bytes.len() < total {
        return Ok(None);
    }
    let selected_protocol = select_upgrade(method, target);
    let candidate = selected_protocol.is_some();
    if let Some(protocol) = selected_protocol
        && (request.version != Some(1)
            || !connection_upgrade
            || !upgrade_protocol.is_some_and(|value| value.eq_ignore_ascii_case(&protocol)))
    {
        return Err(400);
    }
    if bytes.len() != total && !candidate {
        return Err(400);
    }
    Ok(Some((
        Request {
            method: method.into(),
            target: target.into(),
            body: bytes[offset..total].to_vec(),
        },
        total,
        candidate,
    )))
}

fn simple_reply(status: u16, message: &str) -> Reply {
    Reply {
        status,
        content_type: "text/plain; charset=utf-8".into(),
        body: message.as_bytes().to_vec(),
        requires_paired_tls: false,
        permission: None,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn request_suffix_requires_exact_owner_approval_and_preserves_order() {
        let bytes = b"POST /neutral HTTP/1.1\r\nHost: test\r\nConnection: Upgrade\r\nUpgrade: neutral\r\nContent-Length: 3\r\n\r\nreqsuffix";
        assert!(matches!(super::parse_request(bytes), Err(400)));
        assert!(matches!(
            super::parse_request_prefix(bytes, |method, target| (method == "POST"
                && target == "/other")
                .then(|| "neutral".into())),
            Err(400)
        ));
        let (request, consumed, _) = super::parse_request_prefix(bytes, |method, target| {
            (method == "POST" && target == "/neutral").then(|| "neutral".into())
        })
        .unwrap()
        .unwrap();
        assert_eq!(request.body, b"req");
        assert_eq!(&bytes[consumed..], b"suffix");
        let wrong_token =
            b"GET /neutral HTTP/1.1\r\nHost: test\r\nConnection: Upgrade\r\nUpgrade: other\r\n\r\n";
        assert!(matches!(
            super::parse_request_prefix(wrong_token, |_, _| Some("neutral".into())),
            Err(400)
        ));
        let no_connection = b"GET /neutral HTTP/1.1\r\nHost: test\r\nUpgrade: neutral\r\n\r\n";
        assert!(matches!(
            super::parse_request_prefix(no_connection, |_, _| Some("neutral".into())),
            Err(400)
        ));
        let duplicate = b"GET /neutral HTTP/1.1\r\nHost: test\r\nConnection: Upgrade\r\nUpgrade: neutral\r\nUpgrade: neutral\r\n\r\n";
        assert!(matches!(
            super::parse_request_prefix(duplicate, |_, _| Some("neutral".into())),
            Err(400)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn closed_loopback_connection_allows_immediate_listener_restart() {
        use std::io::Read;
        use std::net::{Shutdown, TcpStream};
        use std::time::Duration;

        let listener = super::bind_listener("127.0.0.1:0".parse().unwrap(), "test").unwrap();
        let address = listener.local_addr().unwrap();
        let mut client = TcpStream::connect(address).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let (accepted, _) = listener.accept().unwrap();
        // The server actively closes first, leaving its endpoint in TIME_WAIT.
        accepted.shutdown(Shutdown::Write).unwrap();
        let mut byte = [0];
        assert_eq!(client.read(&mut byte).unwrap(), 0);
        drop(client);
        drop(accepted);
        drop(listener);
        let restarted = super::bind_listener(address, "restart").unwrap();
        assert_eq!(restarted.local_addr().unwrap(), address);
        assert!(super::bind_listener(address, "occupied").is_err());
    }

    use super::*;
    use std::net::{TcpListener, TcpStream};
    use std::sync::Mutex;

    static FIXED_PORT_LOCK: Mutex<()> = Mutex::new(());

    #[cfg(unix)]
    #[test]
    fn closed_loopback_listener_can_restart_without_sharing_a_live_port() {
        use std::{
            io::Read,
            net::{Shutdown, TcpStream},
            time::Duration,
        };

        let listener = bind_listener("127.0.0.1:0".parse().unwrap(), "test").unwrap();
        let address = listener.local_addr().unwrap();
        assert!(bind_listener(address, "occupied").is_err());

        let mut client = TcpStream::connect(address).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let (accepted, _) = listener.accept().unwrap();
        // Close the accepted side first. After the client observes EOF and
        // closes, the server-side connection enters TIME_WAIT on this port.
        accepted.shutdown(Shutdown::Write).unwrap();
        let mut byte = [0];
        assert_eq!(client.read(&mut byte).unwrap(), 0);
        drop(client);
        drop(accepted);
        drop(listener);

        let restarted = bind_listener(address, "restart").unwrap();
        assert_eq!(restarted.local_addr().unwrap(), address);
    }

    #[test]
    fn strict_http_framing_rejects_smuggling_and_waits_for_complete_bodies() {
        assert!(
            parse_request(b"GET /serverinfo HTTP/1.1\r\nHost: test\r\n\r\n")
                .unwrap()
                .is_some()
        );
        assert!(
            parse_request(b"POST /pair HTTP/1.1\r\nHost: test\r\nContent-Length: 2\r\n\r\nx")
                .unwrap()
                .is_none()
        );
        for bytes in [
            &b"GET / HTTP/1.1\r\nHost: a\r\nContent-Length: 0\r\nContent-Length: 0\r\n\r\n"[..],
            &b"GET / HTTP/1.1\r\nHost: a\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n"[..],
            &b"GET / HTTP/1.1\r\nHost: a\r\n\r\nGET / HTTP/1.1\r\nHost: a\r\n\r\n"[..],
        ] {
            assert!(parse_request(bytes).is_err());
        }
    }
    #[test]
    fn real_socket_dispatches_one_request_and_closes_after_reply() {
        let settings = Settings {
            enabled: true,
            http_bind: "127.0.0.1:0".parse().unwrap(),
            https_bind: "127.0.0.1:0".parse().unwrap(),
            ..Settings::default()
        };
        let mut server = Server::bind(&settings).unwrap();
        let mut client = TcpStream::connect(server.http_address()).unwrap();
        client.set_nonblocking(true).unwrap();
        client
            .write_all(b"GET /serverinfo HTTP/1.1\r\nHost: test\r\n\r\n")
            .unwrap();
        let mut response = Vec::new();
        let mut delivered = false;
        for _ in 0..100 {
            let events = server.tick();
            delivered |= events.closed.iter().any(|closed| closed.response_delivered);
            for incoming in events.requests {
                assert!(!incoming.secure);
                server.respond(incoming.id, simple_reply(200, "hello"));
            }
            let mut bytes = [0; 512];
            match client.read(&mut bytes) {
                Ok(0) => break,
                Ok(length) => response.extend_from_slice(&bytes[..length]),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                Err(error) => panic!("{error}"),
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(response.ends_with(b"hello"));
        assert!(delivered);
        assert_eq!(server.connections(), 0);
    }

    #[test]
    fn queued_protected_response_is_closed_before_writing_without_authority() {
        let settings = Settings {
            enabled: true,
            http_bind: "127.0.0.1:0".parse().unwrap(),
            https_bind: "127.0.0.1:0".parse().unwrap(),
            ..Settings::default()
        };
        let mut server = Server::bind(&settings).unwrap();
        let mut client = TcpStream::connect(server.http_address()).unwrap();
        client.set_nonblocking(true).unwrap();
        client
            .write_all(b"GET /applist HTTP/1.1\r\nHost: test\r\n\r\n")
            .unwrap();
        let mut queued = false;
        for _ in 0..100 {
            for request in server.tick().requests {
                let mut reply = simple_reply(200, "private catalog");
                reply.requires_paired_tls = true;
                reply.permission = Some(crate::wire::Permission::List);
                assert!(server.respond(request.id, reply));
                queued = true;
            }
            if queued {
                break;
            }
        }
        assert!(queued);
        let events = server.tick_checked(|_, _| false);
        assert_eq!(events.closed.len(), 1);
        assert!(!events.closed[0].response_delivered);
        let mut bytes = [0; 128];
        assert_eq!(client.read(&mut bytes).unwrap(), 0);
    }

    #[test]
    fn configured_ipv6_bind_address_is_used_by_both_live_listeners() {
        let settings: Settings = serde_json::from_value(serde_json::json!({
            "enabled": true,
            "address_family": "both",
            "bind_address": "::1",
            "http_bind": "127.0.0.1:0",
            "https_bind": "127.0.0.1:0"
        }))
        .unwrap();
        settings.validate().unwrap();

        let server = Server::bind(&settings).expect("bind IPv6 HTTP pair");
        assert_eq!(server.http_address().ip(), "::1".parse::<IpAddr>().unwrap());
        assert_eq!(
            server.https_address().ip(),
            "::1".parse::<IpAddr>().unwrap()
        );
        assert!(TcpStream::connect(server.http_address()).is_ok());
    }

    #[test]
    fn omitted_bind_address_uses_explicit_ipv4_family_for_live_listeners() {
        assert_explicit_ipv4_family_for_live_listeners(false);
    }

    #[test]
    #[ignore = "opens wildcard listeners; run explicitly when testing remote binding"]
    fn omitted_bind_address_uses_explicit_ipv4_family_for_remote_listeners() {
        assert_explicit_ipv4_family_for_live_listeners(true);
    }

    fn assert_explicit_ipv4_family_for_live_listeners(allow_remote: bool) {
        let settings = Settings {
            enabled: true,
            address_family: Some(crate::settings::AddressFamily::Ipv4),
            allow_remote,
            http_bind: "127.0.0.1:0".parse().unwrap(),
            https_bind: "127.0.0.1:0".parse().unwrap(),
            ..Settings::default()
        };
        let server = Server::bind(&settings).expect("bind explicit IPv4 family");
        let expected_ip = if allow_remote {
            IpAddr::V4(Ipv4Addr::UNSPECIFIED)
        } else {
            IpAddr::V4(Ipv4Addr::LOCALHOST)
        };
        let bound_addresses = server
            .http_addresses()
            .into_iter()
            .chain(server.https_addresses())
            .collect::<Vec<_>>();

        for address in bound_addresses {
            assert_eq!(address.ip(), expected_ip);
            assert!(
                TcpStream::connect(SocketAddr::new(
                    IpAddr::V4(Ipv4Addr::LOCALHOST),
                    address.port(),
                ))
                .is_ok()
            );
        }
    }

    #[test]
    fn both_loopback_families_accept_ipv4_and_ipv6() {
        assert_both_families_accept_ipv4_and_ipv6(false);
    }

    #[test]
    #[ignore = "opens wildcard listeners; run explicitly when testing remote binding"]
    fn both_remote_families_accept_ipv4_and_ipv6() {
        assert_both_families_accept_ipv4_and_ipv6(true);
    }

    fn assert_both_families_accept_ipv4_and_ipv6(allow_remote: bool) {
        let settings: Settings = serde_json::from_value(serde_json::json!({
            "enabled": true,
            "allow_remote": allow_remote,
            "address_family": "both",
            "bind_address": "",
            "http_bind": "127.0.0.1:0",
            "https_bind": "127.0.0.1:0"
        }))
        .unwrap();
        let mut server = Server::bind(&settings).expect("bind both address families");
        let http_addresses = server.http_addresses();
        let https_addresses = server.https_addresses();
        assert_eq!(http_addresses.len(), 2);
        assert_eq!(https_addresses.len(), 2);
        let (expected_v4, expected_v6) = if allow_remote {
            (
                IpAddr::V4(Ipv4Addr::UNSPECIFIED),
                IpAddr::V6(Ipv6Addr::UNSPECIFIED),
            )
        } else {
            (
                IpAddr::V4(Ipv4Addr::LOCALHOST),
                IpAddr::V6(Ipv6Addr::LOCALHOST),
            )
        };
        assert_eq!(http_addresses[0].ip(), expected_v4);
        assert_eq!(http_addresses[1].ip(), expected_v6);
        assert_eq!(http_addresses[0].port(), http_addresses[1].port());
        assert_eq!(https_addresses[0].port(), https_addresses[1].port());
        assert!(https_addresses.iter().all(|address| {
            if allow_remote {
                address.ip().is_unspecified()
            } else {
                address.ip().is_loopback()
            }
        }));

        let tls_configuration =
            ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_no_client_auth()
                .with_cert_resolver(Arc::new(rustls::server::ResolvesServerCertUsingSni::new()));
        server.replace_tls(Arc::new(tls_configuration));

        let mut clients = Vec::new();
        for address in &http_addresses {
            let destination = SocketAddr::new(
                if address.is_ipv4() {
                    IpAddr::V4(Ipv4Addr::LOCALHOST)
                } else {
                    IpAddr::V6(Ipv6Addr::LOCALHOST)
                },
                address.port(),
            );
            let mut client = TcpStream::connect(destination).unwrap();
            client
                .write_all(b"GET /serverinfo HTTP/1.1\r\nHost: test\r\n\r\n")
                .unwrap();
            clients.push(client);
        }
        let mut tls_clients = Vec::new();
        for address in https_addresses {
            let destination = SocketAddr::new(
                if address.is_ipv4() {
                    IpAddr::V4(Ipv4Addr::LOCALHOST)
                } else {
                    IpAddr::V6(Ipv6Addr::LOCALHOST)
                },
                address.port(),
            );
            tls_clients.push(TcpStream::connect(destination).unwrap());
        }

        let requests = server.tick().requests;
        assert_eq!(requests.len(), 2);
        assert_eq!(server.connections(), 4, "both HTTPS families were accepted");
        let peers = requests
            .into_iter()
            .map(|request| request.remote)
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            peers,
            [
                IpAddr::V4(Ipv4Addr::LOCALHOST),
                IpAddr::V6(Ipv6Addr::LOCALHOST)
            ]
            .into_iter()
            .collect()
        );
        drop(clients);
        drop(tls_clients);
    }

    #[test]
    fn port_rebind_retains_both_listener_families() {
        let _lock = FIXED_PORT_LOCK.lock().unwrap();
        let settings: Settings = serde_json::from_value(serde_json::json!({
            "enabled": true,
            "address_family": "both",
            "bind_address": "",
            "http_bind": "127.0.0.1:0",
            "https_bind": "127.0.0.1:0"
        }))
        .unwrap();
        let mut server = Server::bind(&settings).unwrap();
        let base_port = available_port_pair();

        server.rebind_port(base_port).unwrap();

        let http = server.http_addresses();
        let https = server.https_addresses();
        assert_eq!(http.len(), 2);
        assert_eq!(https.len(), 2);
        assert_eq!(http[0].port(), base_port);
        assert_eq!(http[1].port(), base_port);
        assert_eq!(https[0].port(), crate::settings::https_port(base_port));
        assert_eq!(https[1].port(), crate::settings::https_port(base_port));

        let mut clients = Vec::new();
        for address in &http {
            let peer = if address.is_ipv4() {
                IpAddr::V4(Ipv4Addr::LOCALHOST)
            } else {
                IpAddr::V6(Ipv6Addr::LOCALHOST)
            };
            let mut client = TcpStream::connect(SocketAddr::new(peer, address.port())).unwrap();
            client
                .write_all(b"GET /serverinfo HTTP/1.1\r\nHost: test\r\n\r\n")
                .unwrap();
            clients.push(client);
        }
        let requests = server.tick().requests;
        assert_eq!(requests.len(), 2);
        let peers = requests
            .into_iter()
            .map(|request| request.remote)
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            peers,
            [
                IpAddr::V4(Ipv4Addr::LOCALHOST),
                IpAddr::V6(Ipv6Addr::LOCALHOST)
            ]
            .into_iter()
            .collect()
        );
        drop(clients);
    }

    #[test]
    fn port_rebind_changes_both_live_listeners_and_preserves_accepted_connections() {
        let _lock = FIXED_PORT_LOCK.lock().unwrap();
        let settings = Settings {
            enabled: true,
            http_bind: "127.0.0.1:0".parse().unwrap(),
            https_bind: "127.0.0.1:0".parse().unwrap(),
            ..Settings::default()
        };
        let mut server = Server::bind(&settings).unwrap();
        let old_http = server.http_address();
        let mut existing_client = TcpStream::connect(old_http).unwrap();
        existing_client
            .write_all(b"GET /serverinfo HTTP/1.1\r\nHost: test\r\n\r\n")
            .unwrap();
        let existing_request = server.tick().requests.pop().unwrap();

        let base_port = available_port_pair();
        server.rebind_port(base_port).unwrap();

        assert_eq!(server.http_address().port(), base_port);
        assert_eq!(
            server.https_address().port(),
            crate::settings::https_port(base_port)
        );
        assert_eq!(server.connections(), 1);

        let mut new_client = TcpStream::connect(server.http_address()).unwrap();
        new_client
            .write_all(b"GET /serverinfo HTTP/1.1\r\nHost: test\r\n\r\n")
            .unwrap();
        let requests = server.tick().requests;
        assert_eq!(requests.len(), 1);
        assert_ne!(requests[0].id, existing_request.id);
        assert_ne!(old_http.port(), base_port);
    }

    #[test]
    fn overlapping_port_moves_reuse_old_listener_sockets_in_both_directions() {
        let _lock = FIXED_PORT_LOCK.lock().unwrap();
        let base_port = available_port_pair();
        let settings = Settings {
            enabled: true,
            http_bind: SocketAddr::from(([127, 0, 0, 1], base_port)),
            https_bind: SocketAddr::from(([127, 0, 0, 1], crate::settings::https_port(base_port))),
            ..Settings::default()
        };
        let mut server = Server::bind(&settings).unwrap();
        let mut accepted_client = TcpStream::connect(server.http_address()).unwrap();
        accepted_client
            .write_all(b"GET /serverinfo HTTP/1.1\r\nHost: test\r\n\r\n")
            .unwrap();
        let accepted_request = server.tick().requests.pop().unwrap();

        let raised_base_port = base_port + crate::settings::HTTPS_PORT_OFFSET;
        server.rebind_port(raised_base_port).unwrap();
        assert_eq!(server.http_address().port(), raised_base_port);
        assert_eq!(server.https_address().port(), base_port);
        assert!(server.connections.contains_key(&accepted_request.id));

        server.rebind_port(base_port).unwrap();
        assert_eq!(server.http_address().port(), base_port);
        assert_eq!(
            server.https_address().port(),
            crate::settings::https_port(base_port)
        );
        assert!(server.connections.contains_key(&accepted_request.id));
    }

    #[test]
    fn failed_second_listener_bind_keeps_the_current_pair_intact() {
        let _lock = FIXED_PORT_LOCK.lock().unwrap();
        let settings = Settings {
            enabled: true,
            http_bind: "127.0.0.1:0".parse().unwrap(),
            https_bind: "127.0.0.1:0".parse().unwrap(),
            ..Settings::default()
        };
        let mut server = Server::bind(&settings).unwrap();
        let old_http = server.http_address();
        let old_https = server.https_address();
        let (_occupied_https, base_port) = (0..100)
            .find_map(|_| {
                let listener = TcpListener::bind("127.0.0.1:0").ok()?;
                let base_port = listener
                    .local_addr()
                    .ok()?
                    .port()
                    .checked_add(crate::settings::HTTPS_PORT_OFFSET)?;
                if crate::settings::validate_port(base_port).is_err() {
                    return None;
                }
                let http_probe =
                    TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], base_port))).ok()?;
                drop(http_probe);
                Some((listener, base_port))
            })
            .expect("find a valid HTTPS port for the occupied-pair test");

        assert!(server.rebind_port(base_port).is_err());
        assert_eq!(server.http_address(), old_http);
        assert_eq!(server.https_address(), old_https);
    }

    #[test]
    #[ignore = "opens wildcard listeners; run explicitly when testing remote binding"]
    fn same_port_wildcard_expansion_rebinds_all_families_before_reporting_active() {
        let _lock = FIXED_PORT_LOCK.lock().unwrap();
        let base_port = available_port_pair();
        let startup = Settings {
            enabled: true,
            allow_remote: true,
            http_bind: SocketAddr::from(([127, 0, 0, 1], base_port)),
            https_bind: SocketAddr::from(([127, 0, 0, 1], crate::settings::https_port(base_port))),
            ..Settings::default()
        };
        let mut server = Server::bind(&startup).unwrap();
        let mut target = startup.clone();
        target.address_family = Some(crate::settings::AddressFamily::Both);
        target.bind_address = Some(String::new());

        server.rebind_settings(&target).unwrap();

        assert!(
            server
                .http_addresses()
                .iter()
                .any(|address| address.is_ipv4())
        );
        assert!(
            server
                .http_addresses()
                .iter()
                .any(|address| address.is_ipv6())
        );
        assert!(
            server
                .http_addresses()
                .iter()
                .all(|address| address.ip().is_unspecified())
        );
        assert!(
            server
                .https_addresses()
                .iter()
                .all(|address| address.ip().is_unspecified())
        );
        let clients = [
            TcpStream::connect(SocketAddr::from(([127, 0, 0, 1], base_port))).unwrap(),
            TcpStream::connect(SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), base_port))
                .unwrap(),
        ];
        let _ = server.tick();
        assert_eq!(server.connections(), 2);
        drop(clients);
    }

    #[test]
    fn failed_overlapping_loopback_expansion_restores_both_listener_sets() {
        assert_failed_overlapping_expansion_restores_both_listener_sets(false);
    }

    #[test]
    #[ignore = "opens a wildcard listener; run explicitly when testing remote binding"]
    fn failed_overlapping_wildcard_expansion_restores_both_listener_sets() {
        assert_failed_overlapping_expansion_restores_both_listener_sets(true);
    }

    fn assert_failed_overlapping_expansion_restores_both_listener_sets(allow_remote: bool) {
        let _lock = FIXED_PORT_LOCK.lock().unwrap();
        let (_occupied_ipv6, startup, mut server) = (0..100)
            .find_map(|_| {
                let occupied_ipv6 = bind_listener(
                    SocketAddr::new(
                        IpAddr::V6(if allow_remote {
                            Ipv6Addr::UNSPECIFIED
                        } else {
                            Ipv6Addr::LOCALHOST
                        }),
                        0,
                    ),
                    "IPv6 rollback blocker",
                )
                .ok()?;
                let http_port = occupied_ipv6.local_addr().ok()?.port();
                let https_port = http_port.checked_sub(crate::settings::HTTPS_PORT_OFFSET)?;
                if crate::settings::validate_port(http_port).is_err()
                    || crate::settings::validate_port(https_port).is_err()
                {
                    return None;
                }

                let startup = Settings {
                    enabled: true,
                    http_bind: SocketAddr::from(([127, 0, 0, 1], http_port)),
                    https_bind: SocketAddr::from(([127, 0, 0, 1], https_port)),
                    ..Settings::default()
                };
                let server = Server::bind(&startup).ok()?;
                Some((occupied_ipv6, startup, server))
            })
            .expect("find an IPv6-blocked port pair for the rollback test");
        let old_http = server.http_addresses();
        let old_https = server.https_addresses();
        let mut target = startup;
        target.allow_remote = allow_remote;
        target.address_family = Some(crate::settings::AddressFamily::Both);
        target.bind_address = Some(String::new());

        assert!(server.rebind_settings(&target).is_err());
        assert_eq!(server.http_addresses(), old_http);
        assert_eq!(server.https_addresses(), old_https);
    }

    #[test]
    fn unrelated_occupied_target_port_does_not_drop_current_listeners() {
        let _lock = FIXED_PORT_LOCK.lock().unwrap();
        let startup = Settings {
            enabled: true,
            http_bind: "127.0.0.1:0".parse().unwrap(),
            https_bind: "127.0.0.1:0".parse().unwrap(),
            ..Settings::default()
        };
        let mut server = Server::bind(&startup).unwrap();
        let old_http = server.http_addresses();
        let old_https = server.https_addresses();
        let target_port = available_port_pair();
        let occupied = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], target_port))).unwrap();
        let mut target = startup;
        target.http_bind.set_port(target_port);
        target
            .https_bind
            .set_port(crate::settings::https_port(target_port));

        assert!(server.rebind_settings(&target).is_err());
        assert_eq!(server.http_addresses(), old_http);
        assert_eq!(server.https_addresses(), old_https);
        drop(occupied);
    }

    #[test]
    fn unchanged_listener_does_not_trigger_release_for_unrelated_occupied_peer_port() {
        let _lock = FIXED_PORT_LOCK.lock().unwrap();
        let base_port = available_port_pair();
        let startup = Settings {
            enabled: true,
            http_bind: SocketAddr::from(([127, 0, 0, 1], base_port)),
            https_bind: SocketAddr::from(([127, 0, 0, 1], crate::settings::https_port(base_port))),
            ..Settings::default()
        };
        let mut server = Server::bind(&startup).unwrap();
        let old_http = server.http_addresses();
        let old_https = server.https_addresses();
        let occupied = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut target = startup;
        target
            .https_bind
            .set_port(occupied.local_addr().unwrap().port());

        assert!(server.rebind_settings(&target).is_err());
        assert_eq!(server.http_addresses(), old_http);
        assert_eq!(server.https_addresses(), old_https);
        drop(occupied);
    }

    fn available_port_pair() -> u16 {
        for _ in 0..100 {
            let https = TcpListener::bind("127.0.0.1:0").unwrap();
            let Some(base) = https
                .local_addr()
                .unwrap()
                .port()
                .checked_add(crate::settings::HTTPS_PORT_OFFSET)
            else {
                continue;
            };
            if crate::settings::validate_port(base).is_err() {
                continue;
            }
            let Some(raised_base) = base.checked_add(crate::settings::HTTPS_PORT_OFFSET) else {
                continue;
            };
            if crate::settings::validate_port(raised_base).is_err() {
                continue;
            }
            let Ok(http) = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], base))) else {
                continue;
            };
            let Ok(raised_http) =
                TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], raised_base)))
            else {
                continue;
            };
            drop((http, raised_http, https));
            return base;
        }
        panic!("could not reserve an unused local listener port pair");
    }
    #[test]
    fn deferred_request_has_pin_allowance_then_expires_and_reports_cancellation() {
        let settings = Settings {
            enabled: true,
            http_bind: "127.0.0.1:0".parse().unwrap(),
            https_bind: "127.0.0.1:0".parse().unwrap(),
            ..Settings::default()
        };
        let mut server = Server::bind(&settings).unwrap();
        let mut client = TcpStream::connect(server.http_address()).unwrap();
        client
            .write_all(b"GET /pair HTTP/1.1\r\nHost: test\r\n\r\n")
            .unwrap();
        let incoming = server
            .tick()
            .requests
            .pop()
            .expect("complete local request");
        let connection = server.connections.get_mut(&incoming.id).unwrap();
        assert!(connection.waiting);
        assert!(
            connection
                .deadline
                .saturating_duration_since(Instant::now())
                > Duration::from_secs(120)
        );
        connection.deadline = Instant::now();
        assert_eq!(
            server.tick().closed,
            vec![Closed {
                id: incoming.id,
                response_delivered: false,
            }]
        );
        assert_eq!(server.connections(), 0);
    }

    #[test]
    fn saturated_rustls_plaintext_buffer_retains_the_response_for_retry() {
        let listener = Network::test_provider()
            .bind_tcp("127.0.0.1:0".parse().unwrap())
            .unwrap();
        let _peer = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (socket, remote) = listener.accept().unwrap();
        let configuration =
            ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_no_client_auth()
                .with_cert_resolver(Arc::new(rustls::server::ResolvesServerCertUsingSni::new()));
        let mut tls = ServerConnection::new(Arc::new(configuration)).unwrap();
        tls.set_buffer_limit(Some(1));
        assert_eq!(tls.writer().write(b"x").unwrap(), 1);
        let mut connection = Connection {
            socket,
            remote: remote.ip(),
            tls: Some(tls),
            input: Vec::new(),
            output: Some(Output {
                requires_paired_tls: false,
                permission: None,
                bytes: b"response".to_vec(),
                offset: 0,
            }),
            upgrade: None,
            pending_upgrade: None,
            upgrade_candidate: false,
            peer_tls_closed: false,
            waiting: false,
            finished: false,
            closing: false,
            response_delivered: false,
            deadline: Instant::now() + IO_TIMEOUT,
        };
        connection.write_response().unwrap();
        assert_eq!(connection.output.as_ref().unwrap().offset, 0);
        assert!(!connection.finished);
    }

    // The client pins the public test certificate and verifies its handshake
    // signatures. The server uses the production possession verifier below.
    #[derive(Debug)]
    struct TestServerVerifier;
    impl rustls::client::danger::ServerCertVerifier for TestServerVerifier {
        fn verify_server_cert(
            &self,
            certificate: &rustls::pki_types::CertificateDer<'_>,
            _: &[rustls::pki_types::CertificateDer<'_>],
            _: &rustls::pki_types::ServerName<'_>,
            _: &[u8],
            _: rustls::pki_types::UnixTime,
        ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
            let expected = pem_rfc7468::decode_vec(include_bytes!(
                "../../management/tests/fixtures/browser-cert.pem"
            ))
            .unwrap()
            .1;
            if certificate.as_ref() != expected {
                return Err(rustls::Error::General("unexpected test certificate".into()));
            }
            Ok(rustls::client::danger::ServerCertVerified::assertion())
        }
        fn verify_tls12_signature(
            &self,
            message: &[u8],
            certificate: &rustls::pki_types::CertificateDer<'_>,
            signature: &rustls::DigitallySignedStruct,
        ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
            rustls::crypto::verify_tls12_signature(
                message,
                certificate,
                signature,
                &rustls::crypto::ring::default_provider().signature_verification_algorithms,
            )
        }
        fn verify_tls13_signature(
            &self,
            message: &[u8],
            certificate: &rustls::pki_types::CertificateDer<'_>,
            signature: &rustls::DigitallySignedStruct,
        ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
            rustls::crypto::verify_tls13_signature(
                message,
                certificate,
                signature,
                &rustls::crypto::ring::default_provider().signature_verification_algorithms,
            )
        }
        fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
            rustls::crypto::ring::default_provider()
                .signature_verification_algorithms
                .supported_schemes()
        }
    }
    #[test]
    fn attached_upgrade_runs_through_real_tls_connection_and_closes_on_denial() {
        use amaterasu_module_sdk::data_plane::{self, Hub, OwnedChunk, abi::*};
        use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
        use std::io::{Read, Write};
        fn cert(pem: &[u8]) -> CertificateDer<'static> {
            CertificateDer::from(pem_rfc7468::decode_vec(pem).unwrap().1)
        }
        fn key(pem: &[u8]) -> PrivateKeyDer<'static> {
            PrivateKeyDer::try_from(pem_rfc7468::decode_vec(pem).unwrap().1).unwrap()
        }
        let host = cert(include_bytes!(
            "../../management/tests/fixtures/browser-cert.pem"
        ));
        let client_cert = cert(include_bytes!(
            "../../management/tests/fixtures/browser-cert.pem"
        ));
        let server_config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_client_cert_verifier(crate::tls::test_possession_verifier())
        .with_single_cert(
            vec![host],
            key(include_bytes!(
                "../../management/tests/fixtures/browser-key.pem"
            )),
        )
        .unwrap();
        let client_config = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(TestServerVerifier))
        .with_client_auth_cert(
            vec![client_cert.clone()],
            key(include_bytes!(
                "../../management/tests/fixtures/browser-key.pem"
            )),
        )
        .unwrap();
        let settings = Settings {
            enabled: true,
            http_bind: "127.0.0.1:0".parse().unwrap(),
            https_bind: "127.0.0.1:0".parse().unwrap(),
            ..Settings::default()
        };
        let mut server = Server::bind(&settings).unwrap();
        server.replace_tls(Arc::new(server_config));
        let mut socket = TcpStream::connect(server.https_address()).unwrap();
        socket.set_nonblocking(true).unwrap();
        let mut client = rustls::ClientConnection::new(
            Arc::new(client_config),
            ServerName::try_from("localhost").unwrap().to_owned(),
        )
        .unwrap();
        fn exchange(
            client: &mut rustls::ClientConnection,
            socket: &mut TcpStream,
            server: &mut Server,
        ) -> Events {
            if client.wants_write() {
                match client.write_tls(socket) {
                    Ok(_) => {}
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                    Err(e) => panic!("client write: {e}"),
                }
            }
            let events = server.tick_checked_with_upgrades(
                |_, _| true,
                |method, target| {
                    (method == "GET" && target == "/neutral").then(|| "neutral".into())
                },
            );
            match client.read_tls(socket) {
                Ok(n) if n > 0 => {
                    client.process_new_packets().unwrap();
                }
                Ok(_) => {}
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                Err(e) => panic!("client read: {e}"),
            }
            events
        }
        for _ in 0..5000 {
            exchange(&mut client, &mut socket, &mut server);
            if !client.is_handshaking() {
                break;
            }
            std::thread::yield_now();
        }
        assert!(!client.is_handshaking());
        client
            .writer()
            .write_all(b"GET /neutral HTTP/1.1\r\nHost: localhost\r\nConnection: Upgrade\r\nUpgrade: neutral\r\n\r\npre")
            .unwrap();
        if std::env::var_os("AMATERASU_DUPLEX_REQUEST_CLOSE").is_some() {
            client.send_close_notify();
        }
        let mut incoming = None;
        for _ in 0..5000 {
            let events = exchange(&mut client, &mut socket, &mut server);
            if let Some(value) = events.requests.into_iter().next() {
                incoming = Some(value);
                break;
            }
            std::thread::yield_now();
        }
        let incoming = incoming.expect("request over TLS");
        assert_eq!(
            incoming.peer_certificate.as_deref(),
            Some(client_cert.as_ref())
        );
        let schema = Schema {
            namespace: 1,
            value: 1,
            version: 1,
        };
        use amaterasu_contracts::{
            ApiFieldKind, ApiMethod, ApiMethodApproval, ApiPrivilege, ApiSchema, Grants,
            RuntimeConfig,
        };
        use amaterasu_module_sdk::{Command, Response, protocol_handler};
        use base64::{Engine, engine::general_purpose::STANDARD};
        use std::collections::BTreeMap;
        let principal = if std::env::var("AMATERASU_DUPLEX_EXPECT").as_deref() == Ok("reverse") {
            "outsider"
        } else {
            "consumer"
        };
        let schema_fields = |fields: &[(&str, ApiFieldKind)]| ApiSchema {
            required: fields
                .iter()
                .map(|(name, kind)| ((*name).into(), kind.clone()))
                .collect(),
            optional: BTreeMap::new(),
            allow_extra: false,
        };
        let binding = crate::wire::participants::Binding {
            path: "/neutral".into(),
            http_method: "GET".into(),
            service: "org.example.neutral/1".into(),
            method: "handle".into(),
            interface_major: 1,
            interface_minor: 0,
            schema: "org.example.request/1".into(),
            entitlement: "org.example/read".into(),
            upgrade: Some(crate::wire::participants::UpgradeProfile {
                protocol: "neutral".into(),
                input_format: schema,
                output_format: schema,
                metadata_schema: schema,
            }),
        };
        let method = ApiMethod {
            service: binding.service.clone(),
            method: binding.method.clone(),
            interface_major: 1,
            interface_minor: 0,
            request: schema_fields(&[
                ("version", ApiFieldKind::Integer),
                ("request", ApiFieldKind::Object),
                ("transfer_service", ApiFieldKind::String),
                ("input_id", ApiFieldKind::Object),
                ("input", ApiFieldKind::Object),
                ("output_id", ApiFieldKind::Object),
                ("output", ApiFieldKind::Object),
            ]),
            response: schema_fields(&[
                ("version", ApiFieldKind::Integer),
                ("outcome", ApiFieldKind::String),
            ]),
            privilege: ApiPrivilege::User,
        };
        let mut config = RuntimeConfig::default();
        config.principals.insert(
            "amaterasu.nvhttp".into(),
            Grants {
                call: [method.service.clone()].into(),
                ..Default::default()
            },
        );
        config.principals.insert(
            principal.into(),
            Grants {
                provide: [method.service.clone()].into(),
                ..Default::default()
            },
        );
        config.api_methods.push(ApiMethodApproval {
            provider: principal.into(),
            callers: ["amaterasu.nvhttp".into()].into(),
            method: method.clone(),
        });
        let mut broker = amaterasu_runtime::Broker::new(config).unwrap();
        assert!(matches!(
            broker.dispatch(
                principal,
                Command::RegisterApi {
                    method: method.clone()
                }
            ),
            Response::Ok
        ));
        let mut router = crate::wire::Router::new(
            crate::wire::RouterConfig {
                hostname: "test".into(),
                http_port: 1,
                https_port: 2,
            },
            crate::launch::Controller::new(None).unwrap(),
        );
        router.set_participants(vec![binding.clone()]);
        let hub = Hub::new(2, 128 * 1024).unwrap();
        let connection_id = incoming.id;
        let deny_authority = std::env::var_os("AMATERASU_DUPLEX_DENY_AUTHORITY").is_some();
        let no_approval = std::env::var_os("AMATERASU_DUPLEX_NO_APPROVAL").is_some();
        let mut approval_queries = 0;
        let result = crate::begin_configured_upgrade(
            &mut server,
            &mut router,
            &hub,
            incoming,
            binding.clone(),
            |_, _| !deny_authority,
            |cmd| {
                if matches!(cmd, Command::QueryApiApproval { .. }) {
                    approval_queries += 1;
                    if no_approval {
                        return Err("approval unavailable".into());
                    }
                }
                Ok(broker.dispatch("amaterasu.nvhttp", cmd))
            },
        );
        if deny_authority || no_approval {
            assert!(result.is_err());
            assert_eq!(
                approval_queries,
                usize::from(no_approval && !deny_authority)
            );
            assert!(!server.connections.contains_key(&connection_id));
            assert!(router.take_approved_upgrades().is_empty());
            assert!(matches!(
                broker.dispatch(principal, Command::Poll {}),
                Response::Pending
            ));
            return;
        }
        result.unwrap();
        let pending = server
            .connections
            .get(&connection_id)
            .unwrap()
            .pending_upgrade
            .as_ref()
            .unwrap();
        assert_eq!(pending.owner.binding().consumer_principal, principal);
        let input_id = pending.owner.pair().input_id();
        let output_id = pending.owner.pair().output_id();
        let input = pending.owner.pair().input().clone();
        let output = pending.owner.pair().output().clone();
        if std::env::var_os("AMATERASU_DUPLEX_CONSUMER").is_none() {
            output
                .push(OwnedChunk::with_bytes(1, b"out".to_vec()))
                .unwrap();
        }
        let now = Instant::now();
        let lost_reply = std::env::var_os("AMATERASU_DUPLEX_LOST_REPLY").is_some();
        let mut admissions = 0;
        assert!(
            router
                .poll_with_identity(
                    now,
                    &mut |cmd| {
                        let is_call = matches!(cmd, Command::CallApi { .. });
                        let response = broker.dispatch("amaterasu.nvhttp", cmd);
                        if is_call {
                            admissions += 1;
                        }
                        if lost_reply && is_call {
                            Err("accepted reply lost".into())
                        } else {
                            Ok(response)
                        }
                    },
                    |_, _| true
                )
                .is_empty()
        );
        let request_id = match broker.dispatch(principal, Command::Poll {}) {
            Response::Request {
                id,
                caller,
                payload,
                ..
            } => {
                assert_eq!(caller, "amaterasu.nvhttp");
                let offer = serde_json::from_value::<protocol_handler::UpgradeOffer>(
                    payload["arguments"].clone(),
                )
                .unwrap();
                assert_eq!(offer.input_id, input_id);
                assert_eq!(offer.output_id, output_id);
                assert_eq!(offer.transfer_service, crate::DUPLEX_TRANSFER_SERVICE);
                assert_eq!(
                    offer.request.peer.certificate_der_base64,
                    STANDARD.encode(client_cert.as_ref())
                );
                id
            }
            _ => panic!("approved handler did not receive offer"),
        };
        if lost_reply {
            let replies = router.poll_with_identity(
                now + Duration::from_millis(1),
                &mut |cmd| Ok(broker.dispatch("amaterasu.nvhttp", cmd)),
                |_, _| false,
            );
            assert_eq!(replies.len(), 1);
            assert!(server.respond(connection_id, replies.into_iter().next().unwrap().1));
            assert_eq!(input.snapshot().phase, CANCELLED);
            assert_eq!(output.snapshot().phase, CANCELLED);
            assert!(
                router
                    .poll_with_identity(
                        now + Duration::from_millis(200),
                        &mut |cmd| {
                            if matches!(cmd, Command::CallApi { .. }) {
                                admissions += 1;
                            }
                            Ok(broker.dispatch("amaterasu.nvhttp", cmd))
                        },
                        |_, _| true
                    )
                    .is_empty()
            );
            assert_eq!(admissions, 1);
            assert!(router.take_approved_upgrades().is_empty());
            return;
        }
        if std::env::var_os("AMATERASU_DUPLEX_PENDING_EOF").is_some() {
            socket.shutdown(std::net::Shutdown::Write).unwrap();
        }
        for _ in 0..4 {
            exchange(&mut client, &mut socket, &mut server);
        }
        if std::env::var_os("AMATERASU_DUPLEX_PENDING_EOF").is_some() {
            assert!(
                server
                    .connections
                    .get(&connection_id)
                    .unwrap()
                    .pending_upgrade
                    .as_ref()
                    .unwrap()
                    .input_eof_observed
            );
        }
        let mut premature = [0; 8];
        assert!(
            matches!(client.reader().read(&mut premature), Err(error) if error.kind() == io::ErrorKind::WouldBlock)
        );
        let decision = match std::env::var("AMATERASU_DUPLEX_DECISION").as_deref() {
            Ok("reject") => serde_json::json!({"outcome":"reject", "version":1}),
            Ok("invalid") => serde_json::json!({"outcome":"accept", "version":2}),
            _ => serde_json::json!({"outcome":"accept", "version":1}),
        };
        assert!(matches!(
            broker.dispatch(
                principal,
                Command::Complete {
                    request: request_id,
                    payload: decision
                }
            ),
            Response::Ok
        ));
        let replies = router.poll_with_identity(
            now,
            &mut |cmd| Ok(broker.dispatch("amaterasu.nvhttp", cmd)),
            |_, _| true,
        );
        if std::env::var_os("AMATERASU_DUPLEX_DECISION").is_some() {
            assert_eq!(replies.len(), 1);
            assert!(server.respond(connection_id, replies.into_iter().next().unwrap().1));
            assert_eq!(input.snapshot().phase, CANCELLED);
            assert_eq!(output.snapshot().phase, CANCELLED);
            assert!(router.take_approved_upgrades().is_empty());
            return;
        }
        assert!(replies.is_empty());
        assert!(router.take_approved_upgrades().contains(&connection_id));
        assert!(server.activate_upgrade(connection_id, |_, _| true));
        // The neutral fixture receives only a native table, never TLS or a socket.
        // Runtime principal checks have a separate qualification.
        // SAFETY: environment points to a checked-in test fixture built by the runner.
        let library = std::env::var_os("AMATERASU_DUPLEX_CONSUMER")
            .map(|path| unsafe { libloading::Library::new(path).unwrap() });
        let factory = hub.factory();
        let mut native: *const amaterasu_module_sdk::abi::NativeInterface = std::ptr::null();
        if let Some(library) = &library {
            for _ in 0..5000 {
                exchange(&mut client, &mut socket, &mut server);
                if input.snapshot().queued_bytes == 3 {
                    break;
                }
                std::thread::yield_now();
            }
            assert_eq!(input.snapshot().queued_bytes, 3);
            let query = amaterasu_module_sdk::abi::NativeInterfaceQuery {
                header: amaterasu_module_sdk::abi::AbiHeader::of::<
                    amaterasu_module_sdk::abi::NativeInterfaceQuery,
                >(),
                interface_major: INTERFACE_MAJOR,
                min_minor: INTERFACE_MINOR,
                min_size: API_SIZE,
            };
            // SAFETY: factory, caller bytes, query and output pointer remain live
            // through binding, table calls and release.
            assert_eq!(
                unsafe {
                    (factory.api.bind.unwrap())(
                        factory.api.context,
                        amaterasu_module_sdk::abi::BorrowedBytes::new(principal.as_bytes()),
                        &query,
                        &mut native,
                    )
                },
                amaterasu_module_sdk::abi::STATUS_OK
            );
            type Exchange = unsafe extern "C" fn(
                *const TransferApi,
                ChannelId,
                ChannelId,
            ) -> amaterasu_module_sdk::abi::Status;
            // SAFETY: this symbol is provided by the checked-in fixture and all
            // binding objects remain live through the synchronous callback.
            let consumer: libloading::Symbol<Exchange> =
                unsafe { library.get(b"neutral_exchange\0").unwrap() };
            let upgrade = server
                .connections
                .get(&connection_id)
                .unwrap()
                .upgrade
                .as_ref()
                .unwrap();
            assert_eq!(
                unsafe {
                    consumer(
                        native.cast(),
                        upgrade.owner.pair().input_id(),
                        upgrade.owner.pair().output_id(),
                    )
                },
                amaterasu_module_sdk::abi::STATUS_OK
            );
        }
        let mut plaintext = Vec::new();
        for _ in 0..5000 {
            exchange(&mut client, &mut socket, &mut server);
            let mut bytes = [0; 128];
            match client.reader().read(&mut bytes) {
                Ok(n) if n > 0 => plaintext.extend_from_slice(&bytes[..n]),
                Ok(_) => {}
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                Err(e) => panic!("client plaintext: {e}"),
            }
            if plaintext.ends_with(b"out")
                || plaintext.ends_with(b"pre")
                || plaintext.ends_with(b"erp")
            {
                break;
            }
            std::thread::yield_now();
        }
        let expected = match std::env::var("AMATERASU_DUPLEX_EXPECT").as_deref() {
            Ok("identity") => b"HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: neutral\r\n\r\npre".as_slice(),
            Ok("reverse") => b"HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: neutral\r\n\r\nerp".as_slice(),
            _ => b"HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: neutral\r\n\r\nout".as_slice(),
        };
        assert_eq!(plaintext, expected);
        assert_eq!(
            input.snapshot().queued_bytes,
            if library.is_some() { 0 } else { 3 }
        );
        if library.is_none()
            && std::env::var_os("AMATERASU_DUPLEX_PENDING_EOF").is_none()
            && std::env::var_os("AMATERASU_DUPLEX_REQUEST_CLOSE").is_none()
        {
            // Fill the input channel, then send two further chunks. The owner
            // must retain order and stop reading while the consumer is slow.
            let payload: Vec<u8> = (0..80 * 1024).map(|n| (n % 251) as u8).collect();
            client.set_buffer_limit(Some(256 * 1024));
            client.writer().write_all(&payload).unwrap();
            if std::env::var_os("AMATERASU_DUPLEX_RAW_EOF").is_some() {
                while client.wants_write() {
                    match client.write_tls(&mut socket) {
                        Ok(_) => {}
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => std::thread::yield_now(),
                        Err(e) => panic!("client write: {e}"),
                    }
                }
                socket.shutdown(std::net::Shutdown::Write).unwrap();
            } else {
                client.send_close_notify();
            }
            for _ in 0..50 {
                exchange(&mut client, &mut socket, &mut server);
                std::thread::yield_now();
            }
            assert!(input.snapshot().queued_bytes >= 3);
            assert!(input.snapshot().queued_bytes <= 64 * 1024);
            fn consume(channel: &data_plane::Channel) -> Vec<u8> {
                let mut bytes = Vec::new();
                // SAFETY: copy each borrowed segment during the callback only.
                let status = unsafe {
                    channel.receive(|chunk| {
                        for segment in
                            std::slice::from_raw_parts(chunk.segments, chunk.segment_count as usize)
                        {
                            bytes.extend_from_slice(std::slice::from_raw_parts(
                                segment.data,
                                segment.len,
                            ));
                        }
                        amaterasu_module_sdk::abi::STATUS_OK
                    })
                };
                assert_eq!(status, amaterasu_module_sdk::abi::STATUS_OK);
                bytes
            }
            assert_eq!(consume(&input), b"pre");
            let mut received = Vec::new();
            for _ in 0..50000 {
                exchange(&mut client, &mut socket, &mut server);
                if input.snapshot().queued_chunks > 0 {
                    received.extend(consume(&input));
                }
                if received.len() == payload.len() {
                    break;
                }
                std::thread::yield_now();
            }
            assert_eq!(received, payload);
            for _ in 0..5000 {
                exchange(&mut client, &mut socket, &mut server);
                if input.snapshot().phase == ENDED {
                    break;
                }
                std::thread::yield_now();
            }
            assert_eq!(input.snapshot().phase, ENDED);
            // A peer's inbound EOF does not suppress the opposite direction.
            output
                .push(OwnedChunk::with_bytes(2, b"keep".to_vec()))
                .unwrap();
            let mut after_eof = Vec::new();
            for _ in 0..5000 {
                exchange(&mut client, &mut socket, &mut server);
                let mut bytes = [0; 16];
                match client.reader().read(&mut bytes) {
                    Ok(n) if n > 0 => after_eof.extend_from_slice(&bytes[..n]),
                    Ok(_) => {}
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                    Err(e) => panic!("client plaintext after EOF: {e}"),
                }
                if after_eof.len() >= 4 {
                    break;
                }
                std::thread::yield_now();
            }
            assert_eq!(after_eof, b"keep");
        }
        if std::env::var_os("AMATERASU_DUPLEX_PENDING_EOF").is_some()
            || std::env::var_os("AMATERASU_DUPLEX_REQUEST_CLOSE").is_some()
        {
            if input.snapshot().queued_chunks > 0 {
                // SAFETY: callback does not retain the borrowed descriptor.
                assert_eq!(
                    unsafe { input.receive(|_| amaterasu_module_sdk::abi::STATUS_OK) },
                    amaterasu_module_sdk::abi::STATUS_OK
                );
            }
            for _ in 0..5000 {
                exchange(&mut client, &mut socket, &mut server);
                if input.snapshot().phase == ENDED {
                    break;
                }
                std::thread::yield_now();
            }
            assert_eq!(input.snapshot().phase, ENDED);
        }
        if std::env::var_os("AMATERASU_DUPLEX_NATURAL").is_some() {
            assert_eq!(input.snapshot().phase, ENDED);
            output.finish().unwrap();
            let mut naturally_closed = false;
            for _ in 0..5000 {
                let events = exchange(&mut client, &mut socket, &mut server);
                if events
                    .closed
                    .iter()
                    .any(|closed| closed.id == connection_id)
                {
                    naturally_closed = true;
                    break;
                }
                std::thread::yield_now();
            }
            assert!(naturally_closed);
            assert_eq!(input.snapshot().phase, CANCELLED);
            assert_eq!(output.snapshot().phase, CANCELLED);
            return;
        }
        output
            .push(OwnedChunk::with_bytes(3, b"late".to_vec()))
            .unwrap();
        let mut checks = 0;
        // Admit the tick-level check, then simulate revocation at the next
        // bounded effect. No queued output may be delivered after that denial.
        let closed = server
            .tick_checked(|_, _| {
                checks += 1;
                checks == 1
            })
            .closed;
        assert!(checks >= 2);
        assert_eq!(closed.len(), 1);
        assert_eq!(input.snapshot().phase, CANCELLED);
        assert_eq!(output.snapshot().phase, CANCELLED);
        if !native.is_null() {
            // SAFETY: all fixture calls have returned and the factory is live.
            unsafe { ((*native).release.unwrap())((*native).context) };
        }
        drop(library);
    }
}

#[cfg(test)]
mod provider_selection_tests;
