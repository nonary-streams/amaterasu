use super::{Network, abi, address_from_native, address_to_native, check, timeout_ms};
use std::{
    io::{self, Read, Write},
    net::{Ipv6Addr, Shutdown, SocketAddr},
    sync::Arc,
    time::Duration,
};
#[derive(Debug)]
struct Handle {
    network: Network,
    id: u64,
}
impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: final local resource handle owns close; Network guard retains table.
        let _ = unsafe { (self.network.table().close)(self.network.context(), self.id) };
    }
}
impl Handle {
    fn new(network: Network, id: u64) -> io::Result<Arc<Self>> {
        if id == 0 {
            return Err(io::ErrorKind::InvalidData.into());
        }
        Ok(Arc::new(Self { network, id }))
    }
    fn address(&self, peer: bool) -> io::Result<SocketAddr> {
        let mut address = abi::Address::default();
        let net = &self.network;
        // SAFETY: live provider binding/ID and writable value output.
        let status = unsafe {
            if peer {
                (net.table().peer_addr)(net.context(), self.id, &mut address)
            } else {
                (net.table().local_addr)(net.context(), self.id, &mut address)
            }
        };
        check(status)?;
        address_from_native(address)
    }
    fn option(&self, option: u32, value: u64) -> io::Result<()> {
        // SAFETY: live binding and opaque owned handle; option is a value.
        check(unsafe {
            (self.network.table().set_option)(self.network.context(), self.id, option, value)
        })
    }
    fn get_option(&self, option: u32) -> io::Result<u64> {
        let mut value = 0;
        // SAFETY: value output is writable during callback.
        check(unsafe {
            (self.network.table().get_option)(self.network.context(), self.id, option, &mut value)
        })?;
        Ok(value)
    }
    fn read(&self, buffer: &mut [u8], peek: bool) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        let size = buffer.len().min(self.network.maximum_io());
        let mut count = 0;
        let net = &self.network;
        // SAFETY: bounded writable buffer is held for the synchronous callback.
        check(unsafe {
            if peek {
                (net.table().peek)(
                    net.context(),
                    self.id,
                    buffer.as_mut_ptr(),
                    size,
                    &mut count,
                )
            } else {
                (net.table().read)(
                    net.context(),
                    self.id,
                    buffer.as_mut_ptr(),
                    size,
                    &mut count,
                )
            }
        })?;
        checked_count(count, size)
    }
    fn write(&self, buffer: &[u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        let size = buffer.len().min(self.network.maximum_io());
        let mut count = 0;
        let net = &self.network;
        // SAFETY: bounded readable bytes and writable count live for callback.
        check(unsafe {
            (net.table().write)(net.context(), self.id, buffer.as_ptr(), size, &mut count)
        })?;
        checked_count(count, size)
    }
    fn timeout(&self, option: u32, value: Option<Duration>) -> io::Result<()> {
        self.option(
            option,
            match value {
                Some(value) => u64::from(timeout_ms(value)?),
                None => 0,
            },
        )
    }
}
fn checked_count(count: usize, maximum: usize) -> io::Result<usize> {
    if count > maximum {
        Err(io::ErrorKind::InvalidData.into())
    } else {
        Ok(count)
    }
}

#[derive(Clone, Debug)]
pub struct TcpListener(Arc<Handle>);
impl TcpListener {
    pub(super) fn new(network: Network, id: u64) -> io::Result<Self> {
        Handle::new(network, id).map(Self)
    }
    pub fn accept(&self) -> io::Result<(TcpStream, SocketAddr)> {
        let mut id = 0;
        let mut address = abi::Address::default();
        let net = &self.0.network;
        // SAFETY: all output values are writable and listener guard remains live.
        check(unsafe { (net.table().accept)(net.context(), self.0.id, &mut id, &mut address) })?;
        let stream = TcpStream::new(net.clone(), id)?;
        Ok((stream, address_from_native(address)?))
    }
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.0.address(false)
    }
    pub fn set_nonblocking(&self, value: bool) -> io::Result<()> {
        self.0.option(abi::NONBLOCKING, u64::from(value))
    }
    pub fn try_clone(&self) -> io::Result<Self> {
        Ok(self.clone())
    }
}

#[derive(Clone, Debug)]
pub struct TcpStream(Arc<Handle>);
impl TcpStream {
    pub(super) fn new(network: Network, id: u64) -> io::Result<Self> {
        Handle::new(network, id).map(Self)
    }
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.0.address(false)
    }
    pub fn peer_addr(&self) -> io::Result<SocketAddr> {
        self.0.address(true)
    }
    pub fn set_nonblocking(&self, value: bool) -> io::Result<()> {
        self.0.option(abi::NONBLOCKING, u64::from(value))
    }
    pub fn set_read_timeout(&self, value: Option<Duration>) -> io::Result<()> {
        self.0.timeout(abi::READ_TIMEOUT_MS, value)
    }
    pub fn set_write_timeout(&self, value: Option<Duration>) -> io::Result<()> {
        self.0.timeout(abi::WRITE_TIMEOUT_MS, value)
    }
    pub fn set_nodelay(&self, value: bool) -> io::Result<()> {
        self.0.option(abi::NODELAY, u64::from(value))
    }
    pub fn nodelay(&self) -> io::Result<bool> {
        self.0.get_option(abi::NODELAY).map(|v| v != 0)
    }
    pub fn peek(&self, buffer: &mut [u8]) -> io::Result<usize> {
        self.0.read(buffer, true)
    }
    pub fn shutdown(&self, how: Shutdown) -> io::Result<()> {
        let how = match how {
            Shutdown::Read => abi::SHUTDOWN_READ,
            Shutdown::Write => abi::SHUTDOWN_WRITE,
            Shutdown::Both => abi::SHUTDOWN_BOTH,
        };
        // SAFETY: live owned handle and provider, shutdown mode is a validated value.
        check(unsafe {
            (self.0.network.table().shutdown)(self.0.network.context(), self.0.id, how)
        })
    }
    pub fn try_clone(&self) -> io::Result<Self> {
        Ok(self.clone())
    }
}
impl Read for TcpStream {
    fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
        self.0.read(b, false)
    }
}
impl Read for &TcpStream {
    fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
        self.0.read(b, false)
    }
}
impl Write for TcpStream {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.0.write(b)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
impl Write for &TcpStream {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.0.write(b)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct UdpSocket(Arc<Handle>);
impl UdpSocket {
    pub(super) fn new(network: Network, id: u64) -> io::Result<Self> {
        Handle::new(network, id).map(Self)
    }
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.0.address(false)
    }
    pub fn peer_addr(&self) -> io::Result<SocketAddr> {
        self.0.address(true)
    }
    pub fn connect(&self, address: SocketAddr) -> io::Result<()> {
        // SAFETY: live handle and copied address.
        check(unsafe {
            (self.0.network.table().connect_udp)(
                self.0.network.context(),
                self.0.id,
                address_to_native(address),
            )
        })
    }
    pub fn send(&self, buffer: &[u8]) -> io::Result<usize> {
        if buffer.len() > self.0.network.maximum_io() {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let net = &self.0.network;
        let mut count = 0;
        // SAFETY: even an empty buffer represents a real UDP datagram. Its
        // synchronous pointer and writable count remain live for the call.
        check(unsafe {
            (net.table().write)(
                net.context(),
                self.0.id,
                buffer.as_ptr(),
                buffer.len(),
                &mut count,
            )
        })?;
        checked_count(count, buffer.len())
    }
    pub fn recv(&self, buffer: &mut [u8]) -> io::Result<usize> {
        let net = &self.0.network;
        let size = buffer.len().min(net.maximum_io());
        let mut count = 0;
        // SAFETY: zero-capacity UDP reads still consume a datagram, unlike
        // zero-length TCP reads. Keep that operation visible to the provider.
        check(unsafe {
            (net.table().read)(
                net.context(),
                self.0.id,
                buffer.as_mut_ptr(),
                size,
                &mut count,
            )
        })?;
        checked_count(count, size)
    }
    pub fn peek(&self, buffer: &mut [u8]) -> io::Result<usize> {
        self.0.read(buffer, true)
    }
    pub fn send_to(&self, buffer: &[u8], address: SocketAddr) -> io::Result<usize> {
        let net = &self.0.network;
        if buffer.len() > net.maximum_io() {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let mut count = 0;
        // SAFETY: buffer is a readable synchronous borrow and count is writable.
        check(unsafe {
            (net.table().send_to)(
                net.context(),
                self.0.id,
                buffer.as_ptr(),
                buffer.len(),
                address_to_native(address),
                &mut count,
            )
        })?;
        checked_count(count, buffer.len())
    }
    pub fn recv_from(&self, buffer: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        let net = &self.0.network;
        let size = buffer.len().min(net.maximum_io());
        let mut count = 0;
        let mut address = abi::Address::default();
        // SAFETY: bounded mutable buffer and address/count outputs live during call.
        check(unsafe {
            (net.table().recv_from)(
                net.context(),
                self.0.id,
                buffer.as_mut_ptr(),
                size,
                &mut count,
                &mut address,
            )
        })?;
        Ok((checked_count(count, size)?, address_from_native(address)?))
    }
    pub fn send_batch(&self, datagrams: &[&[u8]]) -> io::Result<usize> {
        let net = &self.0.network;
        net.require(abi::BATCH)?;
        if datagrams.len() > net.capabilities().max_batch as usize
            || datagrams.iter().any(|b| b.len() > net.maximum_io())
        {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        if datagrams.is_empty() {
            return Ok(0);
        }
        let descriptors: Vec<_> = datagrams
            .iter()
            .map(|bytes| abi::Datagram {
                data: bytes.as_ptr(),
                len: bytes.len(),
            })
            .collect();
        let mut count = 0;
        // SAFETY: descriptor array and all payloads are synchronous immutable borrows.
        check(unsafe {
            (net.table().send_batch)(
                net.context(),
                self.0.id,
                descriptors.as_ptr(),
                descriptors.len(),
                &mut count,
            )
        })?;
        checked_count(count, datagrams.len())
    }
    pub fn set_nonblocking(&self, v: bool) -> io::Result<()> {
        self.0.option(abi::NONBLOCKING, u64::from(v))
    }
    pub fn set_read_timeout(&self, v: Option<Duration>) -> io::Result<()> {
        self.0.timeout(abi::READ_TIMEOUT_MS, v)
    }
    pub fn set_write_timeout(&self, v: Option<Duration>) -> io::Result<()> {
        self.0.timeout(abi::WRITE_TIMEOUT_MS, v)
    }
    pub fn set_broadcast(&self, v: bool) -> io::Result<()> {
        self.0.option(abi::BROADCAST, u64::from(v))
    }
    pub fn broadcast(&self) -> io::Result<bool> {
        self.0.get_option(abi::BROADCAST).map(|v| v != 0)
    }
    pub fn set_ttl(&self, v: u32) -> io::Result<()> {
        self.0.option(abi::TTL, u64::from(v))
    }
    pub fn ttl(&self) -> io::Result<u32> {
        u32::try_from(self.0.get_option(abi::TTL)?).map_err(|_| io::ErrorKind::InvalidData.into())
    }
    pub fn set_multicast_if_v6(&self, v: u32) -> io::Result<()> {
        self.0.option(abi::MULTICAST_IF_V6, u64::from(v))
    }
    pub fn set_multicast_hops_v6(&self, v: u32) -> io::Result<()> {
        self.0.option(abi::MULTICAST_HOPS_V6, u64::from(v))
    }
    pub fn set_multicast_loop_v4(&self, v: bool) -> io::Result<()> {
        self.0.option(abi::MULTICAST_LOOP_V4, u64::from(v))
    }
    pub fn set_multicast_loop_v6(&self, v: bool) -> io::Result<()> {
        self.0.option(abi::MULTICAST_LOOP_V6, u64::from(v))
    }
    pub fn set_multicast_ttl_v4(&self, v: u32) -> io::Result<()> {
        self.0.option(abi::MULTICAST_TTL_V4, u64::from(v))
    }
    pub fn join_multicast_v6(&self, group: &Ipv6Addr, interface: u32) -> io::Result<()> {
        self.multicast(group, interface, true)
    }
    pub fn leave_multicast_v6(&self, group: &Ipv6Addr, interface: u32) -> io::Result<()> {
        self.multicast(group, interface, false)
    }
    fn multicast(&self, group: &Ipv6Addr, interface: u32, join: bool) -> io::Result<()> {
        self.0.network.require(abi::MULTICAST)?;
        // SAFETY: group address is a copied value; binding and resource remain live.
        check(unsafe {
            (self.0.network.table().join_multicast)(
                self.0.network.context(),
                self.0.id,
                address_to_native(SocketAddr::from((*group, 0))),
                interface,
                join,
            )
        })
    }
    pub fn try_clone(&self) -> io::Result<Self> {
        Ok(self.clone())
    }
}
