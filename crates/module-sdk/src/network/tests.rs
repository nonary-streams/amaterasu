use super::*;
use amaterasu_abi::{AbiHeader, BorrowedBytes, NativeInterface, NativeInterfaceQuery};
use std::io::{Read, Write};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

static REPLACEMENT_RESOLVE_CALLS: AtomicUsize = AtomicUsize::new(0);
static REPLACEMENT_SHARED_CALLS: AtomicUsize = AtomicUsize::new(0);
static SHARED_CALLBACK: OnceLock<
    unsafe extern "C" fn(*mut std::ffi::c_void, abi::Address, *mut u64) -> amaterasu_abi::Status,
> = OnceLock::new();

unsafe extern "C" fn replacement_shared(
    context: *mut std::ffi::c_void,
    address: abi::Address,
    out: *mut u64,
) -> amaterasu_abi::Status {
    REPLACEMENT_SHARED_CALLS.fetch_add(1, Ordering::SeqCst);
    // SAFETY: the fixture forwards the live binding and synchronous output unchanged.
    unsafe { SHARED_CALLBACK.get().unwrap()(context, address, out) }
}

unsafe extern "C" fn replacement_resolve(
    _context: *mut std::ffi::c_void,
    host: BorrowedBytes,
    port: u16,
    timeout_ms: u32,
    output: *mut abi::Address,
    capacity: usize,
    count: *mut usize,
) -> amaterasu_abi::Status {
    // SAFETY: the unchanged SDK borrows this host only during the callback.
    let Ok(host) = (unsafe { host.read() }) else {
        return amaterasu_abi::STATUS_INVALID_ARGUMENT;
    };
    if host != b"fixture.local"
        || timeout_ms == 0
        || output.is_null()
        || count.is_null()
        || capacity == 0
    {
        return amaterasu_abi::STATUS_INVALID_ARGUMENT;
    }
    let address = address_to_native(std::net::SocketAddr::from(([127, 0, 0, 2], port)));
    // SAFETY: the SDK provides at least one writable address and one count.
    unsafe {
        output.write(address);
        count.write(1);
    }
    REPLACEMENT_RESOLVE_CALLS.fetch_add(1, Ordering::SeqCst);
    STATUS_OK
}

#[test]
fn unchanged_resolve_consumer_uses_the_selected_provider_implementation() {
    fn consumer(network: &Network, host: &str) -> std::io::Result<Vec<std::net::SocketAddr>> {
        network.resolve_timeout(host, 4242, Duration::from_secs(1))
    }

    let default_provider = Network::test_provider();
    assert_eq!(
        consumer(&default_provider, "127.0.0.1").unwrap(),
        ["127.0.0.1:4242".parse().unwrap()]
    );

    let replacement_table = standalone();
    // SAFETY: this uniquely owned, valid table has no consumers or calls yet.
    // The fixture replaces DNS itself; the other callbacks and release continue
    // to own their original, correctly typed binding. No OS resolver is called.
    unsafe {
        (*replacement_table.cast_mut()).resolve = replacement_resolve;
    }
    // SAFETY: the well-formed public Network table and release ownership are
    // transferred exactly once to the same SDK used by the default consumer.
    let replacement_provider = unsafe { Network::from_native_table(replacement_table) }.unwrap();
    let calls_before = REPLACEMENT_RESOLVE_CALLS.load(Ordering::SeqCst);
    assert_eq!(
        consumer(&replacement_provider, "fixture.local").unwrap(),
        ["127.0.0.2:4242".parse().unwrap()]
    );
    assert_eq!(
        REPLACEMENT_RESOLVE_CALLS.load(Ordering::SeqCst),
        calls_before + 1
    );
}

fn standalone() -> *const abi::NetworkApi {
    let factory = amaterasu_network_sockets::Factory::new();
    let query = NativeInterfaceQuery {
        header: AbiHeader::of::<NativeInterfaceQuery>(),
        interface_major: abi::MAJOR,
        min_minor: 0,
        min_size: size_of::<abi::NetworkApi>() as u32,
    };
    let mut output = std::ptr::null();
    // SAFETY: factory lives for bind; returned table retains its own provider state.
    assert_eq!(
        unsafe {
            (factory.api.bind.unwrap())(
                factory.api.context,
                BorrowedBytes::new(b"sdk.tests"),
                &query,
                &mut output,
            )
        },
        STATUS_OK
    );
    output.cast()
}
// Another parallel fixture may briefly acquire the released ephemeral port.
// Keep the assertion tied to the same endpoint and fail on a persistent owner.
fn assert_udp_rebinds<T>(bind: impl Fn() -> std::io::Result<T>) {
    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    loop {
        match bind() {
            Ok(_) => return,
            Err(error)
                if error.kind() == std::io::ErrorKind::AddrInUse
                    && std::time::Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(error) => panic!("released UDP endpoint could not be rebound: {error}"),
        }
    }
}
#[test]
fn opaque_udp_handles_preserve_datagrams_and_drop_closes_final_owner() {
    let network = Network::test_provider();
    let sender = network.bind_udp("127.0.0.1:0".parse().unwrap()).unwrap();
    let receiver = network.bind_udp("127.0.0.1:0".parse().unwrap()).unwrap();
    receiver.set_nonblocking(false).unwrap();
    receiver
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    sender.connect(receiver.local_addr().unwrap()).unwrap();
    assert_eq!(sender.send_batch(&[b"one", b"two"]).unwrap(), 2);
    let mut bytes = [0; 16];
    assert_eq!(receiver.recv(&mut bytes).unwrap(), 3);
    assert_eq!(&bytes[..3], b"one");
    assert_eq!(receiver.recv(&mut bytes).unwrap(), 3);
    assert_eq!(&bytes[..3], b"two");
    let address = receiver.local_addr().unwrap();
    let clone = receiver.try_clone().unwrap();
    drop(receiver);
    assert!(std::net::UdpSocket::bind(address).is_err());
    drop(clone);
    assert_udp_rebinds(|| std::net::UdpSocket::bind(address));
}
#[test]
fn shared_udp_sockets_can_coexist_and_old_providers_are_not_substituted() {
    let network = Network::test_provider();
    let first = network
        .bind_udp_shared("127.0.0.1:0".parse().unwrap())
        .unwrap();
    let address = first.local_addr().unwrap();
    let second = network.bind_udp_shared(address).unwrap();
    assert_eq!(second.local_addr().unwrap(), address);
    drop(first);
    drop(second);
    assert_udp_rebinds(|| network.bind_udp(address));

    let mut old = Network::test_provider();
    old.capabilities.features &= !abi::SHARED_UDP;
    assert_eq!(
        old.bind_udp_shared(address).unwrap_err().kind(),
        std::io::ErrorKind::Unsupported
    );
}
#[test]
fn unchanged_shared_udp_consumer_uses_replacement_provider() {
    let table = standalone();
    // SAFETY: the fixture owns the v1.2 table exclusively before handing it to the SDK.
    unsafe {
        let extended = &mut *table.cast_mut().cast::<abi::NetworkApiV1_2>();
        SHARED_CALLBACK.set(extended.udp_bind_shared).ok().unwrap();
        extended.udp_bind_shared = replacement_shared;
    }
    // SAFETY: ownership of the valid table and release callback transfers once.
    let network = unsafe { Network::from_native_table(table) }.unwrap();
    let before = REPLACEMENT_SHARED_CALLS.load(Ordering::SeqCst);
    let socket = network
        .bind_udp_shared("127.0.0.1:0".parse().unwrap())
        .unwrap();
    assert!(socket.local_addr().unwrap().port() != 0);
    assert_eq!(REPLACEMENT_SHARED_CALLS.load(Ordering::SeqCst), before + 1);
}
#[test]
fn tcp_wrappers_support_tls_style_read_write_and_cloned_listener_ownership() {
    let network = Network::test_provider();
    let listener = network
        .bind_tcp_with_options(
            "127.0.0.1:0".parse().unwrap(),
            TcpListenOptions {
                nonblocking: false,
                ..Default::default()
            },
        )
        .unwrap();
    let address = listener.local_addr().unwrap();
    let client_network = network.clone();
    let worker = std::thread::spawn(move || {
        let mut client = client_network
            .connect_tcp(address, Duration::from_secs(1))
            .unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        client.write_all(b"request").unwrap();
        let mut reply = [0; 5];
        client.read_exact(&mut reply).unwrap();
        assert_eq!(&reply, b"reply");
    });
    let (mut accepted, peer) = listener.accept().unwrap();
    assert!(peer.ip().is_loopback());
    accepted.set_nonblocking(false).unwrap();
    accepted
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    let mut bytes = [0; 7];
    accepted.read_exact(&mut bytes).unwrap();
    assert_eq!(&bytes, b"request");
    // The outbound Read must wait for bytes rather than expose WouldBlock just
    // because its peer has not produced the TLS-style reply yet.
    std::thread::sleep(Duration::from_millis(50));
    accepted.write_all(b"reply").unwrap();
    worker.join().unwrap();
}
#[test]
fn incompatible_owned_table_releases_well_formed_prefix_resources() {
    let table = standalone();
    let mut id = 0;
    let address = address_to_native("127.0.0.1:0".parse().unwrap());
    // SAFETY: standalone table exclusively owned here; no calls race its mutation.
    unsafe {
        assert_eq!(
            ((*table).udp_bind)((*table).base.context, address, &mut id),
            STATUS_OK
        );
        let mut bound = abi::Address::default();
        assert_eq!(
            ((*table).local_addr)((*table).base.context, id, &mut bound),
            STATUS_OK
        );
        let address = address_from_native(bound).unwrap();
        let prefix = table.cast_mut().cast::<NativeInterface>();
        (*prefix).header.major = 99;
        assert!(Network::from_native_table(table).is_err());
        assert_udp_rebinds(|| std::net::UdpSocket::bind(address));
    }
}
#[test]
fn native_addresses_round_trip_scope_and_reject_malformed_v4() {
    for address in [
        "127.0.0.1:42".parse().unwrap(),
        SocketAddr::V6(SocketAddrV6::new(Ipv6Addr::LOCALHOST, 9, 3, 7)),
    ] {
        assert_eq!(
            address_from_native(address_to_native(address)).unwrap(),
            address
        );
    }
    let mut malformed = address_to_native("127.0.0.1:9".parse().unwrap());
    malformed.bytes[8] = 7;
    assert_eq!(
        address_from_native(malformed).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
}

#[test]
fn connected_udp_preserves_zero_length_datagrams() {
    let network = Network::test_provider();
    let sender = network.bind_udp("127.0.0.1:0".parse().unwrap()).unwrap();
    let receiver = network.bind_udp("127.0.0.1:0".parse().unwrap()).unwrap();
    receiver.set_nonblocking(false).unwrap();
    receiver
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    sender.connect(receiver.local_addr().unwrap()).unwrap();
    assert_eq!(sender.send(&[]).unwrap(), 0);
    assert_eq!(receiver.recv(&mut [0; 1]).unwrap(), 0);
    sender.send(b"discarded").unwrap();
    assert_eq!(receiver.recv(&mut []).unwrap(), 0);
    receiver.set_nonblocking(true).unwrap();
    assert_eq!(
        receiver.recv(&mut [0; 1]).unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
}

#[test]
fn timed_receive_preserves_datagrams_for_modern_and_prefix_only_providers() {
    for prefix_only in [false, true] {
        let table = standalone();
        if prefix_only {
            // SAFETY: exclusive standalone ownership, before binding it into the SDK.
            // This simulates the unchanged v1 provider contract without exposing its tail.
            unsafe {
                let prefix = table.cast_mut().cast::<NativeInterface>();
                (*prefix).header.minor = 0;
                (*prefix).header.size = size_of::<abi::NetworkApi>() as u32;
            }
        }
        // SAFETY: standalone transfers exactly one owned live provider table.
        let network = unsafe { Network::from_native_table(table).unwrap() };
        let receiver = network.bind_udp("127.0.0.1:0".parse().unwrap()).unwrap();
        receiver.set_nonblocking(false).unwrap();
        receiver
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        let sender = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        sender
            .send_to(b"one whole datagram", receiver.local_addr().unwrap())
            .unwrap();
        if !prefix_only {
            std::thread::sleep(Duration::from_millis(20));
        }
        let mut payload = [0u8; 64];
        let (count, peer, timing) = receiver.recv_from_timed(&mut payload).unwrap();
        assert_eq!(&payload[..count], b"one whole datagram");
        assert_eq!(peer, sender.local_addr().unwrap());
        assert_eq!(timing.is_none(), prefix_only);
        if !prefix_only && cfg!(target_os = "linux") {
            let timing = timing.unwrap();
            assert_ne!(timing.flags & abi::KERNEL_RESIDENCE_REALTIME, 0);
            assert!(timing.kernel_residence_us >= 10_000);
            let pressure = receiver.diagnostics().unwrap();
            assert_ne!(pressure.flags & abi::SOCKET_MEMORY, 0);
            assert!(pressure.receive_buffer_bytes > 0);
        }
    }
}

unsafe extern "C" fn replacement_reverse(
    _context: *mut c_void,
    address: abi::Address,
    timeout_ms: u32,
    output: *mut u8,
    capacity: usize,
    count: *mut usize,
) -> Status {
    let hostname = b"replacement-client";
    if address_from_native(address).unwrap().ip() != std::net::Ipv4Addr::LOCALHOST
        || timeout_ms == 0
        || capacity < hostname.len()
        || output.is_null()
        || count.is_null()
    {
        return amaterasu_abi::STATUS_INVALID_ARGUMENT;
    }
    // SAFETY: the SDK supplies sufficient writable bytes and a count pointer,
    // retained for this callback. The fixture does not retain either borrow.
    unsafe {
        std::ptr::copy_nonoverlapping(hostname.as_ptr(), output, hostname.len());
        count.write(hostname.len());
    }
    STATUS_OK
}

#[test]
fn unchanged_reverse_consumer_uses_selected_provider_and_supports_older_tables() {
    fn consumer(network: &Network) -> io::Result<String> {
        network
            .reverse_resolve_timeout(std::net::Ipv4Addr::LOCALHOST.into(), Duration::from_secs(1))
    }
    let table = standalone();
    // SAFETY: standalone returns a uniquely owned default-provider v1.3 table.
    // Only its reverse callback changes before ownership transfers to the SDK.
    unsafe {
        (*table.cast_mut().cast::<abi::NetworkApiV1_3>()).reverse_resolve = replacement_reverse;
    }
    // SAFETY: the well-formed table and release ownership transfer exactly once.
    let network = unsafe { Network::from_native_table(table) }.unwrap();
    assert_eq!(consumer(&network).unwrap(), "replacement-client");
    drop(network);
    let table = standalone();
    // SAFETY: emulate an older prefix without changing ownership or release.
    unsafe {
        (*table.cast_mut()).base.header.minor = 0;
        (*table.cast_mut()).base.header.size = size_of::<abi::NetworkApi>() as u32;
    }
    // SAFETY: ownership of this valid v1 prefix transfers to the unchanged SDK.
    let network = unsafe { Network::from_native_table(table) }.unwrap();
    assert_eq!(
        consumer(&network).unwrap_err().kind(),
        io::ErrorKind::Unsupported
    );
}

#[test]
fn default_reverse_provider_resolves_loopback_through_the_owned_helper() {
    let network = Network::test_provider();
    let name = network
        .reverse_resolve_timeout(std::net::Ipv4Addr::LOCALHOST.into(), Duration::from_secs(2))
        .unwrap();
    assert!(!name.is_empty());
    assert!(name.parse::<IpAddr>().is_err());
}
