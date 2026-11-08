//! Encrypted, one-transaction-per-connection RTSP transport. Ticket cryptographic
//! state survives TCP connections; the caller owns ticket authorization and routing.
mod connection;
mod tickets;

use amaterasu_gamestream::{crypto::MAX_RTSP_PLAINTEXT_BYTES, rtsp::Request};
use amaterasu_module_sdk::network::{Network, TcpListener};
use connection::{Connection, Step};
use std::{
    collections::BTreeMap,
    io,
    net::{IpAddr, SocketAddr},
};
use tickets::Tickets;

pub type TransportResult<T> = Result<T, &'static str>;
enum BindError {
    Failure(&'static str),
    Listen {
        address: SocketAddr,
        kind: io::ErrorKind,
        selected_port: Option<u16>,
    },
}

impl BindError {
    fn code(&self) -> &'static str {
        match self {
            Self::Failure(code) => code,
            Self::Listen { .. } => "tcp_bind_failed",
        }
    }
}
const MAX_CONNECTIONS: usize = 16;
const MAX_CONNECTIONS_PER_IP: usize = 8;
const ACCEPT_BUDGET: usize = 4;

pub enum Event {
    Request {
        connection: u64,
        ticket: u64,
        request: Request,
    },
    Disconnected {
        connection: u64,
        ticket: Option<u64>,
        reason: &'static str,
    },
}

pub struct Transport {
    network: Network,
    listeners: Vec<TcpListener>,
    tickets: Tickets,
    connections: BTreeMap<u64, Connection>,
    next_connection: u64,
    now: u64,
    allow_remote: bool,
}

impl Transport {
    #[cfg(test)]
    pub fn bind(address: SocketAddr, allow_remote: bool) -> TransportResult<Self> {
        Self::bind_all(vec![address], allow_remote)
    }

    #[cfg(test)]
    pub fn bind_all(addresses: Vec<SocketAddr>, allow_remote: bool) -> TransportResult<Self> {
        Self::bind_all_with_network(Network::test_provider(), addresses, allow_remote)
    }

    pub fn bind_all_with_network(
        network: Network,
        addresses: Vec<SocketAddr>,
        allow_remote: bool,
    ) -> TransportResult<Self> {
        Self::try_bind_all(network, addresses, allow_remote).map_err(|error| error.code())
    }

    fn try_bind_all(
        network: Network,
        addresses: Vec<SocketAddr>,
        allow_remote: bool,
    ) -> Result<Self, BindError> {
        validate_listen_addresses(&addresses, allow_remote).map_err(BindError::Failure)?;
        let mut listeners = Vec::with_capacity(addresses.len());
        let mut selected_port = None;
        for mut address in addresses {
            if address.port() == 0
                && let Some(port) = selected_port
            {
                address.set_port(port);
            }
            // Socket ownership and platform listener semantics belong to the
            // selected provider; RTSP retains address admission and tickets.
            let listener = network
                .bind_tcp(address)
                .map_err(|error| BindError::Listen {
                    address,
                    kind: error.kind(),
                    selected_port,
                })?;
            listener
                .set_nonblocking(true)
                .map_err(|_| BindError::Failure("tcp_configuration_failed"))?;
            if selected_port.is_none() {
                selected_port = Some(
                    listener
                        .local_addr()
                        .map_err(|_| BindError::Failure("tcp_address_failed"))?
                        .port(),
                );
            }
            listeners.push(listener);
        }
        Ok(Self {
            network,
            listeners,
            tickets: Tickets::default(),
            connections: BTreeMap::new(),
            next_connection: 0,
            now: 0,
            allow_remote,
        })
    }

    #[cfg(test)]
    pub fn local_addr(&self) -> TransportResult<SocketAddr> {
        self.local_addrs()?
            .into_iter()
            .next()
            .ok_or("tcp_address_failed")
    }

    pub fn local_addrs(&self) -> TransportResult<Vec<SocketAddr>> {
        self.listeners
            .iter()
            .map(|listener| listener.local_addr().map_err(|_| "tcp_address_failed"))
            .collect()
    }

    /// Replaces every listener while preserving ticket history and crypto state.
    /// Callers defer network changes until active connections are drained.
    pub fn rebind_all(
        &mut self,
        addresses: Vec<SocketAddr>,
        allow_remote: bool,
    ) -> TransportResult<()> {
        if self.connection_count() != 0 {
            return Err("tcp_listener_active");
        }
        // Invalid configuration is not a bind collision. Reject it before
        // releasing listeners, including connections already in their backlog.
        validate_listen_addresses(&addresses, allow_remote)?;
        let previous_addresses = self.local_addrs()?;
        match Self::try_bind_all(self.network.clone(), addresses.clone(), allow_remote) {
            Ok(replacement) => {
                self.listeners = replacement.listeners;
                self.allow_remote = allow_remote;
                Ok(())
            }
            Err(preflight_error) => {
                if !matches!(&preflight_error, BindError::Listen { address, kind: io::ErrorKind::AddrInUse, .. }
                    if previous_addresses.iter().any(|previous| listeners_overlap(*previous, *address)))
                {
                    return Err(preflight_error.code());
                }
                // A self-collision may hide a later independent failure. Probe
                // every non-overlapping target and reject conflicting targets
                // before releasing any old listener or its accept backlog.
                let selected_port = match &preflight_error {
                    BindError::Listen { selected_port, .. } => *selected_port,
                    BindError::Failure(_) => None,
                }
                .or_else(|| (addresses[0].port() != 0).then_some(addresses[0].port()));
                let effective: Vec<_> = addresses
                    .iter()
                    .copied()
                    .map(|mut address| {
                        if address.port() == 0
                            && let Some(port) = selected_port
                        {
                            address.set_port(port);
                        }
                        address
                    })
                    .collect();
                if effective.iter().enumerate().any(|(index, address)| {
                    effective[..index]
                        .iter()
                        .any(|earlier| listeners_overlap(*earlier, *address))
                }) {
                    return Err("tcp_bind_failed");
                }
                let mut probes = Vec::new();
                for address in effective {
                    if !previous_addresses
                        .iter()
                        .any(|previous| listeners_overlap(*previous, address))
                    {
                        probes.push(
                            Self::try_bind_all(self.network.clone(), vec![address], allow_remote)
                                .map_err(|error| error.code())?,
                        );
                    }
                }
                drop(probes);
                // Same-port wildcard expansion can overlap an existing specific
                // bind. Release all listeners together, retry, and restore the
                // old set if any member of the new set still fails.
                drop(std::mem::take(&mut self.listeners));
                match Self::bind_all_with_network(self.network.clone(), addresses, allow_remote) {
                    Ok(replacement) => {
                        self.listeners = replacement.listeners;
                        self.allow_remote = allow_remote;
                        Ok(())
                    }
                    Err(replacement_error) => {
                        self.listeners = Self::bind_all_with_network(
                            self.network.clone(),
                            previous_addresses,
                            self.allow_remote,
                        )
                        .map_err(|_| "tcp_rebind_rollback_failed")?
                        .listeners;
                        Err(replacement_error)
                    }
                }
            }
        }
    }

    pub fn add_ticket(&mut self, id: u64, peer: IpAddr, key: &[u8; 16]) -> TransportResult<()> {
        if !valid_peer(peer) || (!self.allow_remote && !peer.is_loopback()) {
            return Err("invalid_peer_address");
        }
        self.tickets.add(id, peer, key)
    }

    /// Revocation removes active crypto state and closes all bound connections.
    /// Unauthenticated connections cannot select a revoked ticket afterward.
    pub fn remove_ticket(&mut self, id: u64) -> Vec<Event> {
        self.tickets.remove(id);
        let ids: Vec<_> = self
            .connections
            .iter()
            .filter(|(_, connection)| connection.ticket() == Some(id))
            .map(|(&id, _)| id)
            .collect();
        ids.into_iter()
            .map(|connection| self.close(connection, "ticket_revoked"))
            .collect()
    }

    /// Plaintext must be a complete response produced by the RTSP serializer.
    /// Admission precedes sealing; rejected duplicate replies consume no nonce.
    pub fn reply(&mut self, connection: u64, plaintext: &[u8]) -> TransportResult<()> {
        if plaintext.is_empty() || plaintext.len() > MAX_RTSP_PLAINTEXT_BYTES {
            return Err("invalid_response_size");
        }
        let current = self
            .connections
            .get(&connection)
            .ok_or("connection_not_found")?;
        if !current.awaiting_reply() {
            return Err("connection_not_waiting");
        }
        let ticket = current.ticket().ok_or("connection_not_authenticated")?;
        let encrypted = self.tickets.seal(ticket, plaintext)?;
        self.connections
            .get_mut(&connection)
            .expect("validated connection")
            .reply(encrypted, self.now);
        Ok(())
    }

    pub fn poll(&mut self, now_ms: u64) -> Vec<Event> {
        self.now = self.now.max(now_ms);
        self.accept();
        let ids: Vec<_> = self.connections.keys().copied().collect();
        let mut events = Vec::new();
        for id in ids {
            let step = self
                .connections
                .get_mut(&id)
                .expect("enumerated connection")
                .step(&mut self.tickets, self.now);
            match step {
                Step::Pending => {}
                Step::Request { ticket, request } => events.push(Event::Request {
                    connection: id,
                    ticket,
                    request,
                }),
                Step::Close(reason) => events.push(self.close(id, reason)),
            }
        }
        events
    }

    pub fn connection_count(&self) -> usize {
        self.connections.len()
    }

    fn accept(&mut self) {
        for listener in &self.listeners {
            for _ in 0..ACCEPT_BUDGET {
                let (socket, peer) = match listener.accept() {
                    Ok(accepted) => accepted,
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                };
                if self.connections.len() >= MAX_CONNECTIONS
                    || self
                        .connections
                        .values()
                        .filter(|connection| connection.peer() == peer.ip())
                        .count()
                        >= MAX_CONNECTIONS_PER_IP
                    || (!self.allow_remote && !peer.ip().is_loopback())
                    || !self.tickets.has_peer(peer.ip())
                {
                    continue;
                }
                let Some(id) = self.next_connection.checked_add(1) else {
                    continue;
                };
                let Ok(connection) = Connection::new(socket, peer.ip(), self.now) else {
                    continue;
                };
                eprintln!("amaterasu rtsp connection accepted");
                self.next_connection = id;
                self.connections.insert(id, connection);
            }
        }
    }

    fn close(&mut self, id: u64, reason: &'static str) -> Event {
        let connection = self.connections.remove(&id).expect("owned connection");
        Event::Disconnected {
            connection: id,
            ticket: connection.ticket(),
            reason,
        }
    }
}

fn listeners_overlap(first: SocketAddr, second: SocketAddr) -> bool {
    first.port() == second.port()
        && first.port() != 0
        && first.is_ipv4() == second.is_ipv4()
        && (first.ip() == second.ip()
            || first.ip().is_unspecified()
            || second.ip().is_unspecified())
}

fn validate_listen_addresses(addresses: &[SocketAddr], allow_remote: bool) -> TransportResult<()> {
    if addresses.is_empty()
        || addresses.iter().any(|address| {
            mapped(address.ip())
                || address.ip().is_multicast()
                || (!allow_remote && !address.ip().is_loopback())
        })
    {
        Err("invalid_listen_address")
    } else {
        Ok(())
    }
}

fn mapped(address: IpAddr) -> bool {
    matches!(address, IpAddr::V6(address) if address.to_ipv4_mapped().is_some())
}
fn valid_peer(address: IpAddr) -> bool {
    !mapped(address)
        && !address.is_unspecified()
        && !address.is_multicast()
        && address != IpAddr::V4(std::net::Ipv4Addr::BROADCAST)
}

#[cfg(test)]
mod tests;
