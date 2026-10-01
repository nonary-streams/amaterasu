//! LAN option and loopback default tests for the Linux session profile.
use super::super::options::{
    AddressFamily, DEFAULT_BASE_PORT, ListenerScope, MAX_BASE_PORT, MIN_BASE_PORT, NetworkOption,
    OptionError,
};
use super::*;
use std::collections::BTreeSet;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

fn nvhttp_settings_of(settings: &BTreeMap<String, Value>) -> amaterasu_nvhttp::Settings {
    serde_json::from_value(settings["amaterasu.nvhttp"].clone())
        .expect("NVHTTP parses the generated settings strictly")
}

fn listener_ips(addresses: &[SocketAddr]) -> BTreeSet<IpAddr> {
    addresses.iter().map(SocketAddr::ip).collect()
}

#[test]
fn every_listener_binds_loopback_by_default() {
    let settings = default_policy("loopback").settings;
    let nvhttp = nvhttp_settings_of(&settings);
    nvhttp.validate().unwrap();
    assert!(nvhttp.enabled && !nvhttp.allow_remote);
    let http = nvhttp.resolved_http_binds().unwrap();
    let https = nvhttp.resolved_https_binds().unwrap();
    assert_eq!(
        http,
        vec![SocketAddr::from(([127, 0, 0, 1], DEFAULT_BASE_PORT))]
    );
    assert_eq!(
        https,
        vec![SocketAddr::from(([127, 0, 0, 1], DEFAULT_BASE_PORT - 5))]
    );
    assert_eq!(
        settings["amaterasu.rtsp"]["bind"],
        format!("127.0.0.1:{}", DEFAULT_BASE_PORT + 21)
    );
    for component in [
        "amaterasu.nvhttp",
        "amaterasu.rtsp",
        "amaterasu.stream-session",
        "amaterasu.media-transport",
        "amaterasu.control-transport",
    ] {
        assert_eq!(settings[component]["allow_remote"], false, "{component}");
    }
}

fn network_for(arguments_: &[&str]) -> (NetworkOption, BTreeMap<String, Value>) {
    let options = LinuxSessionOptions::from_arguments(&arguments(arguments_)).unwrap();
    let root = TemporaryRoot::new("network");
    let settings = linux_session_policy(root.path(), &options)
        .unwrap()
        .settings;
    (options.network, settings)
}

fn assert_consistent(network: &NetworkOption, settings: &BTreeMap<String, Value>) {
    let nvhttp = nvhttp_settings_of(settings);
    nvhttp.validate().unwrap();
    let http = nvhttp.resolved_http_binds().unwrap();
    let https = nvhttp.resolved_https_binds().unwrap();
    assert!(http.iter().all(|bind| bind.port() == network.http_port()));
    assert!(
        https
            .iter()
            .all(|bind| bind.port() == network.http_port() - 5)
    );
    assert_eq!(network.https_port(), network.http_port() - 5);
    assert_eq!(network.rtsp_port(), network.http_port() + 21);
    assert_eq!(listener_ips(&http), listener_ips(&https));

    let rtsp = &settings["amaterasu.rtsp"];
    assert_eq!(
        rtsp["bind"]
            .as_str()
            .unwrap()
            .parse::<SocketAddr>()
            .unwrap()
            .port(),
        network.rtsp_port()
    );
    assert_eq!(rtsp["allow_remote"], nvhttp.allow_remote);
    assert_eq!(
        rtsp["address_family"],
        settings["amaterasu.nvhttp"]["address_family"]
    );
    assert_eq!(
        rtsp["bind_address"],
        settings["amaterasu.nvhttp"]["bind_address"]
    );
    for component in [
        "amaterasu.stream-session",
        "amaterasu.media-transport",
        "amaterasu.control-transport",
    ] {
        assert_eq!(
            settings[component]["allow_remote"], nvhttp.allow_remote,
            "{component}"
        );
    }
}

#[test]
fn a_concrete_lan_address_configures_every_listener_consistently() {
    let (network, settings) = network_for(&["--lan", "192.168.1.20:50000"]);
    assert_eq!(
        network.scope(),
        ListenerScope::Interface(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 20)))
    );
    assert_consistent(&network, &settings);
    let nvhttp = nvhttp_settings_of(&settings);
    assert!(nvhttp.allow_remote);
    assert_eq!(
        nvhttp.resolved_http_binds().unwrap(),
        vec![SocketAddr::from(([192, 168, 1, 20], 50_000))]
    );
    assert_eq!(
        nvhttp.resolved_https_binds().unwrap(),
        vec![SocketAddr::from(([192, 168, 1, 20], 49_995))]
    );
    assert_eq!(settings["amaterasu.rtsp"]["bind"], "192.168.1.20:50021");
    assert_eq!(settings["amaterasu.rtsp"]["bind_address"], "192.168.1.20");
    assert_eq!(settings["amaterasu.rtsp"]["address_family"], "ipv4");
    // The LAN option never turns on an effect.
    assert_eq!(settings[VIDEO_COMPONENT_ID]["enabled"], false);
    assert_eq!(settings["amaterasu.input"]["allow_injection"], false);
}

#[test]
fn an_ipv6_lan_address_uses_the_dual_family_listener_setting() {
    let (network, settings) = network_for(&["--lan", "[fd00::20]:47989"]);
    assert_eq!(network.address_family(), AddressFamily::Both);
    assert_consistent(&network, &settings);
    let nvhttp = nvhttp_settings_of(&settings);
    assert_eq!(
        nvhttp.resolved_http_binds().unwrap(),
        vec![SocketAddr::new(
            IpAddr::V6("fd00::20".parse().unwrap()),
            47_989
        )]
    );
    assert_eq!(settings["amaterasu.rtsp"]["bind"], "[fd00::20]:48010");
}

#[test]
fn an_explicit_loopback_address_only_moves_the_ports() {
    let (network, settings) = network_for(&["--lan", "127.0.0.1:52000"]);
    assert!(!network.allow_remote());
    assert_consistent(&network, &settings);
    assert_eq!(settings["amaterasu.nvhttp"]["allow_remote"], false);
    let (network, settings) = network_for(&["--lan", "[::1]:52000"]);
    assert!(!network.allow_remote());
    assert_consistent(&network, &settings);
}

#[test]
fn wildcard_listening_needs_an_explicit_allow_remote_request() {
    for wildcard in ["0.0.0.0:47989", "[::]:47989"] {
        let rejected = LinuxSessionOptions::from_arguments(&arguments(&["--lan", wildcard]));
        assert_eq!(
            rejected,
            Err(OptionError::WildcardRequiresAllowRemote),
            "{wildcard}"
        );
    }
    let (network, settings) = network_for(&["--lan", "0.0.0.0:47989", "--allow-remote"]);
    assert_eq!(
        network.scope(),
        ListenerScope::AllInterfaces(AddressFamily::Ipv4)
    );
    assert_consistent(&network, &settings);
    assert_eq!(
        nvhttp_settings_of(&settings).resolved_http_binds().unwrap(),
        vec![SocketAddr::from(([0, 0, 0, 0], 47_989))]
    );
    assert_eq!(settings["amaterasu.rtsp"]["bind_address"], "");

    let (network, settings) = network_for(&["--allow-remote", "--lan", "[::]:47989"]);
    assert_eq!(
        network.scope(),
        ListenerScope::AllInterfaces(AddressFamily::Both)
    );
    assert_consistent(&network, &settings);
    assert_eq!(
        listener_ips(&nvhttp_settings_of(&settings).resolved_http_binds().unwrap()),
        BTreeSet::from([
            IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            IpAddr::V6(Ipv6Addr::UNSPECIFIED)
        ])
    );
}

#[test]
fn allow_remote_alone_or_with_a_concrete_address_is_rejected() {
    assert_eq!(
        LinuxSessionOptions::from_arguments(&arguments(&["--allow-remote"])),
        Err(OptionError::AllowRemoteWithoutWildcard)
    );
    for concrete in ["192.168.1.20:47989", "127.0.0.1:47989"] {
        assert_eq!(
            LinuxSessionOptions::from_arguments(&arguments(&["--lan", concrete, "--allow-remote"])),
            Err(OptionError::AllowRemoteWithoutWildcard),
            "{concrete}"
        );
    }
}

#[test]
fn base_ports_follow_the_nvhttp_and_rtsp_bounds() {
    let loopback = IpAddr::V4(Ipv4Addr::LOCALHOST);
    for port in [0, 1, 1_028, MAX_BASE_PORT + 1, u16::MAX] {
        assert_eq!(
            NetworkOption::new(loopback, port, false),
            Err(OptionError::PortOutOfRange(port))
        );
    }
    let lowest = NetworkOption::new(loopback, MIN_BASE_PORT, false).unwrap();
    assert_eq!(lowest.https_port(), 1_024);
    assert_eq!(lowest.rtsp_port(), 1_050);
    let highest = NetworkOption::new(loopback, MAX_BASE_PORT, false).unwrap();
    assert_eq!(highest.https_port(), 65_509);
    assert_eq!(highest.rtsp_port(), u16::MAX);

    for (network, label) in [(lowest, "lowest"), (highest, "highest")] {
        let options = LinuxSessionOptions {
            network,
            ..LinuxSessionOptions::default()
        };
        let root = TemporaryRoot::new(label);
        let settings = linux_session_policy(root.path(), &options)
            .unwrap()
            .settings;
        assert_consistent(&network, &settings);
    }
}

#[test]
fn unusable_listener_addresses_are_rejected() {
    for address in [
        IpAddr::V4(Ipv4Addr::new(224, 0, 0, 251)),
        IpAddr::V4(Ipv4Addr::BROADCAST),
        IpAddr::V6("ff02::1".parse().unwrap()),
        IpAddr::V6("::ffff:192.168.1.20".parse().unwrap()),
    ] {
        assert_eq!(
            NetworkOption::new(address, 47_989, false),
            Err(OptionError::UnusableBindAddress(address)),
            "{address}"
        );
    }
}

#[test]
fn malformed_options_are_rejected_with_the_option_named() {
    for (input, expected) in [
        (vec!["--lan"], OptionError::MissingValue("--lan")),
        (
            vec!["--lan", "192.168.1.20"],
            OptionError::InvalidLanValue("192.168.1.20".into()),
        ),
        (
            vec!["--lan", "lan.example:47989"],
            OptionError::InvalidLanValue("lan.example:47989".into()),
        ),
        (
            vec!["--lan", "127.0.0.1:47989", "--lan", "127.0.0.1:50000"],
            OptionError::RepeatedOption("--lan"),
        ),
        (
            vec!["--enable-input", "--enable-input"],
            OptionError::RepeatedOption("--enable-input"),
        ),
        (
            vec!["--enable-everything"],
            OptionError::UnknownOption("--enable-everything".into()),
        ),
        (
            vec!["--management-port", "80"],
            OptionError::InvalidManagementPort("80".into()),
        ),
        (
            vec!["--management-port", "not-a-port"],
            OptionError::InvalidManagementPort("not-a-port".into()),
        ),
        (
            vec!["--catalog-owner", "steam"],
            OptionError::InvalidCatalogOwner("steam".into()),
        ),
        (
            vec!["--capture-output", " padded "],
            OptionError::InvalidLabel("--capture-output"),
        ),
        (
            vec!["--audio-endpoint", ""],
            OptionError::InvalidLabel("--audio-endpoint"),
        ),
        (
            vec!["--audio-endpoint", "bad\nname"],
            OptionError::InvalidLabel("--audio-endpoint"),
        ),
    ] {
        assert_eq!(
            LinuxSessionOptions::from_arguments(&arguments(&input)),
            Err(expected),
            "{input:?}"
        );
    }
    let valid = LinuxSessionOptions::from_arguments(&arguments(&[
        "--capture-output",
        "kms:card1/conn=3/crtc=4/plane=5",
        "--audio-endpoint",
        "alsa_output.example",
        "--management-port",
        "47990",
    ]))
    .unwrap();
    assert_eq!(valid.capture_output, "kms:card1/conn=3/crtc=4/plane=5");
    assert_eq!(valid.audio_endpoint, "alsa_output.example");
    assert_eq!(valid.management_port, Some(47_990));
    assert_eq!(
        LinuxSessionOptions::from_arguments(&[]),
        Ok(LinuxSessionOptions::default())
    );
}

#[test]
fn error_messages_tell_the_operator_what_to_do() {
    let message = OptionError::WildcardRequiresAllowRemote.to_string();
    assert!(message.contains("--allow-remote") && message.contains("concrete"));
    let message = OptionError::PortOutOfRange(80).to_string();
    assert!(message.contains("1029") && message.contains("65514"));
}
