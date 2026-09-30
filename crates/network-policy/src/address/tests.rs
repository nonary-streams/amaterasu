use super::*;

fn ip(value: &str) -> IpAddr {
    value.parse().unwrap()
}

fn policy(
    family: Option<AddressFamily>,
    address: Option<&str>,
    allow_remote: bool,
) -> ListenPolicy<'_> {
    ListenPolicy {
        family,
        address,
        legacy_address: ip("127.0.0.2"),
        allow_remote,
    }
}

#[test]
fn address_family_accepts_only_the_public_configuration_choices() {
    for (value, family) in [("ipv4", AddressFamily::Ipv4), ("both", AddressFamily::Both)] {
        assert_eq!(
            serde_json::from_str::<AddressFamily>(&format!("\"{value}\"")).unwrap(),
            family
        );
        assert_eq!(
            serde_json::to_string(&family).unwrap(),
            format!("\"{value}\"")
        );
    }
    for value in ["ipv6", "IPv4", "", "all"] {
        assert!(serde_json::from_str::<AddressFamily>(&format!("\"{value}\"")).is_err());
    }
    assert!(AddressFamily::Ipv4.supports(ip("127.0.0.1")));
    assert!(!AddressFamily::Ipv4.supports(ip("::1")));
    assert!(AddressFamily::Both.supports(ip("127.0.0.1")));
    assert!(AddressFamily::Both.supports(ip("::1")));
}

#[test]
fn omitted_policy_preserves_the_exact_legacy_address() {
    for legacy in ["127.0.0.2", "::1", "192.0.2.10", "2001:db8::10"] {
        let policy = ListenPolicy {
            legacy_address: ip(legacy),
            ..policy(None, None, true)
        };
        assert_eq!(policy.resolve_ips().unwrap(), [ip(legacy)]);
        assert_eq!(
            policy.effective_family(),
            if ip(legacy).is_ipv4() {
                AddressFamily::Ipv4
            } else {
                AddressFamily::Both
            }
        );
    }
    assert_eq!(
        ListenPolicy {
            legacy_address: ip("192.0.2.10"),
            ..policy(None, None, false)
        }
        .resolve_ips(),
        Err("remote_binding_requires_allow_remote")
    );
}

#[test]
fn explicit_family_uses_defaults_and_blank_legacy_ipv4_retains_its_loopback() {
    for address in [None, Some(""), Some(" \t ")] {
        assert_eq!(
            policy(Some(AddressFamily::Ipv4), address, false)
                .resolve_ips()
                .unwrap(),
            [ip("127.0.0.1")]
        );
        assert_eq!(
            policy(Some(AddressFamily::Both), address, false)
                .resolve_ips()
                .unwrap(),
            [ip("127.0.0.1"), ip("::1")]
        );
        assert_eq!(
            policy(Some(AddressFamily::Ipv4), address, true)
                .resolve_ips()
                .unwrap(),
            [ip("0.0.0.0")]
        );
        assert_eq!(
            policy(Some(AddressFamily::Both), address, true)
                .resolve_ips()
                .unwrap(),
            [ip("0.0.0.0"), ip("::")]
        );
    }
    assert_eq!(
        policy(None, Some(" "), false).resolve_ips().unwrap(),
        [ip("127.0.0.2")]
    );
    assert_eq!(
        policy(None, Some(" "), true).resolve_ips().unwrap(),
        [ip("0.0.0.0")]
    );
    for allow_remote in [false, true] {
        assert_eq!(
            ListenPolicy {
                legacy_address: ip("::1"),
                ..policy(None, Some(" "), allow_remote)
            }
            .resolve_ips()
            .unwrap(),
            if allow_remote {
                vec![ip("0.0.0.0"), ip("::")]
            } else {
                vec![ip("127.0.0.1"), ip("::1")]
            }
        );
    }
}

#[test]
fn explicit_ip_is_trimmed_and_remains_a_single_address() {
    for address in [" 127.0.0.3 ", " ::1 ", " 192.0.2.10 ", " 2001:db8::10 "] {
        let configured = policy(None, Some(address), true);
        assert_eq!(configured.resolve_ips().unwrap(), [ip(address.trim())]);
        assert_eq!(
            configured.effective_family(),
            if ip(address.trim()).is_ipv4() {
                AddressFamily::Ipv4
            } else {
                AddressFamily::Both
            }
        );
    }
    assert_eq!(
        policy(Some(AddressFamily::Ipv4), Some("::1"), true).resolve_ips(),
        Err("address_family_mismatch")
    );
    assert_eq!(
        policy(Some(AddressFamily::Both), Some("127.0.0.3"), false)
            .resolve_ips()
            .unwrap(),
        [ip("127.0.0.3")]
    );
    for address in ["localhost", "127.0.0.1:80", "[::1]", "::1%lo", "127.1"] {
        assert_eq!(
            policy(None, Some(address), true).resolve_ips(),
            Err("invalid_bind_address")
        );
    }
}

#[test]
fn wildcard_and_remote_selection_requires_opt_in_without_opening_sockets() {
    for address in ["0.0.0.0", "::", "192.0.2.10", "2001:db8::10"] {
        assert_eq!(
            policy(None, Some(address), false).resolve_ips(),
            Err("remote_binding_requires_allow_remote")
        );
        assert_eq!(
            policy(None, Some(address), true).resolve_ips().unwrap(),
            [ip(address)]
        );
    }
    for address in [
        "224.0.0.1",
        "255.255.255.255",
        "ff02::1",
        "::ffff:127.0.0.1",
    ] {
        assert!(invalid_configured_bind(ip(address)));
        assert_eq!(
            policy(None, Some(address), true).resolve_ips(),
            Err("invalid_bind_address")
        );
    }
}

#[test]
fn peer_and_local_validation_reject_ambiguous_or_unsafe_addresses() {
    for address in [
        "0.0.0.0",
        "::",
        "224.0.0.1",
        "255.255.255.255",
        "ff02::1",
        "::ffff:127.0.0.1",
    ] {
        assert!(!valid_peer(ip(address)));
    }
    for peer in ["127.0.0.1", "::1", "192.0.2.10", "2001:db8::10"] {
        assert!(valid_peer(ip(peer)));
    }
    for (local, peer) in [
        ("127.0.0.1", "::1"),
        ("::1", "127.0.0.1"),
        ("224.0.0.1", "127.0.0.1"),
        ("255.255.255.255", "127.0.0.1"),
        ("ff02::1", "::1"),
        ("::ffff:127.0.0.1", "::1"),
    ] {
        for allow_remote in [false, true] {
            assert_eq!(
                validate_local_bind(ip(local), ip(peer), allow_remote),
                Err("invalid_bind")
            );
        }
    }
    for (local, peer) in [
        ("0.0.0.0", "192.0.2.10"),
        ("192.0.2.20", "192.0.2.10"),
        ("::", "2001:db8::10"),
        ("2001:db8::20", "2001:db8::10"),
    ] {
        assert_eq!(
            validate_local_bind(ip(local), ip(peer), false),
            Err("invalid_bind")
        );
        assert_eq!(validate_local_bind(ip(local), ip(peer), true), Ok(()));
    }
    for address in ["127.0.0.1", "::1"] {
        assert_eq!(validate_local_bind(ip(address), ip(address), false), Ok(()));
    }
}
