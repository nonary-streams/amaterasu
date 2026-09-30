use socket2::{Domain, Protocol, Socket, Type};
use std::{
    io,
    net::{SocketAddr, UdpSocket},
};

/// Binds exactly the requested address without sharing the port. IPv6 sockets
/// accept IPv6 only, matching the separate IPv4 and IPv6 listener policy.
pub fn bind_udp(address: SocketAddr) -> io::Result<UdpSocket> {
    let socket = Socket::new(
        Domain::for_address(address),
        Type::DGRAM,
        Some(Protocol::UDP),
    )?;
    if address.is_ipv6() {
        socket.set_only_v6(true)?;
    }
    socket.bind(&address.into())?;
    socket.set_nonblocking(true)?;
    Ok(socket.into())
}

#[cfg(test)]
mod tests;
