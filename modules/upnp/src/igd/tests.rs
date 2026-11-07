use super::*;

#[test]
fn discovery_location_is_bound_to_response_source_and_has_no_credential_or_remote_redirect() {
    let advertisement = |location| format!("HTTP/1.1 200 OK\r\nLOCATION: {location}\r\n\r\n");
    let source = "192.168.1.1:1900".parse().unwrap();
    assert_eq!(
        advertised_location(
            advertisement("http://192.168.1.1:80/root.xml").as_bytes(),
            source
        ),
        Some(("192.168.1.1:80".parse().unwrap(), "/root.xml".into()))
    );
    for location in [
        "http://192.168.1.2/root.xml",
        "http://user@192.168.1.1/root.xml",
        "https://192.168.1.1/root.xml",
        "http://192.168.1.1//other-host/control",
    ] {
        assert!(advertised_location(advertisement(location).as_bytes(), source).is_none());
    }
}

#[test]
fn descriptions_support_wanppp_and_both_wanip_versions_with_relative_control_paths() {
    for service in SERVICES {
        let xml = format!(
            "<root><URLBase>http://192.168.1.1:1234/base/</URLBase><device><deviceList><device><serviceList><service><serviceType>{service}</serviceType><controlURL>mapping</controlURL></service></serviceList></device></deviceList></device></root>"
        );
        let root = Element::parse(xml.as_bytes()).unwrap();
        assert_eq!(
            service_control(&root),
            Some((service.into(), "mapping".into()))
        );
        assert_eq!(
            control_target(
                &root,
                "192.168.1.1:80".parse().unwrap(),
                "/root.xml",
                "mapping"
            ),
            Some(("192.168.1.1:1234".parse().unwrap(), "/base/mapping".into()))
        );
    }
}

#[test]
fn control_url_cannot_switch_gateway_or_inject_headers() {
    let root = Element::parse(b"<root/>".as_slice()).unwrap();
    let address = "192.168.1.1:80".parse().unwrap();
    assert!(control_target(&root, address, "/root.xml", "http://192.168.1.2/control").is_none());
    assert!(control_target(&root, address, "/root.xml", "/control\r\nExtra: x").is_none());
    assert!(control_target(&root, address, "/root.xml", "//other-host/control").is_none());
}

fn mapping_xml() -> String {
    "<Envelope><Body><GetGenericPortMappingEntryResponse><NewRemoteHost/><NewExternalPort>47998</NewExternalPort><NewProtocol>UDP</NewProtocol><NewInternalPort>47998</NewInternalPort><NewInternalClient>192.168.1.5</NewInternalClient><NewEnabled>1</NewEnabled><NewPortMappingDescription>Amaterasu owner</NewPortMappingDescription><NewLeaseDuration>3600</NewLeaseDuration></GetGenericPortMappingEntryResponse></Body></Envelope>".into()
}

#[test]
fn mapping_response_keeps_complete_ownership_attributes_and_rejects_malformed_values() {
    let xml = mapping_xml();
    let root = Element::parse(xml.as_bytes()).unwrap();
    let mapping = mapping_from_response(&root).unwrap();
    assert_eq!(mapping.external_port, 47998);
    assert_eq!(mapping.protocol, Protocol::Udp);
    assert_eq!(
        mapping.internal_address,
        "192.168.1.5".parse::<Ipv4Addr>().unwrap()
    );
    assert!(mapping.enabled);
    assert_eq!(mapping.remote_host, "");
    assert_eq!(mapping.description, "Amaterasu owner");
    assert_eq!(mapping.lease_duration_seconds, 3600);
    for (before, after) in [
        ("UDP", "RAW"),
        ("<NewEnabled>1", "<NewEnabled>2"),
        ("192.168.1.5", "::1"),
        ("<NewLeaseDuration>3600", "<NewLeaseDuration>-1"),
    ] {
        let xml = xml.replace(before, after);
        assert!(mapping_from_response(&Element::parse(xml.as_bytes()).unwrap()).is_err());
    }
}

#[test]
fn mapping_soap_uses_negotiated_service_and_escapes_parameters() {
    let soap = soap_envelope(
        SERVICES[2],
        "AddPortMapping",
        &[
            ("PortMappingDescription", "A&B <test>".into()),
            ("Protocol", "UDP".into()),
        ],
    );
    assert!(soap.contains(SERVICES[2]));
    assert!(
        soap.contains(
            "<NewPortMappingDescription>A&amp;B &lt;test&gt;</NewPortMappingDescription>"
        )
    );
    assert!(soap.contains("<NewProtocol>UDP</NewProtocol>"));
}

fn serve_soap_responses(
    listener: std::net::TcpListener,
    responses: Vec<(u16, String)>,
) -> std::thread::JoinHandle<Vec<String>> {
    use std::io::{Read, Write};
    std::thread::spawn(move || {
        let mut requests = Vec::new();
        for (status, body) in responses {
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
                    let header_end = position + 4;
                    let headers = std::str::from_utf8(&bytes[..position]).unwrap();
                    let length = headers
                        .lines()
                        .filter_map(|line| line.split_once(':'))
                        .find_map(|(name, value)| {
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    if bytes.len() >= header_end + length {
                        break;
                    }
                }
            }
            requests.push(String::from_utf8(bytes).unwrap());
            let response = format!(
                "HTTP/1.1 {status} Result\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).unwrap();
        }
        requests
    })
}

#[test]
fn ipv4_mapping_inspection_and_safe_delete_use_selected_provider_tcp() {
    let network = Network::test_provider();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let mapping =
        mapping_from_response(&Element::parse(mapping_xml().as_bytes()).unwrap()).unwrap();
    let fault = "<Envelope><Body><Fault><detail><UPnPError><errorCode>713</errorCode><errorDescription>SpecifiedArrayIndexInvalid</errorDescription></UPnPError></detail></Fault></Body></Envelope>".to_owned();
    let response = "<Envelope><Body><DeletePortMappingResponse/></Body></Envelope>".to_owned();
    let peer = serve_soap_responses(
        listener,
        vec![(200, mapping_xml()), (500, fault), (200, response)],
    );
    let mut gateway = IgdGateway {
        network,
        address,
        root_path: "/root.xml".into(),
        control_path: "/control/mapping".into(),
        service: SERVICES[2].into(),
        internal_address: mapping.internal_address,
        operation_deadline: OperationDeadline::default(),
    };
    assert_eq!(
        gateway.remove_if_matches(&mapping).unwrap(),
        RemovalOutcome::Removed
    );
    let requests = peer.join().unwrap();
    assert_eq!(requests.len(), 3);
    assert!(requests[0].contains("<NewPortMappingIndex>0</NewPortMappingIndex>"));
    assert!(requests[1].contains("<NewPortMappingIndex>1</NewPortMappingIndex>"));
    assert!(requests[2].contains("WANIPConnection:2#DeletePortMapping"));
    assert!(requests[2].contains("<NewExternalPort>47998</NewExternalPort>"));
    assert!(requests[2].contains("<NewProtocol>UDP</NewProtocol>"));
}

#[test]
fn shutdown_budget_restricted_during_soap_stops_later_scan_and_delete_effects() {
    use std::io::{Read, Write};
    use std::sync::mpsc;

    let operation_deadline = OperationDeadline::default();
    let peer_deadline = operation_deadline.clone();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let (stop, stopped) = mpsc::channel();
    let peer = std::thread::spawn(move || {
        let accept_deadline = Instant::now() + Duration::from_secs(2);
        let mut requests = Vec::new();
        let responses = [
            (200, mapping_xml()),
            (500, "<Envelope><Body><Fault><detail><UPnPError><errorCode>713</errorCode><errorDescription>SpecifiedArrayIndexInvalid</errorDescription></UPnPError></detail></Fault></Body></Envelope>".into()),
            (200, "<Envelope><Body><DeletePortMappingResponse/></Body></Envelope>".into()),
        ];
        for (status, body) in responses {
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if stopped.try_recv().is_ok() || Instant::now() >= accept_deadline {
                            return requests;
                        }
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    Err(error) => panic!("loopback SOAP peer failed: {error}"),
                }
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(1)))
                .unwrap();
            let mut bytes = Vec::new();
            let mut buffer = [0; 2048];
            loop {
                let count = stream.read(&mut buffer).unwrap();
                assert_ne!(count, 0);
                bytes.extend_from_slice(&buffer[..count]);
                if let Some(position) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                    let header_end = position + 4;
                    let headers = std::str::from_utf8(&bytes[..position]).unwrap();
                    let length = headers
                        .lines()
                        .filter_map(|line| line.split_once(':'))
                        .find_map(|(name, value)| {
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    if bytes.len() >= header_end + length {
                        break;
                    }
                }
            }
            requests.push(String::from_utf8(bytes).unwrap());
            // The first submitted read is allowed to complete. Its adapter must
            // observe this restriction before any later SOAP connection/effect.
            peer_deadline.restrict(Instant::now());
            let response = format!(
                "HTTP/1.1 {status} Result\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
        }
        requests
    });

    let mapping =
        mapping_from_response(&Element::parse(mapping_xml().as_bytes()).unwrap()).unwrap();
    let gateway = IgdGateway {
        network: Network::test_provider(),
        address,
        root_path: "/root.xml".into(),
        control_path: "/control/mapping".into(),
        service: SERVICES[2].into(),
        internal_address: mapping.internal_address,
        operation_deadline,
    };
    let mut manager = crate::MappingManager::with_owner_id(gateway, "retained-budget-owner");
    manager.owned.insert(mapping.key(), mapping.clone());
    let result = manager.shutdown_until(Instant::now() + Duration::from_secs(30));
    let _ = stop.send(());
    let requests = peer.join().unwrap();

    assert!(
        result.is_err(),
        "an expired scan must not report a successful delete"
    );
    assert_eq!(
        requests.len(),
        1,
        "later SOAP steps must not renew a fresh HTTP budget"
    );
    assert!(requests[0].contains("#GetGenericPortMappingEntry"));
    assert_eq!(manager.owned_mappings().collect::<Vec<_>>(), vec![&mapping]);
}
