//! Network source policy is independent of browser origins and authentication.
use serde::{Deserialize, Serialize};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum PeerTier {
    Pc,
    #[default]
    Lan,
    Wan,
}

pub(crate) fn classify(address: IpAddr) -> PeerTier {
    match address.to_canonical() {
        IpAddr::V4(address) => classify_v4(address),
        IpAddr::V6(address) => classify_v6(address),
    }
}

fn classify_v4(address: Ipv4Addr) -> PeerTier {
    let [first, second, ..] = address.octets();
    if first == 127 {
        PeerTier::Pc
    } else if first == 10
        || (first == 172 && (16..=31).contains(&second))
        || (first == 192 && second == 168)
        || (first == 100 && (64..=127).contains(&second))
        || (first == 169 && second == 254)
    {
        PeerTier::Lan
    } else {
        PeerTier::Wan
    }
}

fn classify_v6(address: Ipv6Addr) -> PeerTier {
    let segments = address.segments();
    if address == Ipv6Addr::LOCALHOST {
        PeerTier::Pc
    } else if segments[0] & 0xfe00 == 0xfc00
        // The pinned source permits fe80::/64, rather than the wider link-local /10.
        || segments[..4] == [0xfe80, 0, 0, 0]
    {
        PeerTier::Lan
    } else {
        PeerTier::Wan
    }
}

pub(crate) fn permits(maximum: Option<PeerTier>, address: IpAddr, tls: bool) -> bool {
    let Some(maximum) = maximum else {
        return false;
    };
    let source = classify(address);
    source <= maximum && (tls || source == PeerTier::Pc)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ipv4_classification_preserves_each_pinned_range_boundary() {
        for (tier, addresses) in [
            (
                PeerTier::Pc,
                &["127.0.0.0", "127.0.0.1", "127.255.255.255"][..],
            ),
            (
                PeerTier::Lan,
                &[
                    "10.0.0.0",
                    "10.255.255.255",
                    "172.16.0.0",
                    "172.31.255.255",
                    "192.168.0.0",
                    "192.168.255.255",
                    "100.64.0.0",
                    "100.127.255.255",
                    "169.254.0.0",
                    "169.254.255.255",
                ][..],
            ),
            (
                PeerTier::Wan,
                &[
                    "0.0.0.0",
                    "9.255.255.255",
                    "11.0.0.0",
                    "126.255.255.255",
                    "128.0.0.0",
                    "172.15.255.255",
                    "172.32.0.0",
                    "192.167.255.255",
                    "192.169.0.0",
                    "100.63.255.255",
                    "100.128.0.0",
                    "169.253.255.255",
                    "169.255.0.0",
                    "224.0.0.1",
                    "255.255.255.255",
                ][..],
            ),
        ] {
            for address in addresses {
                assert_eq!(classify(address.parse().unwrap()), tier, "{address}");
            }
        }
    }

    #[test]
    fn ipv6_classification_uses_exact_source_prefixes() {
        for (address, tier) in [
            ("::1", PeerTier::Pc),
            ("::", PeerTier::Wan),
            ("::2", PeerTier::Wan),
            ("fbff:ffff:ffff:ffff:ffff:ffff:ffff:ffff", PeerTier::Wan),
            ("fc00::", PeerTier::Lan),
            ("fdff:ffff:ffff:ffff:ffff:ffff:ffff:ffff", PeerTier::Lan),
            ("fe00::", PeerTier::Wan),
            ("fe7f:ffff:ffff:ffff:ffff:ffff:ffff:ffff", PeerTier::Wan),
            ("fe80::", PeerTier::Lan),
            ("fe80::ffff:ffff:ffff:ffff", PeerTier::Lan),
            ("fe80:0:0:1::", PeerTier::Wan),
            ("fe81::", PeerTier::Wan),
            ("febf::1", PeerTier::Wan),
            ("2001:db8::1", PeerTier::Wan),
            ("ff02::1", PeerTier::Wan),
        ] {
            assert_eq!(classify(address.parse().unwrap()), tier, "{address}");
        }
    }

    #[test]
    fn ipv4_mapped_addresses_use_the_ipv4_policy() {
        for (address, tier) in [
            ("::ffff:127.42.0.1", PeerTier::Pc),
            ("::ffff:10.0.0.1", PeerTier::Lan),
            ("::ffff:100.64.0.1", PeerTier::Lan),
            ("::ffff:169.254.0.1", PeerTier::Lan),
            ("::ffff:8.8.8.8", PeerTier::Wan),
            ("::127.0.0.1", PeerTier::Wan),
        ] {
            assert_eq!(classify(address.parse().unwrap()), tier, "{address}");
        }
    }

    #[test]
    fn policy_is_a_tls_maximum_and_plaintext_stays_on_the_pc() {
        for (address, source) in [
            ("127.0.0.2", PeerTier::Pc),
            ("192.168.0.2", PeerTier::Lan),
            ("203.0.113.2", PeerTier::Wan),
        ] {
            let address = address.parse().unwrap();
            assert!(!permits(None, address, true));
            assert!(!permits(None, address, false));
            for maximum in [PeerTier::Pc, PeerTier::Lan, PeerTier::Wan] {
                assert_eq!(permits(Some(maximum), address, true), source <= maximum);
                assert_eq!(
                    permits(Some(maximum), address, false),
                    source == PeerTier::Pc
                );
            }
        }
    }

    #[test]
    fn only_exact_policy_spellings_are_deserialized() {
        assert_eq!(PeerTier::default(), PeerTier::Lan);
        for tier in [PeerTier::Pc, PeerTier::Lan, PeerTier::Wan] {
            assert_eq!(
                serde_json::from_value::<PeerTier>(serde_json::to_value(tier).unwrap()).unwrap(),
                tier
            );
        }
        for invalid in ["PC", "LAN", "remote", "", " pc"] {
            assert!(serde_json::from_value::<PeerTier>(serde_json::json!(invalid)).is_err());
        }
    }
}
