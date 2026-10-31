//! A replacement provider can own listener semantics without creating OS sockets.
use super::*;
use amaterasu_module_sdk::abi::{
    AbiHeader, BorrowedBytes, NativeInterface, STATUS_INVALID_ARGUMENT, STATUS_OK, Status,
};
use amaterasu_module_sdk::network::abi::*;
use std::ffi::c_void;
use std::sync::{
    Mutex,
    atomic::{AtomicUsize, Ordering},
};

#[derive(Default)]
struct Calls {
    listen: AtomicUsize,
    accept: AtomicUsize,
    close: AtomicUsize,
    release: AtomicUsize,
}
#[repr(C)]
struct Provider {
    api: NetworkApi,
    address: Mutex<Address>,
    calls: Arc<Calls>,
}

macro_rules! unsupported {
    ($name:ident($($arg:ident: $ty:ty),* $(,)?)) => {
        unsafe extern "C" fn $name($($arg: $ty),*) -> Status { UNSUPPORTED }
    };
}
unsupported!(
    resolve(
        _context: *mut c_void,
        _name: BorrowedBytes,
        _port: u16,
        _timeout: u32,
        _addresses: *mut Address,
        _capacity: usize,
        _count: *mut usize,
    )
);
unsupported!(
    interfaces(
        _context: *mut c_void,
        _addresses: *mut InterfaceAddress,
        _capacity: usize,
        _count: *mut usize,
    )
);
unsupported!(connect(_context: *mut c_void, _address: Address, _timeout: u32, _handle: *mut u64));
unsupported!(udp_bind(_context: *mut c_void, _address: Address, _handle: *mut u64));
unsupported!(peer_addr(_context: *mut c_void, _handle: u64, _address: *mut Address));
unsupported!(
    read(
        _context: *mut c_void,
        _handle: u64,
        _bytes: *mut u8,
        _capacity: usize,
        _length: *mut usize,
    )
);
unsupported!(
    write(
        _context: *mut c_void,
        _handle: u64,
        _bytes: *const u8,
        _length: usize,
        _count: *mut usize,
    )
);
unsupported!(
    recv_from(
        _context: *mut c_void,
        _handle: u64,
        _bytes: *mut u8,
        _capacity: usize,
        _length: *mut usize,
        _peer: *mut Address,
    )
);
unsupported!(
    send_to(
        _context: *mut c_void,
        _handle: u64,
        _bytes: *const u8,
        _length: usize,
        _peer: Address,
        _count: *mut usize,
    )
);
unsupported!(connect_udp(_context: *mut c_void, _handle: u64, _peer: Address));
unsupported!(get_option(_context: *mut c_void, _handle: u64, _option: u32, _value: *mut u64));
unsupported!(
    join_multicast(
        _context: *mut c_void,
        _handle: u64,
        _group: Address,
        _interface: u32,
        _join: bool,
    )
);
unsupported!(
    send_batch(
        _context: *mut c_void,
        _handle: u64,
        _datagrams: *const Datagram,
        _count: usize,
        _sent: *mut usize,
    )
);
unsupported!(shutdown(_context: *mut c_void, _handle: u64, _direction: u32));

unsafe fn provider<'a>(context: *mut c_void) -> &'a Provider {
    // SAFETY: SDK callbacks retain this uniquely allocated table through release.
    unsafe { &*context.cast::<Provider>() }
}
unsafe extern "C" fn capabilities(_context: *mut c_void, output: *mut Capabilities) -> Status {
    // SAFETY: SDK supplies writable output for the whole descriptor.
    unsafe {
        *output = Capabilities {
            size: size_of::<Capabilities>() as u32,
            version: 1,
            features: TCP,
            max_io_bytes: 64 * 1024,
            max_handles: 32,
            max_batch: MAX_BATCH as u32,
        }
    };
    STATUS_OK
}
unsafe extern "C" fn listen(
    context: *mut c_void,
    address: Address,
    _options: ListenOptions,
    handle: *mut u64,
) -> Status {
    // SAFETY: SDK retains context and provides writable handle output.
    let provider = unsafe { provider(context) };
    *provider
        .address
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = address;
    provider.calls.listen.fetch_add(1, Ordering::SeqCst);
    unsafe { *handle = 41 };
    STATUS_OK
}
unsafe extern "C" fn local_addr(context: *mut c_void, handle: u64, output: *mut Address) -> Status {
    if handle != 41 {
        return STATUS_INVALID_ARGUMENT;
    }
    // SAFETY: context lives through this callback, output is writable.
    unsafe {
        *output = *provider(context)
            .address
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    };
    STATUS_OK
}
unsafe extern "C" fn accept(
    context: *mut c_void,
    handle: u64,
    _accepted: *mut u64,
    _peer: *mut Address,
) -> Status {
    if handle != 41 {
        return STATUS_INVALID_ARGUMENT;
    }
    // SAFETY: SDK retains provider context for the callback.
    unsafe { provider(context) }
        .calls
        .accept
        .fetch_add(1, Ordering::SeqCst);
    WOULD_BLOCK
}
unsafe extern "C" fn set_option(
    _context: *mut c_void,
    handle: u64,
    option: u32,
    _value: u64,
) -> Status {
    if handle == 41 && option == NONBLOCKING {
        STATUS_OK
    } else {
        UNSUPPORTED
    }
}
unsafe extern "C" fn close(context: *mut c_void, handle: u64) -> Status {
    if handle != 41 {
        return STATUS_INVALID_ARGUMENT;
    }
    // SAFETY: SDK closes handle before its final provider reference drops.
    unsafe { provider(context) }
        .calls
        .close
        .fetch_add(1, Ordering::SeqCst);
    STATUS_OK
}
unsafe extern "C" fn release(context: *mut c_void) {
    // SAFETY: SDK releases this transferred allocation once after every call and
    // provider-backed handle is retired. All callbacks live in this test binary.
    let provider = unsafe { Box::from_raw(context.cast::<Provider>()) };
    provider.calls.release.fetch_add(1, Ordering::SeqCst);
}
fn replacement(calls: Arc<Calls>) -> Network {
    let mut provider = Box::new(Provider {
        api: NetworkApi {
            base: NativeInterface {
                header: AbiHeader {
                    size: size_of::<NetworkApi>() as u32,
                    major: MAJOR,
                    minor: MINOR,
                },
                context: std::ptr::null_mut(),
                release: Some(release),
            },
            capabilities,
            resolve,
            interfaces,
            tcp_listen: listen,
            tcp_connect: connect,
            udp_bind,
            accept,
            local_addr,
            peer_addr,
            read,
            peek: read,
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
        address: Mutex::new(Address::default()),
        calls,
    });
    provider.api.base.context = std::ptr::from_mut(&mut *provider).cast();
    // SAFETY: this complete thread-safe table and every callback obey NetworkApi.
    // Ownership transfers to the SDK, which calls release before the test returns.
    unsafe { Network::from_native_table(Box::into_raw(provider).cast()).unwrap() }
}

#[test]
fn management_listener_uses_replacement_provider_and_retires_its_handle() {
    // The provider implements its own listener namespace without opening an OS
    // socket. Counters require every operation to reach this selected table.
    let address = "127.0.0.1:42424".parse().unwrap();
    let calls = Arc::new(Calls::default());
    let mut server = Server::bind_with_network(
        replacement(calls.clone()),
        address,
        Vec::new(),
        None,
        None,
        None,
    )
    .unwrap();
    assert_eq!(server.address(), address.to_string());
    assert_eq!(calls.listen.load(Ordering::SeqCst), 1);
    server.accept().unwrap();
    assert_eq!(calls.accept.load(Ordering::SeqCst), 1);
    assert!(server.connections.is_empty());
    assert_eq!(calls.release.load(Ordering::SeqCst), 0);
    drop(server);
    assert_eq!(calls.close.load(Ordering::SeqCst), 1);
    assert_eq!(calls.release.load(Ordering::SeqCst), 1);
}
