use super::*;

#[test]
fn default_ports_match_the_public_gamestream_services() {
    let ports = GameStreamPorts::new(DEFAULT_BASE_PORT).unwrap();
    assert_eq!(ports.base(), 47_989);
    assert_eq!(ports.http(), 47_989);
    assert_eq!(ports.https(), 47_984);
    assert_eq!(ports.rtsp(), 48_010);
    assert_eq!(ports.video(), 47_998);
    assert_eq!(ports.control(), 47_999);
    assert_eq!(ports.audio(), 48_000);
}

#[test]
fn every_accepted_base_stays_in_the_unprivileged_u16_port_range() {
    for base in 0..=u16::MAX {
        let result = GameStreamPorts::new(base);
        if !(MIN_BASE_PORT..=MAX_BASE_PORT).contains(&base) {
            assert_eq!(result, Err("invalid_port"));
            continue;
        }
        let ports = result.unwrap();
        let mapped = [
            ports.http(),
            ports.https(),
            ports.rtsp(),
            ports.video(),
            ports.control(),
            ports.audio(),
        ];
        assert!(mapped.into_iter().all(|port| port >= 1024));
        for (index, port) in mapped.iter().enumerate() {
            assert!(!mapped[..index].contains(port));
        }
    }
    assert_eq!(GameStreamPorts::new(MIN_BASE_PORT).unwrap().https(), 1024);
    assert_eq!(
        GameStreamPorts::new(MAX_BASE_PORT).unwrap().rtsp(),
        u16::MAX
    );
}
