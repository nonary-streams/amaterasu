//! Default OS socket implementation of the public network contract.
//! Resources are confined to their runtime-bound native binding. No socket or
//! allocator crosses the ABI; callbacks borrow buffers only synchronously.
mod batch;
mod diagnostics;
mod interfaces;
mod resolver;
use amaterasu_abi::*;
use amaterasu_network_api as net;
use socket2::{Domain, Protocol, SockAddr, SockRef, Socket, Type};
use std::{
    collections::BTreeMap,
    ffi::c_void,
    io::{self, Read, Write},
    net::{
        IpAddr, Ipv4Addr, Ipv6Addr, Shutdown, SocketAddr, SocketAddrV6, TcpListener, TcpStream,
        UdpSocket,
    },
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU32, AtomicU64, Ordering},
    },
    time::Duration,
};

const DEFAULT_MAX_HANDLES: u32 = 1024;
struct Shared {
    max_handles: u32,
    handles: AtomicU32,
    resolver: resolver::Resolver,
}
struct Permit(Arc<Shared>);
impl Drop for Permit {
    fn drop(&mut self) {
        self.0.handles.fetch_sub(1, Ordering::AcqRel);
    }
}
enum SocketResource {
    Listener(TcpListener),
    Tcp(TcpStream),
    Udp(UdpSocket),
}
struct Resource {
    socket: SocketResource,
    _permit: Permit,
    #[cfg(windows)]
    send_healthy: Mutex<bool>,
}
#[repr(C)]
struct Binding {
    api: net::NetworkApiV1_1,
    shared: Arc<Shared>,
    resources: Mutex<BTreeMap<u64, Arc<Resource>>>,
    next: AtomicU64,
}
/// Keep this factory alive until runtime registration has ended and every
/// consumer binding has been released. DNS deadline workers are joined at final
/// factory teardown before provider code can be unloaded.
pub struct Factory {
    pub api: NativeServiceFactory,
    shared: Arc<Shared>,
}
impl Factory {
    pub fn new() -> Box<Self> {
        Self::with_limits(DEFAULT_MAX_HANDLES)
    }
    pub fn with_limits(max_handles: u32) -> Box<Self> {
        let mut factory = Box::new(Self {
            api: NativeServiceFactory {
                header: AbiHeader::of::<NativeServiceFactory>(),
                context: std::ptr::null_mut(),
                bind: Some(bind),
            },
            shared: Arc::new(Shared {
                max_handles: max_handles.clamp(1, 65536),
                handles: AtomicU32::new(0),
                resolver: resolver::Resolver::new(),
            }),
        });
        factory.api.context = (&mut *factory as *mut Self).cast();
        factory
    }
}
/// # Safety
/// Factory must outlive the returned table. Release table.base exactly once,
/// after every concurrent table call has finished.
pub unsafe fn bind_standalone(
    factory: &Factory,
    owner: &str,
) -> Result<*const net::NetworkApi, Status> {
    let query = NativeInterfaceQuery {
        header: AbiHeader::of::<NativeInterfaceQuery>(),
        interface_major: net::MAJOR,
        min_minor: net::MINOR,
        min_size: size_of::<net::NetworkApi>() as u32,
    };
    let mut output = std::ptr::null();
    let status = unsafe {
        bind(
            factory.api.context,
            BorrowedBytes::new(owner.as_bytes()),
            &query,
            &mut output,
        )
    };
    if status == STATUS_OK {
        Ok(output.cast())
    } else {
        Err(status)
    }
}
unsafe extern "C" fn bind(
    context: *mut c_void,
    principal: BorrowedBytes,
    query: *const NativeInterfaceQuery,
    output: *mut *const NativeInterface,
) -> Status {
    guarded(|| {
        if context.is_null() || query.is_null() || output.is_null() {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        // SAFETY: runtime retains factory and readable query, principal and output.
        let (factory, query) = unsafe {
            *output = std::ptr::null();
            (&*context.cast::<Factory>(), &*query)
        };
        if !query.header.supports::<NativeInterfaceQuery>()
            || query.interface_major != net::MAJOR
            || query.min_minor > net::DIAGNOSTICS_MINOR
            || query.min_size > size_of::<net::NetworkApiV1_1>() as u32
        {
            return Err(net::UNSUPPORTED);
        }
        let owner = unsafe { principal.read()? };
        if owner.is_empty()
            || owner.len() > 128
            || !owner
                .iter()
                .all(|b| b.is_ascii_alphanumeric() || b"._-/".contains(b))
        {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        let mut binding = Box::new(Binding {
            api: net::NetworkApiV1_1 {
                v1: net::NetworkApi {
                    base: NativeInterface {
                        header: AbiHeader {
                            size: size_of::<net::NetworkApiV1_1>() as u32,
                            major: net::MAJOR,
                            minor: net::DIAGNOSTICS_MINOR,
                        },
                        context: std::ptr::null_mut(),
                        release: Some(release),
                    },
                    capabilities,
                    resolve,
                    interfaces,
                    tcp_listen,
                    tcp_connect,
                    udp_bind,
                    accept,
                    local_addr,
                    peer_addr,
                    read,
                    peek,
                    write,
                    recv_from,
                    send_to,
                    connect_udp,
                    set_option,
                    get_option,
                    join_multicast,
                    send_batch,
                    shutdown,
                    close,
                },
                recv_from_timed: diagnostics::recv_from_timed,
                socket_diagnostics: diagnostics::socket_diagnostics,
            },
            shared: factory.shared.clone(),
            resources: Mutex::new(BTreeMap::new()),
            next: AtomicU64::new(1),
        });
        binding.api.base.context = (&mut *binding as *mut Binding).cast();
        unsafe { *output = Box::into_raw(binding).cast() };
        Ok(())
    })
}
unsafe extern "C" fn release(context: *mut c_void) {
    if catch_unwind(AssertUnwindSafe(|| {
        if context.is_null() {
            std::process::abort()
        } // SAFETY: runtime transfers unique ownership after all table calls end.
        drop(unsafe { Box::from_raw(context.cast::<Binding>()) });
    }))
    .is_err()
    {
        std::process::abort()
    }
}
unsafe fn binding<'a>(context: *mut c_void) -> Result<&'a Binding, Status> {
    if context.is_null() {
        Err(STATUS_INVALID_ARGUMENT)
    } else {
        // SAFETY: native caller holds the binding throughout this synchronous call.
        Ok(unsafe { &*context.cast::<Binding>() })
    }
}
fn guarded(operation: impl FnOnce() -> Result<(), Status>) -> Status {
    match catch_unwind(AssertUnwindSafe(operation)) {
        Ok(Ok(())) => STATUS_OK,
        Ok(Err(status)) => status,
        Err(_) => STATUS_INTERNAL_ERROR,
    }
}
fn lock<T>(mutex: &Mutex<T>) -> Result<std::sync::MutexGuard<'_, T>, Status> {
    mutex.lock().map_err(|_| STATUS_INTERNAL_ERROR)
}
fn resource(binding: &Binding, id: u64) -> Result<Arc<Resource>, Status> {
    lock(&binding.resources)?
        .get(&id)
        .cloned()
        .ok_or(net::NOT_FOUND)
}
fn permit(binding: &Binding) -> Result<Permit, Status> {
    let shared = &binding.shared;
    let mut count = shared.handles.load(Ordering::Acquire);
    loop {
        if count >= shared.max_handles {
            return Err(net::RESOURCE_EXHAUSTED);
        }
        match shared.handles.compare_exchange_weak(
            count,
            count + 1,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => break,
            Err(actual) => count = actual,
        }
    }
    Ok(Permit(shared.clone()))
}
fn insert(binding: &Binding, socket: SocketResource, permit: Permit) -> Result<u64, Status> {
    insert_from_listener(binding, socket, permit, None)
}
fn insert_from_listener(
    binding: &Binding,
    socket: SocketResource,
    permit: Permit,
    listener: Option<(u64, &Arc<Resource>)>,
) -> Result<u64, Status> {
    let mut resources = lock(&binding.resources)?;
    // The accept call may have held the listener alive while another thread
    // closed its handle. Serialize this check with close so no child can be
    // published after its owning listener has been removed.
    if let Some((id, expected)) = listener
        && !resources
            .get(&id)
            .is_some_and(|resource| Arc::ptr_eq(resource, expected))
    {
        return Err(net::NOT_FOUND);
    }
    if resources.len() >= binding.shared.max_handles as usize {
        return Err(net::RESOURCE_EXHAUSTED);
    }
    let mut id = binding.next.load(Ordering::Acquire);
    loop {
        let next = id.checked_add(1).ok_or(net::RESOURCE_EXHAUSTED)?;
        match binding
            .next
            .compare_exchange_weak(id, next, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) => break,
            Err(actual) => id = actual,
        }
    }
    resources.insert(
        id,
        Arc::new(Resource {
            socket,
            _permit: permit,
            #[cfg(windows)]
            send_healthy: Mutex::new(true),
        }),
    );
    Ok(id)
}
fn to_address(address: SocketAddr) -> net::Address {
    match address {
        SocketAddr::V4(v) => {
            let mut bytes = [0; 16];
            bytes[..4].copy_from_slice(&v.ip().octets());
            net::Address {
                family: 4,
                port: v.port(),
                bytes,
                ..Default::default()
            }
        }
        SocketAddr::V6(v) => net::Address {
            family: 6,
            port: v.port(),
            scope_id: v.scope_id(),
            flowinfo: v.flowinfo(),
            bytes: v.ip().octets(),
            reserved: 0,
        },
    }
}
fn from_address(address: net::Address) -> Result<SocketAddr, Status> {
    if address.reserved != 0 {
        return Err(STATUS_INVALID_ARGUMENT);
    }
    match address.family {
        4 if address.scope_id == 0
            && address.flowinfo == 0
            && address.bytes[4..].iter().all(|b| *b == 0) =>
        {
            Ok(SocketAddr::new(
                IpAddr::V4(Ipv4Addr::new(
                    address.bytes[0],
                    address.bytes[1],
                    address.bytes[2],
                    address.bytes[3],
                )),
                address.port,
            ))
        }
        6 => Ok(SocketAddr::V6(SocketAddrV6::new(
            Ipv6Addr::from(address.bytes),
            address.port,
            address.flowinfo,
            address.scope_id,
        ))),
        _ => Err(STATUS_INVALID_ARGUMENT),
    }
}
fn io_status(error: io::Error) -> Status {
    if error.raw_os_error() == Some(10040) {
        return net::INVALID_DATA;
    }
    if error.raw_os_error() == Some(10054) {
        return net::CONNECTION_RESET;
    }
    match error.kind() {
        io::ErrorKind::WouldBlock => net::WOULD_BLOCK,
        io::ErrorKind::TimedOut => net::TIMED_OUT,
        io::ErrorKind::Interrupted => net::INTERRUPTED,
        io::ErrorKind::ConnectionRefused => net::CONNECTION_REFUSED,
        io::ErrorKind::ConnectionReset => net::CONNECTION_RESET,
        io::ErrorKind::ConnectionAborted => net::CONNECTION_ABORTED,
        io::ErrorKind::NotConnected => net::NOT_CONNECTED,
        io::ErrorKind::AddrInUse => net::ADDRESS_IN_USE,
        io::ErrorKind::AddrNotAvailable => net::ADDRESS_UNAVAILABLE,
        io::ErrorKind::PermissionDenied => net::PERMISSION_DENIED,
        io::ErrorKind::Unsupported => net::UNSUPPORTED,
        io::ErrorKind::NotFound => net::NOT_FOUND,
        io::ErrorKind::BrokenPipe => net::BROKEN_PIPE,
        io::ErrorKind::InvalidData => net::INVALID_DATA,
        io::ErrorKind::HostUnreachable => net::HOST_UNREACHABLE,
        io::ErrorKind::NetworkUnreachable => net::NETWORK_UNREACHABLE,
        io::ErrorKind::InvalidInput => STATUS_INVALID_ARGUMENT,
        _ => net::IO_ERROR,
    }
}
unsafe fn output<T>(pointer: *mut T, value: T) -> Result<(), Status> {
    if pointer.is_null() {
        return Err(STATUS_INVALID_ARGUMENT);
    } // SAFETY: caller provides properly aligned writable storage for T.
    unsafe { *pointer = value };
    Ok(())
}
unsafe fn read_buffer<'a>(pointer: *const u8, len: usize) -> Result<&'a [u8], Status> {
    if len > net::MAX_IO_BYTES || (pointer.is_null() && len != 0) {
        return Err(STATUS_INVALID_ARGUMENT);
    }
    if len == 0 {
        Ok(&[])
    } else {
        // SAFETY: caller promises immutable initialized input for the synchronous call.
        Ok(unsafe { std::slice::from_raw_parts(pointer, len) })
    }
}
unsafe fn write_buffer<'a>(pointer: *mut u8, len: usize) -> Result<&'a mut [u8], Status> {
    if len > net::MAX_IO_BYTES || (pointer.is_null() && len != 0) {
        return Err(STATUS_INVALID_ARGUMENT);
    }
    if len == 0 {
        Ok(&mut [])
    } else {
        // SAFETY: caller promises exclusively borrowed writable output for this call.
        Ok(unsafe { std::slice::from_raw_parts_mut(pointer, len) })
    }
}
unsafe extern "C" fn capabilities(context: *mut c_void, out: *mut net::Capabilities) -> Status {
    guarded(|| {
        let binding = unsafe { binding(context)? };
        unsafe {
            output(
                out,
                net::Capabilities {
                    size: size_of::<net::Capabilities>() as u32,
                    version: 1,
                    features: net::TCP
                        | net::UDP
                        | net::DNS
                        | net::BATCH
                        | net::MULTICAST
                        | net::INTERFACES
                        | net::DATAGRAM_DIAGNOSTICS,
                    max_io_bytes: net::MAX_IO_BYTES as u64,
                    max_handles: binding.shared.max_handles,
                    max_batch: net::MAX_BATCH as u32,
                },
            )
        }
    })
}
unsafe extern "C" fn resolve(
    context: *mut c_void,
    host: BorrowedBytes,
    port: u16,
    timeout_ms: u32,
    out: *mut net::Address,
    capacity: usize,
    count: *mut usize,
) -> Status {
    guarded(|| {
        let binding = unsafe { binding(context)? };
        if out.is_null() || count.is_null() || capacity == 0 || capacity > net::MAX_ADDRESSES {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        let host =
            std::str::from_utf8(unsafe { host.read()? }).map_err(|_| STATUS_INVALID_ARGUMENT)?;
        if host.is_empty() || host.len() > 253 || host.bytes().any(|b| b.is_ascii_control()) {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        let addresses = binding
            .shared
            .resolver
            .resolve(host, port, timeout_ms)
            .map_err(io_status)?;
        if addresses.len() > capacity {
            return Err(net::RESOURCE_EXHAUSTED);
        }
        for (index, address) in addresses.iter().copied().enumerate() {
            unsafe { out.add(index).write(to_address(address)) }
        }
        unsafe { output(count, addresses.len()) }
    })
}
unsafe extern "C" fn interfaces(
    context: *mut c_void,
    out: *mut net::InterfaceAddress,
    capacity: usize,
    count: *mut usize,
) -> Status {
    guarded(|| {
        unsafe { binding(context)? };
        if out.is_null() || count.is_null() || capacity == 0 || capacity > net::MAX_ADDRESSES {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        let addresses = interfaces::enumerate().map_err(io_status)?;
        if addresses.len() > capacity {
            return Err(net::RESOURCE_EXHAUSTED);
        }
        for (index, address) in addresses.iter().copied().enumerate() {
            unsafe { out.add(index).write(address) }
        }
        unsafe { output(count, addresses.len()) }
    })
}
unsafe extern "C" fn tcp_listen(
    context: *mut c_void,
    address: net::Address,
    options: net::ListenOptions,
    out: *mut u64,
) -> Status {
    guarded(|| {
        let binding = unsafe { binding(context)? };
        if out.is_null()
            || options.size < size_of::<net::ListenOptions>() as u32
            || options.reserved != 0
            || options.flags
                & !(net::LISTEN_NONBLOCKING | net::LISTEN_REUSE_ADDRESS | net::LISTEN_IPV6_ONLY)
                != 0
            || options.backlog == 0
            || options.backlog > i32::MAX as u32
        {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        let address = from_address(address)?;
        let permit = permit(binding)?;
        let socket = Socket::new(
            Domain::for_address(address),
            Type::STREAM,
            Some(Protocol::TCP),
        )
        .map_err(io_status)?;
        if options.flags & net::LISTEN_REUSE_ADDRESS != 0 {
            socket.set_reuse_address(true).map_err(io_status)?
        }
        if address.is_ipv6() {
            socket
                .set_only_v6(options.flags & net::LISTEN_IPV6_ONLY != 0)
                .map_err(io_status)?
        }
        socket.bind(&SockAddr::from(address)).map_err(io_status)?;
        socket.listen(options.backlog as i32).map_err(io_status)?;
        socket
            .set_nonblocking(options.flags & net::LISTEN_NONBLOCKING != 0)
            .map_err(io_status)?;
        let id = insert(binding, SocketResource::Listener(socket.into()), permit)?;
        unsafe { output(out, id) }
    })
}
unsafe extern "C" fn tcp_connect(
    context: *mut c_void,
    address: net::Address,
    timeout_ms: u32,
    out: *mut u64,
) -> Status {
    guarded(|| {
        let binding = unsafe { binding(context)? };
        if out.is_null() || timeout_ms == 0 {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        let address = from_address(address)?;
        let permit = permit(binding)?;
        let socket =
            TcpStream::connect_timeout(&address, Duration::from_millis(u64::from(timeout_ms)))
                .map_err(io_status)?;
        let id = insert(binding, SocketResource::Tcp(socket), permit)?;
        unsafe { output(out, id) }
    })
}
/// Room for a large video frame's burst. These sockets never block, so anything
/// past the kernel's small default buffer waits for a later send attempt.
const UDP_SEND_BUFFER_BYTES: usize = 1024 * 1024;

/// Best effort: the kernel clamps the request to its configured ceiling.
fn request_udp_send_buffer(socket: &Socket) {
    let _ = socket.set_send_buffer_size(UDP_SEND_BUFFER_BYTES);
}

unsafe extern "C" fn udp_bind(
    context: *mut c_void,
    address: net::Address,
    out: *mut u64,
) -> Status {
    guarded(|| {
        let binding = unsafe { binding(context)? };
        if out.is_null() {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        let address = from_address(address)?;
        let permit = permit(binding)?;
        let socket = Socket::new(
            Domain::for_address(address),
            Type::DGRAM,
            Some(Protocol::UDP),
        )
        .map_err(io_status)?;
        if address.is_ipv6() {
            socket.set_only_v6(true).map_err(io_status)?
        }
        socket.bind(&address.into()).map_err(io_status)?;
        socket.set_nonblocking(true).map_err(io_status)?;
        request_udp_send_buffer(&socket);
        diagnostics::enable_timestamps(&socket);
        let id = insert(binding, SocketResource::Udp(socket.into()), permit)?;
        unsafe { output(out, id) }
    })
}
unsafe extern "C" fn accept(
    context: *mut c_void,
    id: u64,
    out: *mut u64,
    peer: *mut net::Address,
) -> Status {
    guarded(|| {
        let binding = unsafe { binding(context)? };
        if out.is_null() || peer.is_null() {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        let parent = resource(binding, id)?;
        let SocketResource::Listener(listener) = &parent.socket else {
            return Err(net::UNSUPPORTED);
        };
        let permit = permit(binding)?;
        let (stream, address) = listener.accept().map_err(io_status)?;
        stream.set_nonblocking(true).map_err(io_status)?;
        let id = insert_from_listener(
            binding,
            SocketResource::Tcp(stream),
            permit,
            Some((id, &parent)),
        )?;
        unsafe {
            output(out, id)?;
            output(peer, to_address(address))
        }
    })
}
unsafe extern "C" fn local_addr(context: *mut c_void, id: u64, out: *mut net::Address) -> Status {
    guarded(|| {
        let binding = unsafe { binding(context)? };
        if out.is_null() {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        let resource = resource(binding, id)?;
        let address = match &resource.socket {
            SocketResource::Listener(s) => s.local_addr(),
            SocketResource::Tcp(s) => s.local_addr(),
            SocketResource::Udp(s) => s.local_addr(),
        }
        .map_err(io_status)?;
        unsafe { output(out, to_address(address)) }
    })
}
unsafe extern "C" fn peer_addr(context: *mut c_void, id: u64, out: *mut net::Address) -> Status {
    guarded(|| {
        let binding = unsafe { binding(context)? };
        if out.is_null() {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        let resource = resource(binding, id)?;
        let address = match &resource.socket {
            SocketResource::Tcp(s) => s.peer_addr(),
            SocketResource::Udp(s) => s.peer_addr(),
            _ => return Err(net::UNSUPPORTED),
        }
        .map_err(io_status)?;
        unsafe { output(out, to_address(address)) }
    })
}
unsafe extern "C" fn read(
    context: *mut c_void,
    id: u64,
    buffer: *mut u8,
    len: usize,
    count: *mut usize,
) -> Status {
    guarded(|| {
        let binding = unsafe { binding(context)? };
        if count.is_null() {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        let buffer = unsafe { write_buffer(buffer, len)? };
        let resource = resource(binding, id)?;
        let count_value = match &resource.socket {
            SocketResource::Tcp(s) => {
                let mut borrowed = s;
                borrowed.read(buffer)
            }
            SocketResource::Udp(s) => s.recv(buffer),
            _ => return Err(net::UNSUPPORTED),
        }
        .map_err(io_status)?;
        unsafe { output(count, count_value) }
    })
}
unsafe extern "C" fn peek(
    context: *mut c_void,
    id: u64,
    buffer: *mut u8,
    len: usize,
    count: *mut usize,
) -> Status {
    guarded(|| {
        let binding = unsafe { binding(context)? };
        if count.is_null() {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        let buffer = unsafe { write_buffer(buffer, len)? };
        let resource = resource(binding, id)?;
        let count_value = match &resource.socket {
            SocketResource::Tcp(s) => s.peek(buffer),
            SocketResource::Udp(s) => s.peek(buffer),
            _ => return Err(net::UNSUPPORTED),
        }
        .map_err(io_status)?;
        unsafe { output(count, count_value) }
    })
}
unsafe extern "C" fn write(
    context: *mut c_void,
    id: u64,
    buffer: *const u8,
    len: usize,
    count: *mut usize,
) -> Status {
    guarded(|| {
        let binding = unsafe { binding(context)? };
        if count.is_null() {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        let buffer = unsafe { read_buffer(buffer, len)? };
        let resource = resource(binding, id)?;
        #[cfg(windows)]
        let _send_guard = send_guard(&resource)?;
        let count_value = match &resource.socket {
            SocketResource::Tcp(s) => {
                let mut borrowed = s;
                borrowed.write(buffer)
            }
            SocketResource::Udp(s) => s.send(buffer),
            _ => return Err(net::UNSUPPORTED),
        }
        .map_err(io_status)?;
        unsafe { output(count, count_value) }
    })
}
unsafe extern "C" fn recv_from(
    context: *mut c_void,
    id: u64,
    buffer: *mut u8,
    len: usize,
    count: *mut usize,
    peer: *mut net::Address,
) -> Status {
    guarded(|| {
        let binding = unsafe { binding(context)? };
        if count.is_null() || peer.is_null() {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        let buffer = unsafe { write_buffer(buffer, len)? };
        let resource = resource(binding, id)?;
        let SocketResource::Udp(socket) = &resource.socket else {
            return Err(net::UNSUPPORTED);
        };
        let (size, address) = socket.recv_from(buffer).map_err(io_status)?;
        unsafe {
            output(count, size)?;
            output(peer, to_address(address))
        }
    })
}
unsafe extern "C" fn send_to(
    context: *mut c_void,
    id: u64,
    buffer: *const u8,
    len: usize,
    peer: net::Address,
    count: *mut usize,
) -> Status {
    guarded(|| {
        let binding = unsafe { binding(context)? };
        if count.is_null() {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        let buffer = unsafe { read_buffer(buffer, len)? };
        let peer = from_address(peer)?;
        let resource = resource(binding, id)?;
        let SocketResource::Udp(socket) = &resource.socket else {
            return Err(net::UNSUPPORTED);
        };
        #[cfg(windows)]
        let _send_guard = send_guard(&resource)?;
        let size = socket.send_to(buffer, peer).map_err(io_status)?;
        unsafe { output(count, size) }
    })
}
unsafe extern "C" fn connect_udp(context: *mut c_void, id: u64, peer: net::Address) -> Status {
    guarded(|| {
        let binding = unsafe { binding(context)? };
        let peer = from_address(peer)?;
        let resource = resource(binding, id)?;
        let SocketResource::Udp(socket) = &resource.socket else {
            return Err(net::UNSUPPORTED);
        };
        socket.connect(peer).map_err(io_status)
    })
}
fn sockref(resource: &Resource) -> SockRef<'_> {
    match &resource.socket {
        SocketResource::Listener(s) => SockRef::from(s),
        SocketResource::Tcp(s) => SockRef::from(s),
        SocketResource::Udp(s) => SockRef::from(s),
    }
}
fn boolean(value: u64) -> Result<bool, Status> {
    match value {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(STATUS_INVALID_ARGUMENT),
    }
}
fn timeout(value: u64) -> Result<Option<Duration>, Status> {
    if value > u32::MAX as u64 {
        return Err(STATUS_INVALID_ARGUMENT);
    }
    Ok((value != 0).then(|| Duration::from_millis(value)))
}
unsafe extern "C" fn set_option(context: *mut c_void, id: u64, option: u32, value: u64) -> Status {
    guarded(|| {
        let binding = unsafe { binding(context)? };
        let resource = resource(binding, id)?;
        let socket = sockref(&resource);
        match option {
            net::NONBLOCKING => socket.set_nonblocking(boolean(value)?),
            net::READ_TIMEOUT_MS => socket.set_read_timeout(timeout(value)?),
            net::WRITE_TIMEOUT_MS => socket.set_write_timeout(timeout(value)?),
            net::NODELAY => socket.set_tcp_nodelay(boolean(value)?),
            net::BROADCAST => socket.set_broadcast(boolean(value)?),
            net::TTL => {
                socket.set_ttl_v4(u32::try_from(value).map_err(|_| STATUS_INVALID_ARGUMENT)?)
            }
            net::MULTICAST_IF_V6 => socket
                .set_multicast_if_v6(u32::try_from(value).map_err(|_| STATUS_INVALID_ARGUMENT)?),
            net::MULTICAST_HOPS_V6 => socket
                .set_multicast_hops_v6(u32::try_from(value).map_err(|_| STATUS_INVALID_ARGUMENT)?),
            net::MULTICAST_LOOP_V4 => socket.set_multicast_loop_v4(boolean(value)?),
            net::MULTICAST_LOOP_V6 => socket.set_multicast_loop_v6(boolean(value)?),
            net::MULTICAST_TTL_V4 => socket
                .set_multicast_ttl_v4(u32::try_from(value).map_err(|_| STATUS_INVALID_ARGUMENT)?),
            net::IPV6_ONLY => socket.set_only_v6(boolean(value)?),
            net::SOCKET_TOS => {
                socket.set_tos_v4(u32::try_from(value).map_err(|_| STATUS_INVALID_ARGUMENT)?)
            }
            _ => return Err(net::UNSUPPORTED),
        }
        .map_err(io_status)
    })
}
unsafe extern "C" fn get_option(
    context: *mut c_void,
    id: u64,
    option: u32,
    out: *mut u64,
) -> Status {
    guarded(|| {
        let binding = unsafe { binding(context)? };
        if out.is_null() {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        let resource = resource(binding, id)?;
        let socket = sockref(&resource);
        let value = match option {
            net::READ_TIMEOUT_MS => socket
                .read_timeout()
                .map(|d| d.map_or(0, |v| v.as_millis() as u64)),
            net::WRITE_TIMEOUT_MS => socket
                .write_timeout()
                .map(|d| d.map_or(0, |v| v.as_millis() as u64)),
            net::NODELAY => socket.tcp_nodelay().map(u64::from),
            net::BROADCAST => socket.broadcast().map(u64::from),
            net::TTL => socket.ttl_v4().map(u64::from),
            net::MULTICAST_IF_V6 => socket.multicast_if_v6().map(u64::from),
            net::MULTICAST_HOPS_V6 => socket.multicast_hops_v6().map(u64::from),
            net::MULTICAST_LOOP_V4 => socket.multicast_loop_v4().map(u64::from),
            net::MULTICAST_LOOP_V6 => socket.multicast_loop_v6().map(u64::from),
            net::MULTICAST_TTL_V4 => socket.multicast_ttl_v4().map(u64::from),
            net::IPV6_ONLY => socket.only_v6().map(u64::from),
            net::SOCKET_TOS => socket.tos_v4().map(u64::from),
            _ => return Err(net::UNSUPPORTED),
        }
        .map_err(io_status)?;
        unsafe { output(out, value) }
    })
}
unsafe extern "C" fn join_multicast(
    context: *mut c_void,
    id: u64,
    group: net::Address,
    interface: u32,
    join: bool,
) -> Status {
    guarded(|| {
        let binding = unsafe { binding(context)? };
        let group = from_address(group)?;
        let resource = resource(binding, id)?;
        let SocketResource::Udp(socket) = &resource.socket else {
            return Err(net::UNSUPPORTED);
        };
        match group.ip() {
            IpAddr::V6(group) => {
                if join {
                    socket.join_multicast_v6(&group, interface)
                } else {
                    socket.leave_multicast_v6(&group, interface)
                }
            }
            IpAddr::V4(group) if interface == 0 => {
                if join {
                    socket.join_multicast_v4(&group, &Ipv4Addr::UNSPECIFIED)
                } else {
                    socket.leave_multicast_v4(&group, &Ipv4Addr::UNSPECIFIED)
                }
            }
            _ => return Err(net::UNSUPPORTED),
        }
        .map_err(io_status)
    })
}
unsafe extern "C" fn send_batch(
    context: *mut c_void,
    id: u64,
    datagrams: *const net::Datagram,
    len: usize,
    count: *mut usize,
) -> Status {
    guarded(|| {
        let binding = unsafe { binding(context)? };
        if datagrams.is_null() || count.is_null() || len == 0 || len > net::MAX_BATCH {
            return Err(STATUS_INVALID_ARGUMENT);
        } // SAFETY: caller supplies bounded readable descriptor array for this call.
        let datagrams = unsafe { std::slice::from_raw_parts(datagrams, len) };
        let mut total = 0usize;
        let mut buffers = Vec::with_capacity(len);
        for datagram in datagrams {
            if datagram.len > 65535 {
                return Err(STATUS_INVALID_ARGUMENT);
            }
            total = total
                .checked_add(datagram.len)
                .filter(|size| *size <= net::MAX_IO_BYTES)
                .ok_or(STATUS_INVALID_ARGUMENT)?;
            buffers.push(unsafe { read_buffer(datagram.data, datagram.len)? });
        }
        let resource = resource(binding, id)?;
        let SocketResource::Udp(socket) = &resource.socket else {
            return Err(net::UNSUPPORTED);
        };
        #[cfg(windows)]
        let sent = {
            let mut healthy = send_guard(&resource)?;
            batch::send_serialized(socket, &buffers, &mut healthy).map_err(io_status)?
        };
        #[cfg(not(windows))]
        let sent = batch::send(socket, &buffers).map_err(io_status)?;
        unsafe { output(count, sent) }
    })
}
unsafe extern "C" fn shutdown(context: *mut c_void, id: u64, direction: u32) -> Status {
    guarded(|| {
        let binding = unsafe { binding(context)? };
        let resource = resource(binding, id)?;
        let SocketResource::Tcp(socket) = &resource.socket else {
            return Err(net::UNSUPPORTED);
        };
        let direction = match direction {
            net::SHUTDOWN_READ => Shutdown::Read,
            net::SHUTDOWN_WRITE => Shutdown::Write,
            net::SHUTDOWN_BOTH => Shutdown::Both,
            _ => return Err(STATUS_INVALID_ARGUMENT),
        };
        socket.shutdown(direction).map_err(io_status)
    })
}
unsafe extern "C" fn close(context: *mut c_void, id: u64) -> Status {
    guarded(|| {
        let binding = unsafe { binding(context)? };
        let resource = lock(&binding.resources)?
            .remove(&id)
            .ok_or(net::NOT_FOUND)?;
        drop(resource);
        Ok(())
    })
}
#[cfg(test)]
mod tests;

#[cfg(windows)]
fn send_guard(resource: &Resource) -> Result<std::sync::MutexGuard<'_, bool>, Status> {
    let guard = lock(&resource.send_healthy)?;
    if !*guard {
        return Err(net::IO_ERROR);
    }
    Ok(guard)
}

#[cfg(test)]
mod status_tests {
    use super::*;

    #[test]
    fn unreachable_io_errors_have_provider_neutral_statuses() {
        assert_eq!(
            io_status(io::ErrorKind::HostUnreachable.into()),
            net::HOST_UNREACHABLE
        );
        assert_eq!(
            io_status(io::ErrorKind::NetworkUnreachable.into()),
            net::NETWORK_UNREACHABLE
        );
    }
}
