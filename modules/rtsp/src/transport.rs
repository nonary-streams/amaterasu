//! Encrypted, one-transaction-per-connection RTSP transport. Ticket cryptographic
//! state survives TCP connections; the caller owns ticket authorization and routing.
mod connection;
mod tickets;

use amaterasu_gamestream::{crypto::MAX_RTSP_PLAINTEXT_BYTES, rtsp::Request};
use connection::{Connection, Step};
use std::{
    collections::BTreeMap,
    io,
    net::{IpAddr, SocketAddr, TcpListener},
};
use tickets::Tickets;

pub type TransportResult<T> = Result<T, &'static str>;
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
    listener: TcpListener,
    tickets: Tickets,
    connections: BTreeMap<u64, Connection>,
    next_connection: u64,
    now: u64,
    allow_remote: bool,
}

impl Transport {
    pub fn bind(address: SocketAddr, allow_remote: bool) -> TransportResult<Self> {
        if mapped(address.ip())
            || address.ip().is_multicast()
            || (!allow_remote && !address.ip().is_loopback())
        {
            return Err("invalid_listen_address");
        }
        let listener = TcpListener::bind(address).map_err(|_| "tcp_bind_failed")?;
        listener
            .set_nonblocking(true)
            .map_err(|_| "tcp_configuration_failed")?;
        Ok(Self {
            listener,
            tickets: Tickets::default(),
            connections: BTreeMap::new(),
            next_connection: 0,
            now: 0,
            allow_remote,
        })
    }

    pub fn local_addr(&self) -> TransportResult<SocketAddr> {
        self.listener.local_addr().map_err(|_| "tcp_address_failed")
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
        for _ in 0..ACCEPT_BUDGET {
            let (socket, peer) = match self.listener.accept() {
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
            self.next_connection = id;
            self.connections.insert(id, connection);
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
