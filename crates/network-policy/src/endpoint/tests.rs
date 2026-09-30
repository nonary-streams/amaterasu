use super::*;

fn endpoint(value: &str) -> SocketAddr {
    value.parse().unwrap()
}

#[test]
fn fixed_bind_response_requires_exact_ip_and_port() {
    for requested in ["127.0.0.1:47999", "[::1]:47999"] {
        let requested = endpoint(requested);
        assert_eq!(
            validate_bound_endpoint(requested, Some(requested), requested.ip()),
            Ok(())
        );
        assert_eq!(
            validate_bound_endpoint(
                SocketAddr::new(requested.ip(), 48000),
                Some(requested),
                requested.ip()
            ),
            Err("invalid_bind")
        );
    }
    assert_eq!(
        validate_bound_endpoint(
            endpoint("127.0.0.2:47999"),
            Some(endpoint("127.0.0.1:47999")),
            "127.0.0.1".parse().unwrap()
        ),
        Err("invalid_bind")
    );
    assert_eq!(
        validate_bound_endpoint(
            endpoint("[::1]:47999"),
            Some(endpoint("127.0.0.1:47999")),
            "::1".parse().unwrap()
        ),
        Err("invalid_bind")
    );
}

#[test]
fn wildcard_and_ephemeral_requests_allow_only_the_intended_substitutions() {
    for (wildcard, concrete, peer) in [
        ("0.0.0.0:47999", "192.0.2.10:47999", "192.0.2.20"),
        ("[::]:47999", "[2001:db8::10]:47999", "2001:db8::20"),
    ] {
        for actual in [endpoint(wildcard), endpoint(concrete)] {
            assert_eq!(
                validate_bound_endpoint(actual, Some(endpoint(wildcard)), peer.parse().unwrap()),
                Ok(())
            );
            assert_eq!(
                validate_bound_endpoint(actual, None, peer.parse().unwrap()),
                Ok(())
            );
        }
        assert_eq!(
            validate_bound_endpoint(
                endpoint(wildcard),
                Some(endpoint(concrete)),
                peer.parse().unwrap()
            ),
            Err("invalid_bind")
        );
    }
    assert_eq!(
        validate_bound_endpoint(
            endpoint("127.0.0.1:47999"),
            Some(endpoint("127.0.0.1:0")),
            "127.0.0.1".parse().unwrap()
        ),
        Ok(())
    );
}

#[test]
fn bound_response_rejects_zero_ports_unsafe_ips_and_wrong_families() {
    for (actual, peer) in [
        ("127.0.0.1:0", "127.0.0.1"),
        ("[::1]:0", "::1"),
        ("224.0.0.1:47999", "127.0.0.1"),
        ("255.255.255.255:47999", "127.0.0.1"),
        ("[ff02::1]:47999", "::1"),
        ("[::ffff:127.0.0.1]:47999", "::1"),
        ("127.0.0.1:47999", "::1"),
        ("[::1]:47999", "127.0.0.1"),
    ] {
        assert_eq!(
            validate_bound_endpoint(endpoint(actual), None, peer.parse().unwrap()),
            Err("invalid_bind")
        );
    }
}
