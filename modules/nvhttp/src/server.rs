//! Single-threaded nonblocking sockets with bounded request and response buffers.
use crate::{
    settings::Settings,
    wire::{Reply, Request},
};
use rustls::{ServerConfig, ServerConnection};
use std::{
    collections::BTreeMap,
    io::{self, Read, Write},
    net::{IpAddr, SocketAddr, TcpListener, TcpStream},
    sync::Arc,
    time::{Duration, Instant},
};

const MAX_CONNECTIONS: usize = 32;
const MAX_PER_PEER: usize = 8;
const MAX_REQUEST: usize = 32 * 1024;
const MAX_REPLY: usize = 1024 * 1024;
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
    http: TcpListener,
    https: TcpListener,
    connections: BTreeMap<u64, Connection>,
    next_id: u64,
    configuration: Option<Arc<ServerConfig>>,
    tls_ready: bool,
}

impl Server {
    pub fn bind(settings: &Settings) -> Result<Self, String> {
        let http =
            TcpListener::bind(settings.http_bind).map_err(|_| "HTTP listener unavailable")?;
        let https =
            TcpListener::bind(settings.https_bind).map_err(|_| "HTTPS listener unavailable")?;
        http.set_nonblocking(true)
            .map_err(|_| "HTTP nonblocking setup failed")?;
        https
            .set_nonblocking(true)
            .map_err(|_| "HTTPS nonblocking setup failed")?;
        Ok(Self {
            http,
            https,
            connections: BTreeMap::new(),
            next_id: 0,
            configuration: None,
            tls_ready: false,
        })
    }

    pub fn http_address(&self) -> SocketAddr {
        self.http.local_addr().expect("bound HTTP listener")
    }
    pub fn https_address(&self) -> SocketAddr {
        self.https.local_addr().expect("bound HTTPS listener")
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

    pub fn tick(&mut self) -> Events {
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
            match connection.tick() {
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
        let listener = if secure { &self.https } else { &self.http };
        for _ in 0..4 {
            let (socket, remote) = match listener.accept() {
                Ok(value) => value,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(_) => break,
            };
            let peer_count = self
                .connections
                .values()
                .filter(|connection| connection.remote == remote.ip())
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
                    remote: remote.ip(),
                    tls,
                    input: Vec::new(),
                    output: None,
                    waiting: false,
                    finished: false,
                    closing: false,
                    response_delivered: false,
                    deadline: Instant::now() + IO_TIMEOUT,
                },
            );
        }
    }

    pub fn respond_checked(
        &mut self,
        id: u64,
        reply: Reply,
        verify: impl FnOnce(&[u8]) -> bool,
    ) -> bool {
        let paired = if reply.requires_paired_tls {
            self.connections
                .get(&id)
                .and_then(|connection| connection.tls.as_ref())
                .filter(|tls| !tls.is_handshaking())
                .and_then(|tls| tls.peer_certificates())
                .and_then(|certificates| certificates.first())
                .is_some_and(|certificate| verify(certificate.as_ref()))
        } else {
            false
        };
        let authorized = !reply.requires_paired_tls || paired;
        self.respond(id, reply.authorize(paired)) && authorized
    }

    pub fn respond(&mut self, id: u64, reply: Reply) -> bool {
        if let Some(connection) = self.connections.get_mut(&id) {
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

struct Output {
    bytes: Vec<u8>,
    offset: usize,
}
struct Connection {
    socket: TcpStream,
    remote: IpAddr,
    tls: Option<ServerConnection>,
    input: Vec<u8>,
    output: Option<Output>,
    waiting: bool,
    finished: bool,
    closing: bool,
    response_delivered: bool,
    deadline: Instant,
}

impl Connection {
    fn tick(&mut self) -> io::Result<Option<Request>> {
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
                self.finished = true;
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
        match parse_request(&self.input) {
            Ok(Some(request)) => {
                self.input.clear();
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

    fn tls_io(&mut self) -> io::Result<()> {
        let tls = self.tls.as_mut().expect("TLS connection");
        // One bounded socket read and write per tick, never complete_io's loop.
        // Application plaintext is read only while parsing the single request.
        if tls.wants_read() && self.output.is_none() {
            let read = tls.read_tls(&mut (&mut self.socket).take(IO_BUDGET as u64));
            match read {
                Ok(0) => self.finished = true,
                Ok(_) => {
                    let state = tls
                        .process_new_packets()
                        .map_err(|_| io::Error::other("TLS protocol failure"))?;
                    if state.peer_has_closed() {
                        self.finished = true;
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
        self.output = Some(Output { bytes, offset: 0 });
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

struct BoundedWriter<'a> {
    socket: &'a mut TcpStream,
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

fn parse_request(bytes: &[u8]) -> Result<Option<Request>, u16> {
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
    if bytes.len() != total {
        return Err(400);
    }
    Ok(Some(Request {
        method: method.into(),
        target: target.into(),
        body: bytes[offset..total].to_vec(),
    }))
}

fn simple_reply(status: u16, message: &str) -> Reply {
    Reply {
        status,
        content_type: "text/plain; charset=utf-8",
        body: message.as_bytes().to_vec(),
        requires_paired_tls: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
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
                bytes: b"response".to_vec(),
                offset: 0,
            }),
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
}
