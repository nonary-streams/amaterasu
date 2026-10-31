//! Provider-backed network SDK. Production code never constructs an OS socket
//! here. Every resource retains the selected provider and its native binding.
mod socket;
use crate::{ModuleResult, NativeBinding, RuntimeClient};
#[cfg(feature = "network-test-support")]
use amaterasu_abi::{AbiHeader, NativeInterfaceQuery};
use amaterasu_abi::{BorrowedBytes, STATUS_OK, Status};
pub use amaterasu_network_api as abi;
pub use socket::{TcpListener, TcpStream, UdpSocket};
use std::{
    ffi::c_void,
    io,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV6},
    sync::Arc,
    time::Duration,
};

pub fn valid_provider(service: &str) -> bool {
    amaterasu_contracts::valid_name(service) && service.ends_with("/1")
}

struct Inner {
    table: *const abi::NetworkApi,
    binding: Option<NativeBinding>,
}
// SAFETY: both constructors require the exact thread-safe NetworkApi contract.
// Runtime construction additionally requires a thread-safe live runtime endpoint.
// Every socket owns an Arc; calls borrow it, so release follows all table calls.
unsafe impl Send for Inner {}
unsafe impl Sync for Inner {}
impl Drop for Inner {
    fn drop(&mut self) {
        if self.binding.is_none() {
            // SAFETY: from_native_table transfers exactly one owned live table. No call
            // or child handle can remain after the last Arc is destroyed.
            unsafe {
                let base = (*self.table).base;
                (base.release.expect("validated network release"))(base.context);
            }
        }
    }
}

#[derive(Clone)]
pub struct Network {
    inner: Arc<Inner>,
    capabilities: abi::Capabilities,
}
impl std::fmt::Debug for Network {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Network")
            .field("features", &self.capabilities.features)
            .finish_non_exhaustive()
    }
}
#[derive(Clone, Copy, Debug)]
pub struct TcpListenOptions {
    pub backlog: u32,
    pub nonblocking: bool,
    pub reuse_address: bool,
    pub ipv6_only: bool,
}
impl Default for TcpListenOptions {
    fn default() -> Self {
        Self {
            backlog: 4096,
            nonblocking: true,
            reuse_address: cfg!(unix),
            ipv6_only: true,
        }
    }
}
impl Network {
    /// Select a versioned contract from trusted module startup configuration. A
    /// missing or denied provider is an error; there is no local socket fallback.
    /// The ManagedModule/runtime lifecycle must join consumer workers before stop.
    pub fn from_settings(
        runtime: &RuntimeClient,
        settings: &serde_json::Value,
    ) -> ModuleResult<Self> {
        let provider = match settings.get("network_provider") {
            None | Some(serde_json::Value::Null) => abi::SERVICE,
            Some(serde_json::Value::String(value)) if valid_provider(value) => value,
            _ => return Err("invalid network_provider contract".into()),
        };
        // SAFETY: module endpoints originate at the thread-safe native runtime; the
        // module lifecycle retains runtime/providers until consumer workers are joined.
        unsafe { Self::bind(runtime, provider) }
    }
    /// # Safety
    /// service must provide NetworkApi v1, all callbacks and runtime endpoint must
    /// be thread-safe, and runtime/provider code must outlive self and its sockets.
    /// Stop/join all consumers before unloading the provider DLL or runtime.
    pub unsafe fn bind(runtime: &RuntimeClient, service: &str) -> ModuleResult<Self> {
        if !valid_provider(service) {
            return Err("invalid network provider service".into());
        }
        // SAFETY: caller establishes the exact service contract and lifetime.
        let binding = unsafe {
            runtime.bind_native_typed(
                service,
                abi::CONTRACT,
                abi::MAJOR,
                abi::MINOR,
                size_of::<abi::NetworkApi>() as u32,
            )?
        };
        let table = unsafe { binding.table::<abi::NetworkApi>()? } as *const _;
        Self::new(Inner {
            table,
            binding: Some(binding),
        })
    }
    /// Takes ownership of one independently implemented native interface.
    ///
    /// # Safety
    /// table must be a fully initialized NetworkApi v1 with a release callback,
    /// readable until release. Its callbacks are thread-safe and honor every ABI
    /// ownership rule. Keep provider code loaded through all clones/socket guards.
    /// No other owner may call release. Intended for explicit provider composition
    /// and alternate-provider tests, never an implicit fallback after bind failure.
    pub unsafe fn from_native_table(table: *const abi::NetworkApi) -> ModuleResult<Self> {
        if table.is_null() || !(table as usize).is_multiple_of(align_of::<abi::NetworkApi>()) {
            return Err("invalid network table".into());
        }
        // SAFETY: caller provides a complete readable table.
        let base = unsafe { &(*table).base };
        if base.header.major != abi::MAJOR
            || base.header.size < size_of::<abi::NetworkApi>() as u32
            || base.release.is_none()
        {
            if base.header.size >= size_of::<amaterasu_abi::NativeInterface>() as u32
                && let Some(release) = base.release
            {
                let context = base.context;
                // SAFETY: a complete readable interface prefix owns its release callback,
                // even when the requested network version/tail is unsupported.
                unsafe { release(context) };
            }
            return Err("incompatible network table".into());
        }
        Self::new(Inner {
            table,
            binding: None,
        })
    }
    fn new(inner: Inner) -> ModuleResult<Self> {
        let mut caps = abi::Capabilities {
            size: 0,
            version: 0,
            features: 0,
            max_io_bytes: 0,
            max_handles: 0,
            max_batch: 0,
        };
        // SAFETY: constructors establish live table and writable capability storage.
        let status =
            unsafe { ((*inner.table).capabilities)((*inner.table).base.context, &mut caps) };
        if status != STATUS_OK
            || caps.version != 1
            || caps.size < size_of::<abi::Capabilities>() as u32
            || caps.max_io_bytes == 0
            || caps.max_io_bytes > abi::MAX_IO_BYTES as u64
            || caps.max_handles == 0
            || caps.max_batch > abi::MAX_BATCH as u32
        {
            return Err(format!("network capabilities rejected: {status}"));
        }
        Ok(Self {
            inner: Arc::new(inner),
            capabilities: caps,
        })
    }
    pub fn capabilities(&self) -> abi::Capabilities {
        self.capabilities
    }
    pub fn supports(&self, capability: u64) -> bool {
        self.capabilities.features & capability == capability
    }
    pub(super) fn table(&self) -> &abi::NetworkApi {
        // SAFETY: every Network owns the live interface via its Arc.
        unsafe { &*self.inner.table }
    }
    pub(super) fn context(&self) -> *mut c_void {
        self.table().base.context
    }
    pub(super) fn maximum_io(&self) -> usize {
        self.capabilities.max_io_bytes as usize
    }
    pub(super) fn require(&self, feature: u64) -> io::Result<()> {
        if self.supports(feature) {
            Ok(())
        } else {
            Err(io::ErrorKind::Unsupported.into())
        }
    }
    pub fn bind_tcp(&self, address: SocketAddr) -> io::Result<TcpListener> {
        self.bind_tcp_with_options(address, TcpListenOptions::default())
    }
    pub fn bind_tcp_with_options(
        &self,
        address: SocketAddr,
        options: TcpListenOptions,
    ) -> io::Result<TcpListener> {
        self.require(abi::TCP)?;
        if options.backlog == 0 || options.backlog > i32::MAX as u32 {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let flags = if options.nonblocking {
            abi::LISTEN_NONBLOCKING
        } else {
            0
        } | if options.reuse_address {
            abi::LISTEN_REUSE_ADDRESS
        } else {
            0
        } | if options.ipv6_only {
            abi::LISTEN_IPV6_ONLY
        } else {
            0
        };
        let options = abi::ListenOptions {
            size: size_of::<abi::ListenOptions>() as u32,
            flags,
            backlog: options.backlog,
            reserved: 0,
        };
        let mut id = 0;
        // SAFETY: address/options are values and output remains live for the call.
        check(unsafe {
            (self.table().tcp_listen)(self.context(), address_to_native(address), options, &mut id)
        })?;
        TcpListener::new(self.clone(), id)
    }
    pub fn connect_tcp(&self, address: SocketAddr, timeout: Duration) -> io::Result<TcpStream> {
        self.require(abi::TCP)?;
        let mut id = 0;
        let timeout = timeout_ms(timeout)?;
        // SAFETY: native synchronous operation, live binding and writable ID output.
        check(unsafe {
            (self.table().tcp_connect)(self.context(), address_to_native(address), timeout, &mut id)
        })?;
        TcpStream::new(self.clone(), id)
    }
    pub fn bind_udp(&self, address: SocketAddr) -> io::Result<UdpSocket> {
        self.require(abi::UDP)?;
        let mut id = 0;
        // SAFETY: copied address and writable output remain live during the call.
        check(unsafe {
            (self.table().udp_bind)(self.context(), address_to_native(address), &mut id)
        })?;
        UdpSocket::new(self.clone(), id)
    }
    pub fn resolve(&self, host: &str, port: u16) -> io::Result<Vec<SocketAddr>> {
        self.resolve_timeout(host, port, Duration::from_secs(10))
    }
    pub fn resolve_timeout(
        &self,
        host: &str,
        port: u16,
        timeout: Duration,
    ) -> io::Result<Vec<SocketAddr>> {
        self.require(abi::DNS)?;
        if host.is_empty() || host.len() > 253 || host.as_bytes().contains(&0) {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let timeout = timeout_ms(timeout)?;
        let mut addresses = [abi::Address::default(); abi::MAX_ADDRESSES];
        let mut count = 0;
        // SAFETY: borrowed hostname and output array live for this synchronous call.
        check(unsafe {
            (self.table().resolve)(
                self.context(),
                BorrowedBytes::new(host.as_bytes()),
                port,
                timeout,
                addresses.as_mut_ptr(),
                addresses.len(),
                &mut count,
            )
        })?;
        if count > addresses.len() {
            return Err(io::ErrorKind::InvalidData.into());
        }
        addresses[..count]
            .iter()
            .map(|address| address_from_native(*address))
            .collect()
    }
    pub fn interface_addresses(&self) -> io::Result<Vec<(IpAddr, u32)>> {
        self.require(abi::INTERFACES)?;
        let mut addresses = [abi::InterfaceAddress::default(); abi::MAX_ADDRESSES];
        let mut count = 0;
        // SAFETY: bounded writable output array retained throughout invocation.
        check(unsafe {
            (self.table().interfaces)(
                self.context(),
                addresses.as_mut_ptr(),
                addresses.len(),
                &mut count,
            )
        })?;
        if count > addresses.len() {
            return Err(io::ErrorKind::InvalidData.into());
        }
        addresses[..count]
            .iter()
            .map(|entry| {
                Ok((
                    address_from_native(entry.address)?.ip(),
                    entry.interface_index,
                ))
            })
            .collect()
    }
    /// Explicit default-provider injection for tests. It never participates in
    /// production provider selection and is absent without the opt-in feature.
    #[cfg(feature = "network-test-support")]
    pub fn test_provider() -> Self {
        let factory = amaterasu_network_sockets::Factory::new();
        let mut table = std::ptr::null();
        let query = NativeInterfaceQuery {
            header: AbiHeader::of::<NativeInterfaceQuery>(),
            interface_major: abi::MAJOR,
            min_minor: abi::MINOR,
            min_size: size_of::<abi::NetworkApi>() as u32,
        };
        // SAFETY: explicit test provider constructs a live owned binding. Its table
        // owns provider state, so factory metadata may drop after this callback.
        let status = unsafe {
            (factory.api.bind.expect("test provider bind"))(
                factory.api.context,
                BorrowedBytes::new(b"sdk.test"),
                &query,
                &mut table,
            )
        };
        assert_eq!(status, STATUS_OK);
        unsafe { Self::from_native_table(table.cast()).expect("explicit test network provider") }
    }
}
pub(super) fn timeout_ms(timeout: Duration) -> io::Result<u32> {
    if timeout.is_zero() {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    u32::try_from(timeout.as_millis().max(1)).map_err(|_| io::ErrorKind::InvalidInput.into())
}
pub(super) fn check(status: Status) -> io::Result<()> {
    if status == STATUS_OK {
        return Ok(());
    }
    Err(io::Error::new(
        match status {
            abi::WOULD_BLOCK => io::ErrorKind::WouldBlock,
            abi::TIMED_OUT => io::ErrorKind::TimedOut,
            abi::INTERRUPTED => io::ErrorKind::Interrupted,
            abi::CONNECTION_REFUSED => io::ErrorKind::ConnectionRefused,
            abi::CONNECTION_RESET => io::ErrorKind::ConnectionReset,
            abi::CONNECTION_ABORTED => io::ErrorKind::ConnectionAborted,
            abi::NOT_CONNECTED => io::ErrorKind::NotConnected,
            abi::ADDRESS_IN_USE => io::ErrorKind::AddrInUse,
            abi::ADDRESS_UNAVAILABLE => io::ErrorKind::AddrNotAvailable,
            abi::PERMISSION_DENIED => io::ErrorKind::PermissionDenied,
            abi::UNSUPPORTED => io::ErrorKind::Unsupported,
            abi::NOT_FOUND => io::ErrorKind::NotFound,
            abi::BROKEN_PIPE => io::ErrorKind::BrokenPipe,
            abi::INVALID_DATA => io::ErrorKind::InvalidData,
            abi::HOST_UNREACHABLE => io::ErrorKind::HostUnreachable,
            abi::NETWORK_UNREACHABLE => io::ErrorKind::NetworkUnreachable,
            amaterasu_abi::STATUS_INVALID_ARGUMENT => io::ErrorKind::InvalidInput,
            _ => io::ErrorKind::Other,
        },
        format!("network provider status {status}"),
    ))
}
pub fn address_to_native(address: SocketAddr) -> abi::Address {
    match address {
        SocketAddr::V4(address) => {
            let mut bytes = [0; 16];
            bytes[..4].copy_from_slice(&address.ip().octets());
            abi::Address {
                family: 4,
                port: address.port(),
                bytes,
                ..Default::default()
            }
        }
        SocketAddr::V6(address) => abi::Address {
            family: 6,
            port: address.port(),
            scope_id: address.scope_id(),
            flowinfo: address.flowinfo(),
            bytes: address.ip().octets(),
            reserved: 0,
        },
    }
}
pub fn address_from_native(address: abi::Address) -> io::Result<SocketAddr> {
    if address.reserved != 0 {
        return Err(io::ErrorKind::InvalidData.into());
    }
    match address.family {
        4 if address.scope_id == 0
            && address.flowinfo == 0
            && address.bytes[4..].iter().all(|b| *b == 0) =>
        {
            Ok(SocketAddr::from((
                Ipv4Addr::new(
                    address.bytes[0],
                    address.bytes[1],
                    address.bytes[2],
                    address.bytes[3],
                ),
                address.port,
            )))
        }
        6 => Ok(SocketAddr::V6(SocketAddrV6::new(
            Ipv6Addr::from(address.bytes),
            address.port,
            address.flowinfo,
            address.scope_id,
        ))),
        _ => Err(io::ErrorKind::InvalidData.into()),
    }
}

#[cfg(all(test, feature = "network-test-support"))]
mod tests;

#[cfg(test)]
mod status_tests {
    use super::*;

    #[test]
    fn unreachable_statuses_preserve_connection_fallback_semantics() {
        assert_eq!(
            check(abi::HOST_UNREACHABLE).unwrap_err().kind(),
            io::ErrorKind::HostUnreachable
        );
        assert_eq!(
            check(abi::NETWORK_UNREACHABLE).unwrap_err().kind(),
            io::ErrorKind::NetworkUnreachable
        );
    }
}
