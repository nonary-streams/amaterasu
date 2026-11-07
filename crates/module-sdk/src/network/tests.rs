use super::*;
use amaterasu_abi::{AbiHeader, BorrowedBytes, NativeInterface, NativeInterfaceQuery};
use std::io::{Read, Write};

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
    assert!(std::net::UdpSocket::bind(address).is_ok());
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
        assert!(std::net::UdpSocket::bind(address).is_ok());
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
