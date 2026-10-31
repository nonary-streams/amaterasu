//! HTTP client adapter using the selected network provider for DNS and byte I/O.
//!
//! HTTP framing, redirects, proxy CONNECT and certificate validation remain in
//! ureq. This module intentionally installs no default TCP connector or resolver.
//! The ureq extension API is unversioned, so the optional dependency is pinned.
use crate::network::{Network, TcpStream};
use std::fmt;
use std::io::{self, Read, Write};
use std::time::{Duration, Instant};
use ureq::config::Config;
use ureq::unversioned::resolver::{ResolvedSocketAddrs, Resolver};
use ureq::unversioned::transport::{
    Buffers, ConnectProxyConnector, ConnectionDetails, Connector, Either, LazyBuffers, NextTimeout,
    RustlsConnector, Transport,
};
use ureq::{Agent, Error, ProxyProtocol};

/// Create an HTTP/HTTPS client with provider-backed DNS, TCP and TLS byte I/O.
/// The network binding is retained by every pooled connection and worker clone.
pub fn agent(config: Config, network: Network) -> Agent {
    let connector =
        ().chain(ConnectProxyConnector::default())
            .chain(ProviderConnector(network.clone()))
            .chain(RustlsConnector::default());
    Agent::with_parts(config, connector, ProviderResolver(network))
}

#[derive(Clone)]
struct ProviderResolver(Network);
impl fmt::Debug for ProviderResolver {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ProviderResolver")
    }
}
impl Resolver for ProviderResolver {
    fn resolve(
        &self,
        uri: &ureq::http::Uri,
        config: &Config,
        timeout: NextTimeout,
    ) -> Result<ResolvedSocketAddrs, Error> {
        let host = uri.host().ok_or(Error::HostNotFound)?;
        let host = host
            .strip_prefix('[')
            .and_then(|value| value.strip_suffix(']'))
            .unwrap_or(host);
        let port = uri
            .port_u16()
            .or_else(|| match uri.scheme_str() {
                Some("https") => Some(443),
                Some("http") => Some(80),
                _ => None,
            })
            .ok_or(Error::HostNotFound)?;
        let duration = bounded_duration(timeout)?;
        let addresses = self
            .0
            .resolve_timeout(host, port, duration)
            .map_err(|error| timeout_error(error, timeout))?;
        let mut result = self.empty();
        for address in config
            .ip_family()
            .keep_wanted(addresses.into_iter())
            .take(16)
        {
            result.push(address);
        }
        if result.is_empty() {
            Err(Error::HostNotFound)
        } else {
            Ok(result)
        }
    }
}

#[derive(Clone)]
struct ProviderConnector(Network);
impl fmt::Debug for ProviderConnector {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ProviderConnector")
    }
}
impl<Incoming: Transport> Connector<Incoming> for ProviderConnector {
    type Out = Either<Incoming, ProviderTransport>;
    fn connect(
        &self,
        details: &ConnectionDetails,
        chained: Option<Incoming>,
    ) -> Result<Option<Self::Out>, Error> {
        if let Some(transport) = chained {
            return Ok(Some(Either::A(transport)));
        }
        if details.config.proxy().is_some_and(|proxy| {
            !matches!(proxy.protocol(), ProxyProtocol::Http | ProxyProtocol::Https)
                && !proxy.is_no_proxy(details.uri)
        }) {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "network HTTP adapter does not negotiate SOCKS",
            )
            .into());
        }
        let started = Instant::now();
        let budget = bounded_duration(details.timeout)?;
        let mut last_error = None;
        for (index, address) in details.addrs.iter().enumerate() {
            let remaining = budget
                .checked_sub(started.elapsed())
                .filter(|duration| !duration.is_zero())
                .ok_or(Error::Timeout(details.timeout.reason))?;
            // Reserve part of the remaining deadline for alternative addresses;
            // an unreachable IPv6 route must not consume the whole IPv4 budget.
            let remaining_addresses = details.addrs.len() - index;
            let per_address = if remaining_addresses > 1 {
                remaining / 2
            } else {
                remaining
            };
            match self.0.connect_tcp(*address, per_address) {
                Ok(stream) => {
                    if details.config.no_delay() {
                        stream.set_nodelay(true)?;
                    }
                    return Ok(Some(Either::B(ProviderTransport {
                        stream,
                        buffers: LazyBuffers::new(
                            details.config.input_buffer_size(),
                            details.config.output_buffer_size(),
                        ),
                    })));
                }
                Err(error) if retryable_connect(&error) => last_error = Some(error),
                Err(error) => return Err(error.into()),
            }
        }
        Err(timeout_error(
            last_error.unwrap_or_else(|| {
                io::Error::new(io::ErrorKind::NotFound, "provider returned no addresses")
            }),
            details.timeout,
        ))
    }
}

struct ProviderTransport {
    stream: TcpStream,
    buffers: LazyBuffers,
}
impl fmt::Debug for ProviderTransport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ProviderTransport")
    }
}
impl Transport for ProviderTransport {
    fn buffers(&mut self) -> &mut dyn Buffers {
        &mut self.buffers
    }
    fn transmit_output(&mut self, amount: usize, timeout: NextTimeout) -> Result<(), Error> {
        let started = Instant::now();
        let budget = optional_duration(timeout)?;
        let mut sent = 0;
        while sent < amount {
            let remaining = budget.map(|budget| budget.saturating_sub(started.elapsed()));
            if remaining.is_some_and(|remaining| remaining.is_zero()) {
                return Err(Error::Timeout(timeout.reason));
            }
            self.stream.set_write_timeout(remaining)?;
            match self.stream.write(&self.buffers.output()[sent..amount]) {
                Ok(0) => return Err(io::Error::from(io::ErrorKind::WriteZero).into()),
                Ok(count) => sent += count,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(timeout_error(error, timeout)),
            }
        }
        Ok(())
    }
    fn await_input(&mut self, timeout: NextTimeout) -> Result<bool, Error> {
        self.stream.set_read_timeout(optional_duration(timeout)?)?;
        let bytes = self
            .stream
            .read(self.buffers.input_append_buf())
            .map_err(|error| timeout_error(error, timeout))?;
        self.buffers.input_appended(bytes);
        Ok(bytes != 0)
    }
    fn is_open(&mut self) -> bool {
        if self.stream.set_nonblocking(true).is_err() {
            return false;
        }
        let mut byte = [0];
        let pending = self.stream.read(&mut byte);
        let restored = self.stream.set_nonblocking(false).is_ok();
        restored && matches!(pending, Err(error) if error.kind() == io::ErrorKind::WouldBlock)
    }
}
fn optional_duration(timeout: NextTimeout) -> Result<Option<Duration>, Error> {
    if timeout.after.is_not_happening() {
        return Ok(None);
    }
    let duration = *timeout.after;
    if duration.is_zero() {
        return Err(Error::Timeout(timeout.reason));
    }
    Ok(Some(duration))
}
fn bounded_duration(timeout: NextTimeout) -> Result<Duration, Error> {
    Ok(optional_duration(timeout)?.unwrap_or(Duration::from_secs(60)))
}
fn timeout_error(error: io::Error, timeout: NextTimeout) -> Error {
    if matches!(
        error.kind(),
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
    ) {
        Error::Timeout(timeout.reason)
    } else {
        error.into()
    }
}
fn retryable_connect(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::ConnectionRefused
            | io::ErrorKind::AddrNotAvailable
            | io::ErrorKind::HostUnreachable
            | io::ErrorKind::NetworkUnreachable
            | io::ErrorKind::TimedOut
            | io::ErrorKind::WouldBlock
    )
}

#[cfg(test)]
mod tests;
