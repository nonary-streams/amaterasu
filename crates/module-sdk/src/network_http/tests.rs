//! Independent in-memory provider: the HTTP adapter must not open an OS socket
//! or ask the system resolver to reach this deliberately nonexistent hostname.
use super::*;
use crate::abi::{
    AbiHeader, BorrowedBytes, NativeInterface, STATUS_INVALID_ARGUMENT, STATUS_OK, Status,
};
use crate::network::abi::*;
use std::ffi::c_void;
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct Observed {
    host: String,
    target: Option<Address>,
    request: Vec<u8>,
    released: bool,
}
struct Provider {
    table: NetworkApi,
    observed: Arc<Mutex<Observed>>,
    response: Vec<u8>,
    cursor: Mutex<usize>,
    write_limit: usize,
    write_delay: Duration,
}
impl Provider {
    fn network(observed: Arc<Mutex<Observed>>) -> Network {
        Self::with_response(observed, b"HTTP/1.1 200 OK\r\nContent-Length: 21\r\nConnection: close\r\n\r\nprovider served this!".to_vec())
    }
    fn with_response(observed: Arc<Mutex<Observed>>, response: Vec<u8>) -> Network {
        Self::with_write_behavior(observed, response, usize::MAX, Duration::ZERO)
    }
    fn with_write_behavior(
        observed: Arc<Mutex<Observed>>,
        response: Vec<u8>,
        write_limit: usize,
        write_delay: Duration,
    ) -> Network {
        let mut provider = Box::new(Self {
            table: NetworkApi {
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
                udp_bind: bind_udp,
                accept,
                local_addr: address,
                peer_addr: address,
                read,
                peek: read,
                write,
                recv_from,
                send_to,
                connect_udp,
                set_option: option,
                get_option,
                join_multicast: multicast,
                send_batch: batch,
                shutdown,
                close,
            },
            observed,
            response,
            cursor: Mutex::new(0),
            write_limit,
            write_delay,
        });
        provider.table.base.context = (&mut *provider as *mut Provider).cast();
        let pointer = Box::into_raw(provider);
        // SAFETY: fixture implements thread-safe table and transfers its sole
        // box to release. Native calls borrow while Network holds the table.
        unsafe { Network::from_native_table(&(*pointer).table).unwrap() }
    }
}
unsafe fn provider<'a>(context: *mut c_void) -> &'a Provider {
    // SAFETY: each test table call is made with its live boxed provider context.
    unsafe { &*context.cast::<Provider>() }
}
unsafe extern "C" fn release(context: *mut c_void) {
    // SAFETY: called once by the last Network guard, after its sockets are closed.
    let provider = unsafe { Box::from_raw(context.cast::<Provider>()) };
    provider.observed.lock().unwrap().released = true;
}
unsafe extern "C" fn capabilities(_: *mut c_void, output: *mut Capabilities) -> Status {
    // SAFETY: SDK supplies writable output.
    unsafe {
        *output = Capabilities {
            size: size_of::<Capabilities>() as u32,
            version: 1,
            features: TCP | DNS,
            max_io_bytes: MAX_IO_BYTES as u64,
            max_handles: 4,
            max_batch: 0,
        }
    };
    STATUS_OK
}
unsafe extern "C" fn resolve(
    context: *mut c_void,
    host: BorrowedBytes,
    port: u16,
    _: u32,
    addresses: *mut Address,
    capacity: usize,
    count: *mut usize,
) -> Status {
    if capacity == 0 {
        return STATUS_INVALID_ARGUMENT;
    }
    // SAFETY: caller retains input hostname and writable outputs for this call.
    let host = unsafe { host.read().unwrap() };
    unsafe { provider(context) }.observed.lock().unwrap().host =
        String::from_utf8(host.to_vec()).unwrap();
    let address = crate::network::address_to_native(([198, 51, 100, 7], port).into());
    unsafe {
        *addresses = address;
        *count = 1;
    }
    STATUS_OK
}
unsafe extern "C" fn connect(
    context: *mut c_void,
    address: Address,
    _: u32,
    id: *mut u64,
) -> Status {
    // SAFETY: live provider and writable ID from SDK.
    unsafe { provider(context) }.observed.lock().unwrap().target = Some(address);
    unsafe {
        *id = 1;
    }
    STATUS_OK
}
unsafe extern "C" fn read(
    context: *mut c_void,
    _: u64,
    bytes: *mut u8,
    capacity: usize,
    count: *mut usize,
) -> Status {
    // SAFETY: SDK retains provider/output buffers for this synchronous call.
    let provider = unsafe { provider(context) };
    let mut cursor = provider.cursor.lock().unwrap();
    let length = (provider.response.len() - *cursor).min(capacity);
    unsafe {
        std::ptr::copy_nonoverlapping(provider.response.as_ptr().add(*cursor), bytes, length);
        *count = length;
    }
    *cursor += length;
    STATUS_OK
}
unsafe extern "C" fn write(
    context: *mut c_void,
    _: u64,
    bytes: *const u8,
    length: usize,
    count: *mut usize,
) -> Status {
    // SAFETY: SDK supplies readable bytes through the whole call.
    let provider = unsafe { provider(context) };
    let accepted = length.min(provider.write_limit);
    if !provider.write_delay.is_zero() {
        std::thread::sleep(provider.write_delay);
    }
    let bytes = unsafe { std::slice::from_raw_parts(bytes, accepted) };
    provider
        .observed
        .lock()
        .unwrap()
        .request
        .extend_from_slice(bytes);
    unsafe {
        *count = accepted;
    }
    STATUS_OK
}
unsafe extern "C" fn option(_: *mut c_void, _: u64, _: u32, _: u64) -> Status {
    STATUS_OK
}
unsafe extern "C" fn close(_: *mut c_void, _: u64) -> Status {
    STATUS_OK
}
unsafe extern "C" fn shutdown(_: *mut c_void, _: u64, _: u32) -> Status {
    STATUS_OK
}
unsafe extern "C" fn interfaces(
    _: *mut c_void,
    _: *mut InterfaceAddress,
    _: usize,
    _: *mut usize,
) -> Status {
    UNSUPPORTED
}
unsafe extern "C" fn listen(_: *mut c_void, _: Address, _: ListenOptions, _: *mut u64) -> Status {
    UNSUPPORTED
}
unsafe extern "C" fn bind_udp(_: *mut c_void, _: Address, _: *mut u64) -> Status {
    UNSUPPORTED
}
unsafe extern "C" fn accept(_: *mut c_void, _: u64, _: *mut u64, _: *mut Address) -> Status {
    UNSUPPORTED
}
unsafe extern "C" fn address(_: *mut c_void, _: u64, _: *mut Address) -> Status {
    UNSUPPORTED
}
unsafe extern "C" fn recv_from(
    _: *mut c_void,
    _: u64,
    _: *mut u8,
    _: usize,
    _: *mut usize,
    _: *mut Address,
) -> Status {
    UNSUPPORTED
}
unsafe extern "C" fn send_to(
    _: *mut c_void,
    _: u64,
    _: *const u8,
    _: usize,
    _: Address,
    _: *mut usize,
) -> Status {
    UNSUPPORTED
}
unsafe extern "C" fn connect_udp(_: *mut c_void, _: u64, _: Address) -> Status {
    UNSUPPORTED
}
unsafe extern "C" fn get_option(_: *mut c_void, _: u64, _: u32, _: *mut u64) -> Status {
    UNSUPPORTED
}
unsafe extern "C" fn multicast(_: *mut c_void, _: u64, _: Address, _: u32, _: bool) -> Status {
    UNSUPPORTED
}
unsafe extern "C" fn batch(
    _: *mut c_void,
    _: u64,
    _: *const Datagram,
    _: usize,
    _: *mut usize,
) -> Status {
    UNSUPPORTED
}

#[test]
fn http_uses_selected_provider_for_dns_connect_and_bytes_without_os_fallback() {
    let observed = Arc::new(Mutex::new(Observed::default()));
    let network = Provider::network(observed.clone());
    let client = agent(
        Config::builder()
            .proxy(None)
            .timeout_global(Some(Duration::from_secs(1)))
            .build(),
        network,
    );
    let body = client
        .get("http://virtual.invalid/asset")
        .call()
        .unwrap()
        .body_mut()
        .read_to_string()
        .unwrap();
    assert_eq!(body, "provider served this!");
    {
        let observed = observed.lock().unwrap();
        assert_eq!(observed.host, "virtual.invalid");
        assert_eq!(observed.target.unwrap().port, 80);
        assert!(observed.request.starts_with(b"GET /asset HTTP/1.1\r\n"));
        assert!(!observed.released);
    }
    drop(client);
    assert!(observed.lock().unwrap().released);
}

#[test]
fn explicit_socks_configuration_fails_without_opening_a_connection() {
    let observed = Arc::new(Mutex::new(Observed::default()));
    let network = Provider::network(observed.clone());
    let config = Config::builder()
        .proxy(Some(ureq::Proxy::new("socks5://127.0.0.1:1080").unwrap()))
        .build();
    let client = agent(config, network);
    assert!(client.get("http://virtual.invalid/asset").call().is_err());
    assert!(observed.lock().unwrap().target.is_none());
}

#[test]
fn proxy_connect_recursion_uses_the_same_selected_provider() {
    let observed = Arc::new(Mutex::new(Observed::default()));
    let response = b"HTTP/1.1 200 Connection established\r\n\r\nHTTP/1.1 200 OK\r\nContent-Length: 21\r\nConnection: close\r\n\r\nprovider served this!".to_vec();
    let network = Provider::with_response(observed.clone(), response);
    let config = Config::builder()
        .proxy(Some(ureq::Proxy::new("http://proxy.invalid:3128").unwrap()))
        .timeout_global(Some(Duration::from_secs(1)))
        .build();
    let client = agent(config, network);
    let body = client
        .get("http://virtual.invalid/asset")
        .call()
        .unwrap()
        .body_mut()
        .read_to_string()
        .unwrap();
    assert_eq!(body, "provider served this!");
    let observed = observed.lock().unwrap();
    assert_eq!(observed.host, "proxy.invalid");
    assert_eq!(observed.target.unwrap().port, 3128);
    assert!(observed.request.starts_with(b"CONNECT "));
    assert!(
        observed
            .request
            .windows(b"GET /asset HTTP/1.1".len())
            .any(|bytes| bytes == b"GET /asset HTTP/1.1")
    );
}

#[test]
fn partial_writes_share_one_absolute_timeout_instead_of_resetting_the_budget() {
    let observed = Arc::new(Mutex::new(Observed::default()));
    // This provider accepts just one byte per delayed call, so renewing the
    // timeout after each partial write would allow the full request through.
    let network =
        Provider::with_write_behavior(observed.clone(), Vec::new(), 1, Duration::from_millis(10));
    let config = Config::builder()
        .proxy(None)
        .timeout_global(Some(Duration::from_millis(35)))
        .build();
    let client = agent(config, network);
    let error = client
        .get("http://virtual.invalid/asset")
        .call()
        .unwrap_err();
    assert!(matches!(error, Error::Timeout(_)), "{error:?}");
    assert!(observed.lock().unwrap().request.len() < 10);
}
