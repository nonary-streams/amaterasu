use super::*;
use amaterasu_module_sdk::abi::{AbiHeader, BorrowedBytes, NativeInterface, STATUS_OK, Status};
use amaterasu_module_sdk::network::{abi::*, address_to_native};
use std::{ffi::c_void, sync::Mutex};

#[derive(Default)]
struct Script {
    reject_listen: bool,
    wake_supported: bool,
    reject_address_lookup: bool,
    listeners: Vec<Address>,
    listen_attempts: usize,
    accepted: bool,
    read_offset: usize,
    written: Vec<u8>,
    closed: Vec<u64>,
    released: bool,
}
struct ScriptedProvider {
    api: NetworkApiV1_4,
    script: Arc<Mutex<Script>>,
}
unsafe fn script<'a>(context: *mut c_void) -> &'a Mutex<Script> {
    // SAFETY: the constructor retains the boxed fixture until final release.
    unsafe { &(*context.cast::<ScriptedProvider>()).script }
}
unsafe extern "C" fn release(context: *mut c_void) {
    // SAFETY: SDK owns this box and releases it once after all calls finish.
    let provider = unsafe { Box::from_raw(context.cast::<ScriptedProvider>()) };
    provider.script.lock().unwrap().released = true;
}
unsafe extern "C" fn capabilities(context: *mut c_void, output: *mut Capabilities) -> Status {
    let wake_supported = unsafe { script(context) }.lock().unwrap().wake_supported;
    // SAFETY: SDK supplies writable callback-lifetime output storage.
    unsafe {
        *output = Capabilities {
            size: size_of::<Capabilities>() as u32,
            version: 1,
            features: TCP | if wake_supported { WAKE_MAC } else { 0 },
            max_io_bytes: MAX_IO_BYTES as u64,
            max_handles: 128,
            max_batch: 1,
        }
    };
    STATUS_OK
}
unsafe extern "C" fn listen(
    context: *mut c_void,
    mut address: Address,
    _: ListenOptions,
    output: *mut u64,
) -> Status {
    // SAFETY: fixture context/output live during this synchronous callback.
    let mut script = unsafe { script(context) }.lock().unwrap();
    script.listen_attempts += 1;
    if script.reject_listen {
        return ADDRESS_IN_USE;
    }
    if address.port == 0 {
        address.port = 48_000 + script.listeners.len() as u16;
    }
    script.listeners.push(address);
    unsafe { *output = script.listeners.len() as u64 };
    STATUS_OK
}
unsafe extern "C" fn accept(
    context: *mut c_void,
    listener: u64,
    handle: *mut u64,
    address: *mut Address,
) -> Status {
    // SAFETY: fixture context/output live during this synchronous callback.
    let mut script = unsafe { script(context) }.lock().unwrap();
    if listener != 1 || script.accepted {
        return WOULD_BLOCK;
    }
    script.accepted = true;
    unsafe {
        *handle = 3;
        *address = address_to_native("127.0.0.1:12345".parse().unwrap())
    };
    STATUS_OK
}
unsafe extern "C" fn local_addr(context: *mut c_void, handle: u64, output: *mut Address) -> Status {
    // SAFETY: fixture context/output live during this synchronous callback.
    let script = unsafe { script(context) }.lock().unwrap();
    if script.reject_address_lookup {
        return NOT_FOUND;
    }
    let index = if handle == 3 {
        0
    } else {
        (handle - 1) as usize
    };
    let Some(address) = script.listeners.get(index) else {
        return NOT_FOUND;
    };
    unsafe { *output = *address };
    STATUS_OK
}
unsafe extern "C" fn read(
    context: *mut c_void,
    _: u64,
    output: *mut u8,
    capacity: usize,
    count: *mut usize,
) -> Status {
    const REQUEST: &[u8] = b"GET /serverinfo HTTP/1.1\r\nHost: fixture\r\n\r\n";
    // SAFETY: fixture state/output allocation are retained by the SDK call.
    let mut script = unsafe { script(context) }.lock().unwrap();
    let remaining = &REQUEST[script.read_offset..];
    if remaining.is_empty() {
        return WOULD_BLOCK;
    }
    let length = remaining.len().min(capacity);
    unsafe {
        std::ptr::copy_nonoverlapping(remaining.as_ptr(), output, length);
        *count = length;
    }
    script.read_offset += length;
    STATUS_OK
}
unsafe extern "C" fn write(
    context: *mut c_void,
    _: u64,
    input: *const u8,
    length: usize,
    count: *mut usize,
) -> Status {
    // SAFETY: fixture state and borrowed bytes/count are valid during call.
    unsafe { script(context) }
        .lock()
        .unwrap()
        .written
        .extend_from_slice(unsafe { std::slice::from_raw_parts(input, length) });
    unsafe { *count = length };
    STATUS_OK
}
unsafe extern "C" fn set_option(_: *mut c_void, _: u64, _: u32, _: u64) -> Status {
    STATUS_OK
}
unsafe extern "C" fn close(context: *mut c_void, handle: u64) -> Status {
    // SAFETY: SDK retains fixture context until all handles have closed.
    unsafe { script(context) }
        .lock()
        .unwrap()
        .closed
        .push(handle);
    STATUS_OK
}
macro_rules! unsupported {
    ($name:ident($($argument:ident: $kind:ty),* $(,)?)) => {
        unsafe extern "C" fn $name($(_: $kind),*) -> Status { UNSUPPORTED }
    };
}
unsupported!(
    resolve(
        c: *mut c_void,
        host: BorrowedBytes,
        port: u16,
        timeout: u32,
        addresses: *mut Address,
        capacity: usize,
        count: *mut usize,
    )
);
unsupported!(
    interfaces(c: *mut c_void, output: *mut InterfaceAddress, capacity: usize, count: *mut usize)
);
unsupported!(connect(c: *mut c_void, address: Address, timeout: u32, handle: *mut u64));
unsupported!(udp_bind(c: *mut c_void, address: Address, handle: *mut u64));
unsupported!(address(c: *mut c_void, handle: u64, output: *mut Address));
unsupported!(peek(c: *mut c_void, handle: u64, data: *mut u8, length: usize, count: *mut usize));
unsupported!(
    recv_from(
        c: *mut c_void,
        handle: u64,
        data: *mut u8,
        length: usize,
        count: *mut usize,
        address: *mut Address,
    )
);
unsupported!(
    send_to(
        c: *mut c_void,
        handle: u64,
        data: *const u8,
        length: usize,
        address: Address,
        count: *mut usize,
    )
);
unsupported!(connect_udp(c: *mut c_void, handle: u64, address: Address));
unsupported!(get_option(c: *mut c_void, handle: u64, option: u32, value: *mut u64));
unsupported!(
    join_multicast(c: *mut c_void, handle: u64, group: Address, interface: u32, join: bool)
);
unsupported!(
    send_batch(
        c: *mut c_void,
        handle: u64,
        datagrams: *const Datagram,
        length: usize,
        count: *mut usize,
    )
);
unsupported!(shutdown(c: *mut c_void, handle: u64, direction: u32));

unsupported!(udp_bind_shared(c: *mut c_void, address: Address, handle: *mut u64));
unsupported!(
    recv_from_timed(
        c: *mut c_void,
        handle: u64,
        data: *mut u8,
        length: usize,
        count: *mut usize,
        address: *mut Address,
        timing: *mut ReceiveTiming,
    )
);
unsupported!(socket_diagnostics(c: *mut c_void, handle: u64, out: *mut SocketDiagnostics));
unsupported!(
    reverse_resolve(
        c: *mut c_void,
        address: Address,
        timeout: u32,
        out: *mut u8,
        capacity: usize,
        count: *mut usize,
    )
);

unsafe extern "C" fn wake_mac(_: *mut c_void, local: Address, out: *mut [u8; 6]) -> Status {
    let mac = match (local.family, local.bytes[3], local.port) {
        (4, 1, 48000) => [2, 0xab, 0, 0, 0, 1],
        (4, 2, 48000) => [2, 0xab, 0, 0, 0, 2],
        (4, 3, 48000) => [1, 0xab, 0, 0, 0, 3],
        (4, 4, 48000) => return WOULD_BLOCK,
        (4, 5, 48000) => return IO_ERROR,
        _ => return NOT_FOUND,
    };
    // SAFETY: the SDK supplies a writable six-byte borrow for this call.
    unsafe { out.write(mac) };
    STATUS_OK
}

pub(crate) fn wake_identity_provider() -> Network {
    scripted_provider(false, true).0
}

pub(crate) fn legacy_provider() -> Network {
    replacement(false).0
}

fn replacement(reject_listen: bool) -> (Network, Arc<Mutex<Script>>) {
    scripted_provider(reject_listen, false)
}

fn scripted_provider(reject_listen: bool, wake_supported: bool) -> (Network, Arc<Mutex<Script>>) {
    let script = Arc::new(Mutex::new(Script {
        reject_listen,
        wake_supported,
        ..Script::default()
    }));
    let mut provider = Box::new(ScriptedProvider {
        api: NetworkApiV1_4 {
            v1_3: NetworkApiV1_3 {
                v1_2: NetworkApiV1_2 {
                    v1_1: NetworkApiV1_1 {
                        v1: NetworkApi {
                            base: NativeInterface {
                                header: if wake_supported {
                                    AbiHeader {
                                        major: MAJOR,
                                        minor: WAKE_MAC_MINOR,
                                        size: size_of::<NetworkApiV1_4>() as u32,
                                    }
                                } else {
                                    AbiHeader::of::<NetworkApi>()
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
                            peer_addr: address,
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
                        recv_from_timed,
                        socket_diagnostics,
                    },
                    udp_bind_shared,
                },
                reverse_resolve,
            },
            wake_mac,
        },
        script: script.clone(),
    });
    provider.api.base.context = (&mut *provider as *mut ScriptedProvider).cast();
    let table = &provider.api.v1_3.v1_2.v1_1.v1 as *const NetworkApi;
    // SAFETY: SDK takes ownership of the complete table through its release
    // callback. No native pointer depends on an external library/allocation.
    let _provider = Box::into_raw(provider);
    (
        unsafe { Network::from_native_table(table).unwrap() },
        script,
    )
}
fn settings() -> Settings {
    Settings {
        enabled: true,
        http_bind: "127.0.0.1:0".parse().unwrap(),
        https_bind: "127.0.0.1:0".parse().unwrap(),
        ..Settings::default()
    }
}
#[test]
fn configured_provider_rejection_does_not_fall_back_to_os_listeners() {
    let (network, script) = replacement(true);
    let error = Server::bind_with_network(network, &settings())
        .err()
        .unwrap();
    assert!(
        error.contains("HTTP listener bind 127.0.0.1:0 failed (AddrInUse)"),
        "{error}"
    );
    assert!(error.contains(&format!("network provider status {ADDRESS_IN_USE}")));
    let script = script.lock().unwrap();
    assert_eq!(script.listen_attempts, 1);
    assert!(script.released);
}
#[test]
fn failed_bound_address_lookup_names_endpoint_and_releases_handle() {
    let (network, script) = replacement(false);
    script.lock().unwrap().reject_address_lookup = true;
    let error = Server::bind_with_network(network, &settings())
        .err()
        .unwrap();
    assert!(
        error.contains("HTTP listener local address lookup for 127.0.0.1:0 failed (NotFound)"),
        "{error}"
    );
    let script = script.lock().unwrap();
    assert_eq!(script.closed, [1]);
    assert!(script.released);
}
#[test]
fn alternate_provider_serves_http_without_os_sockets_and_retires_all_handles() {
    let (network, script) = replacement(false);
    let mut server = Server::bind_with_network(network, &settings()).unwrap();
    assert_eq!(server.http_address().port(), 48_000);
    let request = server.tick().requests.pop().expect("scripted request");
    assert!(!request.secure);
    assert_eq!(request.local, Some("127.0.0.1:48000".parse().unwrap()));
    assert!(server.respond(request.id, simple_reply(200, "alternate provider response")));
    for _ in 0..4 {
        server.tick();
    }
    assert!(
        script
            .lock()
            .unwrap()
            .written
            .ends_with(b"alternate provider response")
    );
    drop(server);
    let mut script = script.lock().unwrap();
    script.closed.sort_unstable();
    assert_eq!(script.closed, vec![1, 2, 3]);
    assert!(script.released);
}
