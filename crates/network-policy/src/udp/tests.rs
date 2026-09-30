use super::*;
use socket2::SockRef;

#[test]
fn loopback_udp_is_nonblocking_exclusive_and_keeps_the_requested_address() {
    for address in ["127.0.0.1:0", "[::1]:0"] {
        let socket = bind_udp(address.parse().unwrap()).unwrap();
        let local = socket.local_addr().unwrap();
        assert_eq!(local.ip(), address.parse::<SocketAddr>().unwrap().ip());
        assert_ne!(local.port(), 0);
        assert_eq!(
            socket.recv_from(&mut [0]).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        assert_eq!(
            bind_udp(local).unwrap_err().kind(),
            io::ErrorKind::AddrInUse
        );
        drop(socket);
        assert_eq!(bind_udp(local).unwrap().local_addr().unwrap(), local);
    }
}

#[test]
fn ipv4_and_ipv6_loopback_can_bind_the_same_port_as_separate_families() {
    let ipv4 = bind_udp("127.0.0.1:0".parse().unwrap()).unwrap();
    let port = ipv4.local_addr().unwrap().port();
    let ipv6_address = SocketAddr::new("::1".parse().unwrap(), port);
    let ipv6 = bind_udp(ipv6_address).unwrap();
    assert_eq!(ipv6.local_addr().unwrap(), ipv6_address);
    assert!(SockRef::from(&ipv6).only_v6().unwrap());
    assert_eq!(
        ipv4.local_addr().unwrap().port(),
        ipv6.local_addr().unwrap().port()
    );
}

#[test]
#[ignore = "binds wildcard addresses; run explicitly in an isolated network namespace"]
fn wildcard_udp_keeps_ipv4_and_ipv6_ports_separate() {
    let ipv4 = bind_udp("0.0.0.0:0".parse().unwrap()).unwrap();
    let port = ipv4.local_addr().unwrap().port();
    let ipv6 = bind_udp(SocketAddr::new("::".parse().unwrap(), port)).unwrap();
    assert!(SockRef::from(&ipv6).only_v6().unwrap());
    assert_eq!(
        ipv4.local_addr().unwrap().port(),
        ipv6.local_addr().unwrap().port()
    );
}
