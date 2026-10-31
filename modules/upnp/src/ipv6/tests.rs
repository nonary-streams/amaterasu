use std::net::{Ipv6Addr, SocketAddr, SocketAddrV6};

use super::*;
use crate::PINHOLE_LEASE_SECONDS;

#[test]
fn ssdp_location_keeps_interface_scope_for_link_local_control() {
    let location = "http://[fe80::1234%25eth0]:49152/root/device.xml";
    let (address, path) = parse_ipv6_http_location(location, 7).unwrap();

    assert_eq!(address.ip(), &"fe80::1234".parse::<Ipv6Addr>().unwrap());
    assert_eq!(address.port(), 49152);
    assert_eq!(address.scope_id(), 7);
    assert_eq!(path, "/root/device.xml");
}

#[test]
fn ssdp_description_finds_ipv6_firewall_service_and_resolves_relative_control_url() {
    let description = Element::parse(
        r#"<root xmlns="urn:schemas-upnp-org:device-1-0"><device><serviceList><service><serviceType>urn:schemas-upnp-org:service:WANIPv6FirewallControl:1</serviceType><controlURL>control/firewall</controlURL></service></serviceList></device></root>"#.as_bytes(),
    )
    .unwrap();
    let control = find_control_url(&description).unwrap();
    assert_eq!(
        resolve_control_path("/root/device.xml", &control).as_deref(),
        Some("/root/control/firewall")
    );
}

#[test]
fn ssdp_description_url_base_resolves_relative_control_on_same_gateway() {
    let description = Element::parse(
        r#"<root xmlns="urn:schemas-upnp-org:device-1-0"><URLBase>http://[fe80::1%257]:49153/base/</URLBase><device><serviceList><service><serviceType>urn:schemas-upnp-org:service:WANIPv6FirewallControl:1</serviceType><controlURL>control/firewall</controlURL></service></serviceList></device></root>"#.as_bytes(),
    )
    .unwrap();
    let device_address = SocketAddrV6::new("fe80::1".parse().unwrap(), 49152, 0, 7);
    let control_url = find_control_url(&description).unwrap();

    let (control_address, path) = resolve_control_target(
        &description,
        device_address,
        "/root/device.xml",
        7,
        &control_url,
    )
    .unwrap();

    assert_eq!(
        control_address,
        SocketAddrV6::new("fe80::1".parse().unwrap(), 49153, 0, 7)
    );
    assert_eq!(path, "/base/control/firewall");
}

#[test]
fn ssdp_description_rejects_url_base_on_a_different_gateway() {
    let description = Element::parse(
        r#"<root xmlns="urn:schemas-upnp-org:device-1-0"><URLBase>http://[fe80::2]:49153/base/</URLBase><device><serviceList><service><serviceType>urn:schemas-upnp-org:service:WANIPv6FirewallControl:1</serviceType><controlURL>control/firewall</controlURL></service></serviceList></device></root>"#.as_bytes(),
    )
    .unwrap();
    let device_address = SocketAddrV6::new("fe80::1".parse().unwrap(), 49152, 0, 7);
    let control_url = find_control_url(&description).unwrap();

    assert!(
        resolve_control_target(
            &description,
            device_address,
            "/root/device.xml",
            7,
            &control_url,
        )
        .is_none()
    );
}

#[test]
fn ipv6_ssdp_fixture_uses_same_source_and_location_gateway() {
    let response = format!(
        "HTTP/1.1 200 OK\r\nST: {IGD_DEVICE_TYPE}\r\nLOCATION: http://[fe80::1%253]:49152/root.xml\r\n\r\n"
    );
    let source = SocketAddr::V6(SocketAddrV6::new("fe80::1".parse().unwrap(), 1900, 0, 3));
    let extracted = parse_ssdp_advertisement(response.as_bytes());
    assert_eq!(
        extracted,
        Some((IGD_DEVICE_TYPE, "http://[fe80::1%253]:49152/root.xml"))
    );
    assert!(source_matches_location(
        source,
        "fe80::1".parse::<Ipv6Addr>().unwrap()
    ));
}

#[test]
fn soap_requests_encode_reference_protocol_numbers_and_arguments() {
    let endpoint = PinholeEndpoint::new("2001:db8::9".parse().unwrap(), 47998, Protocol::Udp);
    let xml = soap_envelope(
        "AddPinhole",
        &[
            ("RemoteHost", "".to_owned()),
            ("RemotePort", "0".to_owned()),
            ("InternalClient", endpoint.internal_client.to_string()),
            ("InternalPort", endpoint.internal_port.to_string()),
            ("Protocol", protocol_number(endpoint.protocol).to_owned()),
            ("LeaseTime", PINHOLE_LEASE_SECONDS.to_string()),
        ],
    );

    assert!(xml.contains("<NewProtocol>17</NewProtocol>"));
    assert!(xml.contains("<NewInternalClient>2001:db8::9</NewInternalClient>"));
    assert!(xml.contains("<NewLeaseTime>3600</NewLeaseTime>"));
    assert!(xml.contains(IPV6_FIREWALL_SERVICE));
}

#[test]
fn status_and_unique_id_soap_values_parse_from_response_fixture() {
    let response = Element::parse(
        br#"<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/"><s:Body><u:GetFirewallStatusResponse xmlns:u="urn:schemas-upnp-org:service:WANIPv6FirewallControl:1"><NewFirewallEnabled>0</NewFirewallEnabled><NewInboundPinholeAllowed>1</NewInboundPinholeAllowed></u:GetFirewallStatusResponse></s:Body></s:Envelope>"#.as_slice(),
    )
    .unwrap();

    assert!(!xml_boolean(&response, "NewFirewallEnabled").unwrap());
    assert!(xml_boolean(&response, "NewInboundPinholeAllowed").unwrap());
    let id = Element::parse(
        br#"<AddPinholeResponse><NewUniqueID>47</NewUniqueID></AddPinholeResponse>"#.as_slice(),
    )
    .unwrap();
    assert_eq!(
        xml_value(&id, "NewUniqueID")
            .unwrap()
            .parse::<u16>()
            .unwrap(),
        47
    );
}

#[test]
fn content_length_and_path_validation_are_bounded() {
    assert_eq!(
        parse_content_length(b"HTTP/1.1 200 OK\r\nContent-Length: 37\r\n\r\n"),
        Some(37)
    );
    assert!(valid_http_path("/root/control"));
    assert!(!valid_http_path("//other-host/control"));
    assert!(!valid_http_path("/root/\r\nInjected: true"));
}

#[test]
fn http_reader_supports_bounded_content_length_chunked_and_connection_close_bodies() {
    let responses: &[&[u8]] = &[
        b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\nabc",
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n1\r\na\r\n2;extension=x\r\nbc\r\n0\r\n\r\n",
        b"HTTP/1.0 200 OK\r\n\r\nabc",
    ];
    for response in responses {
        let response = read_http_response(std::io::Cursor::new(response), 3).unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(response.body, b"abc");
    }
}

#[test]
fn http_reader_rejects_truncated_ambiguous_and_oversize_body_framing() {
    let responses: &[&[u8]] = &[
        b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\nabc",
        b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\nContent-Length: 3\r\n\r\nabc",
        b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\nTransfer-Encoding: chunked\r\n\r\nabc",
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\nabcd\r\n0\r\n\r\n",
        b"HTTP/1.0 200 OK\r\n\r\nabcd",
    ];
    for response in responses {
        assert!(read_http_response(std::io::Cursor::new(response), 3).is_err());
    }
}

#[test]
fn ipv6_firewall_status_and_pinhole_creation_use_selected_provider_tcp() {
    use std::io::{Read, Write};
    let network = Network::test_provider();
    let listener = std::net::TcpListener::bind("[::1]:0").unwrap();
    let SocketAddr::V6(address) = listener.local_addr().unwrap() else {
        panic!("IPv6 listener required");
    };
    let peer = std::thread::spawn(move || {
        let mut requests = Vec::new();
        for body in [
            "<Envelope><Body><GetFirewallStatusResponse><NewFirewallEnabled>1</NewFirewallEnabled><NewInboundPinholeAllowed>1</NewInboundPinholeAllowed></GetFirewallStatusResponse></Body></Envelope>",
            "<Envelope><Body><AddPinholeResponse><NewUniqueID>17</NewUniqueID></AddPinholeResponse></Body></Envelope>",
        ] {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut bytes = Vec::new();
            let mut buffer = [0; 2048];
            loop {
                let count = stream.read(&mut buffer).unwrap();
                assert_ne!(count, 0);
                bytes.extend_from_slice(&buffer[..count]);
                if let Some(position) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                    let end = position + 4;
                    let length = parse_content_length(&bytes[..end]).unwrap_or(0);
                    if bytes.len() >= end + length {
                        break;
                    }
                }
            }
            requests.push(String::from_utf8(bytes).unwrap());
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).unwrap();
        }
        requests
    });
    let mut firewall = IgdIpv6Firewall {
        network,
        control_address: address,
        control_path: "/ipv6/firewall".into(),
        internal_addresses: vec!["2001:db8::9".parse().unwrap()],
        identity: "test".into(),
    };
    let status = firewall.firewall_status().unwrap();
    assert!(status.firewall_enabled && status.pinhole_allowed);
    assert_eq!(
        firewall
            .add_pinhole(
                PinholeEndpoint::new("2001:db8::9".parse().unwrap(), 47998, Protocol::Udp),
                3600
            )
            .unwrap(),
        17
    );
    let requests = peer.join().unwrap();
    assert!(requests[0].contains("WANIPv6FirewallControl:1#GetFirewallStatus"));
    assert!(requests[1].contains("<NewInternalClient>2001:db8::9</NewInternalClient>"));
    assert!(requests[1].contains("<NewProtocol>17</NewProtocol>"));
    assert!(requests[1].contains("<NewLeaseTime>3600</NewLeaseTime>"));
}

#[test]
fn http_total_deadline_stops_a_gateway_that_keeps_trickling_body_bytes() {
    use std::io::{Read, Write};
    let network = Network::test_provider();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let peer = std::thread::spawn(move || {
        let accept_deadline = Instant::now() + Duration::from_secs(1);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error)
                    if error.kind() == io::ErrorKind::WouldBlock
                        && Instant::now() < accept_deadline =>
                {
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return,
                Err(error) => panic!("test peer accept failed: {error}"),
            }
        };
        stream
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        let mut request = Vec::new();
        let mut buffer = [0; 1024];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let length = match stream.read(&mut buffer) {
                Ok(length) if length > 0 => length,
                _ => return,
            };
            request.extend_from_slice(&buffer[..length]);
        }
        if stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 64\r\n\r\n")
            .is_err()
        {
            return;
        }
        for _ in 0..64 {
            if stream.write_all(b"x").is_err() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    });
    let started = Instant::now();
    let result = http_get_until(
        &network,
        address,
        "/description.xml",
        1024,
        started + Duration::from_millis(60),
    );
    assert!(
        result.is_err(),
        "a continuously readable response must still expire"
    );
    let elapsed = started.elapsed();
    assert!(elapsed < Duration::from_secs(1));
    assert!(elapsed >= Duration::from_millis(30));
    peer.join().unwrap();
}

#[test]
fn http_partial_writes_share_one_absolute_deadline() {
    use std::cell::RefCell;
    struct SlowWriter {
        written: usize,
        timeouts: RefCell<Vec<Duration>>,
    }
    impl Read for SlowWriter {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Ok(0)
        }
    }
    impl Write for SlowWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            std::thread::sleep(Duration::from_millis(10));
            let count = usize::from(!bytes.is_empty());
            self.written += count;
            Ok(count)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    impl TimedStream for SlowWriter {
        fn read_timeout(&self, _: Duration) -> io::Result<()> {
            Ok(())
        }
        fn write_timeout(&self, duration: Duration) -> io::Result<()> {
            self.timeouts.borrow_mut().push(duration);
            Ok(())
        }
    }
    let mut stream = DeadlineIo {
        stream: SlowWriter {
            written: 0,
            timeouts: RefCell::new(Vec::new()),
        },
        deadline: Instant::now() + Duration::from_millis(45),
    };
    let error = stream.write_all(&[7; 64]).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert!(stream.stream.written > 0 && stream.stream.written < 64);
    let timeouts = stream.stream.timeouts.borrow();
    assert!(timeouts.windows(2).all(|pair| pair[1] < pair[0]));
}
