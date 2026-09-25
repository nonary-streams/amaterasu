//! Peer network classification and video encryption policy selection.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// The network category used to select the peer's video encryption policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PeerNetwork {
    /// A loopback address belonging to the local machine.
    Local,
    /// An address in a private or link-local network range.
    Lan,
    /// An address outside the local and LAN ranges.
    Wan,
}

/// Classifies a peer address, treating IPv4-mapped IPv6 addresses as IPv4.
pub fn classify_peer(address: IpAddr) -> PeerNetwork {
    match normalize_address(address) {
        IpAddr::V4(address) => classify_ipv4(address),
        IpAddr::V6(address) => classify_ipv6(address),
    }
}

fn normalize_address(address: IpAddr) -> IpAddr {
    match address {
        IpAddr::V6(address) => address
            .to_ipv4_mapped()
            .map(IpAddr::V4)
            .unwrap_or(IpAddr::V6(address)),
        address => address,
    }
}

fn classify_ipv4(address: Ipv4Addr) -> PeerNetwork {
    let octets = address.octets();

    if address.is_loopback() {
        return PeerNetwork::Local;
    }

    let is_private_or_link_local = (octets[0] == 192 && octets[1] == 168)
        || (octets[0] == 172 && (16..=31).contains(&octets[1]))
        || octets[0] == 10
        || (octets[0] == 100 && (64..=127).contains(&octets[1]))
        || (octets[0] == 169 && octets[1] == 254);

    if is_private_or_link_local {
        PeerNetwork::Lan
    } else {
        PeerNetwork::Wan
    }
}

fn classify_ipv6(address: Ipv6Addr) -> PeerNetwork {
    let octets = address.octets();

    if address.is_loopback() {
        return PeerNetwork::Local;
    }

    let is_unique_local = (octets[0] & 0xfe) == 0xfc;
    let is_link_local =
        octets[0] == 0xfe && octets[1] == 0x80 && octets[2..8].iter().all(|octet| *octet == 0);

    if is_unique_local || is_link_local {
        PeerNetwork::Lan
    } else {
        PeerNetwork::Wan
    }
}

/// Video encryption behavior selected by a configured policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VideoEncryptionMode {
    /// Do not use video encryption.
    Never,
    /// Use video encryption when both peers support it.
    Opportunistic,
    /// Require video encryption.
    Mandatory,
}

/// A numeric value that is not a supported video encryption mode.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidVideoEncryptionMode {
    /// The unsupported numeric value.
    pub value: i64,
}

impl TryFrom<i64> for VideoEncryptionMode {
    type Error = InvalidVideoEncryptionMode;

    fn try_from(value: i64) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Never),
            1 => Ok(Self::Opportunistic),
            2 => Ok(Self::Mandatory),
            value => Err(InvalidVideoEncryptionMode { value }),
        }
    }
}

impl VideoEncryptionMode {
    /// Returns whether this mode permits video encryption to be negotiated.
    pub const fn supports_video_encryption(self) -> bool {
        !matches!(self, Self::Never)
    }

    /// Returns whether this mode requires video encryption for the session.
    pub const fn requires_video_encryption(self) -> bool {
        matches!(self, Self::Mandatory)
    }

    /// Returns whether this policy accepts the client's announced video-encryption choice.
    ///
    /// `client_announced_video_encryption_choice` is the parsed boolean choice carried by the
    /// client's ANNOUNCE for this session, not a probe of the client's general capabilities.
    pub const fn accepts_client_announced_video_encryption_choice(
        self,
        client_announced_video_encryption_choice: bool,
    ) -> bool {
        match self {
            Self::Never => !client_announced_video_encryption_choice,
            Self::Opportunistic => true,
            Self::Mandatory => client_announced_video_encryption_choice,
        }
    }
}

/// Global LAN and WAN video encryption settings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NetworkEncryptionPolicy {
    /// Mode used for local and LAN peers.
    pub lan_mode: VideoEncryptionMode,
    /// Mode used for WAN peers.
    pub wan_mode: VideoEncryptionMode,
}

impl Default for NetworkEncryptionPolicy {
    fn default() -> Self {
        Self {
            lan_mode: VideoEncryptionMode::Never,
            wan_mode: VideoEncryptionMode::Opportunistic,
        }
    }
}

impl NetworkEncryptionPolicy {
    /// Selects the configured mode for a peer address.
    pub fn mode_for_peer(self, address: IpAddr) -> VideoEncryptionMode {
        match classify_peer(address) {
            PeerNetwork::Local | PeerNetwork::Lan => self.lan_mode,
            PeerNetwork::Wan => self.wan_mode,
        }
    }

    /// Returns whether the peer's announced video-encryption choice is accepted.
    pub fn accepts_video_encryption_for_peer(
        self,
        address: IpAddr,
        announced_video_encryption_choice: bool,
    ) -> bool {
        self.mode_for_peer(address)
            .accepts_client_announced_video_encryption_choice(announced_video_encryption_choice)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        InvalidVideoEncryptionMode, NetworkEncryptionPolicy, PeerNetwork, VideoEncryptionMode,
        classify_peer,
    };
    use std::net::IpAddr;
    use std::str::FromStr;

    fn address(value: &str) -> IpAddr {
        IpAddr::from_str(value).expect("test address should parse")
    }

    fn assert_classification(value: &str, expected: PeerNetwork) {
        assert_eq!(classify_peer(address(value)), expected, "{value}");
    }

    #[test]
    fn classifies_ipv4_loopback_boundaries_as_local() {
        assert_classification("127.0.0.0", PeerNetwork::Local);
        assert_classification("127.255.255.255", PeerNetwork::Local);
        assert_classification("126.255.255.255", PeerNetwork::Wan);
        assert_classification("128.0.0.0", PeerNetwork::Wan);
    }

    #[test]
    fn classifies_ipv6_loopback_as_local() {
        assert_classification("::1", PeerNetwork::Local);
        assert_classification("::2", PeerNetwork::Wan);
    }

    #[test]
    fn classifies_192_168_lan_range_boundaries() {
        assert_classification("192.168.0.0", PeerNetwork::Lan);
        assert_classification("192.168.255.255", PeerNetwork::Lan);
        assert_classification("192.167.255.255", PeerNetwork::Wan);
        assert_classification("192.169.0.0", PeerNetwork::Wan);
    }

    #[test]
    fn classifies_172_16_lan_range_boundaries() {
        assert_classification("172.16.0.0", PeerNetwork::Lan);
        assert_classification("172.31.255.255", PeerNetwork::Lan);
        assert_classification("172.15.255.255", PeerNetwork::Wan);
        assert_classification("172.32.0.0", PeerNetwork::Wan);
    }

    #[test]
    fn classifies_10_lan_range_boundaries() {
        assert_classification("10.0.0.0", PeerNetwork::Lan);
        assert_classification("10.255.255.255", PeerNetwork::Lan);
        assert_classification("9.255.255.255", PeerNetwork::Wan);
        assert_classification("11.0.0.0", PeerNetwork::Wan);
    }

    #[test]
    fn classifies_shared_address_lan_range_boundaries() {
        assert_classification("100.64.0.0", PeerNetwork::Lan);
        assert_classification("100.127.255.255", PeerNetwork::Lan);
        assert_classification("100.63.255.255", PeerNetwork::Wan);
        assert_classification("100.128.0.0", PeerNetwork::Wan);
    }

    #[test]
    fn classifies_ipv4_link_local_boundaries() {
        assert_classification("169.254.0.0", PeerNetwork::Lan);
        assert_classification("169.254.255.255", PeerNetwork::Lan);
        assert_classification("169.253.255.255", PeerNetwork::Wan);
        assert_classification("169.255.0.0", PeerNetwork::Wan);
    }

    #[test]
    fn classifies_ipv6_unique_local_range_boundaries() {
        assert_classification("fc00::", PeerNetwork::Lan);
        assert_classification("fdff:ffff:ffff:ffff:ffff:ffff:ffff:ffff", PeerNetwork::Lan);
        assert_classification("fbff:ffff:ffff:ffff:ffff:ffff:ffff:ffff", PeerNetwork::Wan);
        assert_classification("fe00::", PeerNetwork::Wan);
    }

    #[test]
    fn classifies_ipv6_link_local_64_bit_prefix_boundaries() {
        assert_classification("fe80::", PeerNetwork::Lan);
        assert_classification("fe80:0:0:0:ffff:ffff:ffff:ffff", PeerNetwork::Lan);
        assert_classification("fe7f:ffff:ffff:ffff:ffff:ffff:ffff:ffff", PeerNetwork::Wan);
        assert_classification("fe80:0:0:1::", PeerNetwork::Wan);
    }

    #[test]
    fn normalizes_ipv4_mapped_ipv6_before_classification() {
        assert_classification("::ffff:127.0.0.1", PeerNetwork::Local);
        assert_classification("::ffff:192.168.1.1", PeerNetwork::Lan);
        assert_classification("::ffff:8.8.8.8", PeerNetwork::Wan);
    }

    #[test]
    fn defaults_select_never_for_local_and_lan_and_opportunistic_for_wan() {
        let policy = NetworkEncryptionPolicy::default();

        assert_eq!(policy.lan_mode, VideoEncryptionMode::Never);
        assert_eq!(policy.wan_mode, VideoEncryptionMode::Opportunistic);
        assert_eq!(
            policy.mode_for_peer(address("127.0.0.1")),
            VideoEncryptionMode::Never
        );
        assert_eq!(
            policy.mode_for_peer(address("10.1.2.3")),
            VideoEncryptionMode::Never
        );
        assert_eq!(
            policy.mode_for_peer(address("8.8.8.8")),
            VideoEncryptionMode::Opportunistic
        );
    }

    #[test]
    fn accepts_announced_video_encryption_choices_for_each_peer_network() {
        let policy = NetworkEncryptionPolicy {
            lan_mode: VideoEncryptionMode::Mandatory,
            wan_mode: VideoEncryptionMode::Never,
        };
        let cases = [
            ("127.0.0.1", false, false),
            ("127.0.0.1", true, true),
            ("10.1.2.3", false, false),
            ("10.1.2.3", true, true),
            ("8.8.8.8", false, true),
            ("8.8.8.8", true, false),
            ("2001:4860:4860::8888", false, true),
            ("2001:4860:4860::8888", true, false),
            ("::ffff:8.8.8.8", false, true),
            ("::ffff:8.8.8.8", true, false),
        ];

        for (peer_address, announced_video_encryption_choice, expected) in cases {
            assert_eq!(
                policy.accepts_video_encryption_for_peer(
                    address(peer_address),
                    announced_video_encryption_choice,
                ),
                expected,
                "peer {peer_address}, announced choice {announced_video_encryption_choice}"
            );
        }
    }

    #[test]
    fn converts_only_supported_numeric_modes() {
        assert_eq!(
            VideoEncryptionMode::try_from(0),
            Ok(VideoEncryptionMode::Never)
        );
        assert_eq!(
            VideoEncryptionMode::try_from(1),
            Ok(VideoEncryptionMode::Opportunistic)
        );
        assert_eq!(
            VideoEncryptionMode::try_from(2),
            Ok(VideoEncryptionMode::Mandatory)
        );
        assert_eq!(
            VideoEncryptionMode::try_from(-1),
            Err(InvalidVideoEncryptionMode { value: -1 })
        );
        assert_eq!(
            VideoEncryptionMode::try_from(3),
            Err(InvalidVideoEncryptionMode { value: 3 })
        );
        assert_eq!(
            VideoEncryptionMode::try_from(i64::MAX),
            Err(InvalidVideoEncryptionMode { value: i64::MAX })
        );
    }

    #[test]
    fn exposes_video_encryption_capability_predicates() {
        assert!(!VideoEncryptionMode::Never.supports_video_encryption());
        assert!(!VideoEncryptionMode::Never.requires_video_encryption());
        assert!(VideoEncryptionMode::Opportunistic.supports_video_encryption());
        assert!(!VideoEncryptionMode::Opportunistic.requires_video_encryption());
        assert!(VideoEncryptionMode::Mandatory.supports_video_encryption());
        assert!(VideoEncryptionMode::Mandatory.requires_video_encryption());
    }

    #[test]
    fn accepts_only_announced_video_encryption_choices_allowed_by_policy() {
        let cases = [
            (VideoEncryptionMode::Never, false, true),
            (VideoEncryptionMode::Never, true, false),
            (VideoEncryptionMode::Opportunistic, false, true),
            (VideoEncryptionMode::Opportunistic, true, true),
            (VideoEncryptionMode::Mandatory, false, false),
            (VideoEncryptionMode::Mandatory, true, true),
        ];

        for (mode, client_announced_choice, expected) in cases {
            assert_eq!(
                mode.accepts_client_announced_video_encryption_choice(client_announced_choice),
                expected,
                "mode {mode:?}, client announced choice {client_announced_choice}"
            );
        }
    }
}
