use super::*;

fn fixtures() -> Vec<(&'static str, Vec<u8>)> {
    include_str!("fixtures.txt")
        .lines()
        .map(|line| {
            let (name, hex) = line.split_once(' ').unwrap();
            let bytes = hex
                .as_bytes()
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
                .collect();
            (name, bytes)
        })
        .collect()
}

#[test]
fn pinned_upstream_c_fixtures_decode_semantically_and_serialize_identically() {
    for (name, bytes) in fixtures() {
        let packet = decode(&bytes, &Limits::default()).unwrap();
        assert_eq!(packet.commands.len(), 1);
        assert_eq!(
            encode(&packet, &Limits::default()).unwrap(),
            bytes,
            "{name}"
        );
        let command = &packet.commands[0];
        match (&command.body, name) {
            (CommandBody::Connect(value), "connect") => {
                assert_eq!(
                    packet.header,
                    Header {
                        peer_id: 4095,
                        session_id: 0,
                        sent_time: Some(0xbeef)
                    }
                );
                assert_eq!(
                    (command.channel_id, command.reliable_sequence, command.flags),
                    (255, 1, 128)
                );
                assert_eq!(
                    (
                        value.outgoing_peer_id,
                        value.incoming_session_id,
                        value.outgoing_session_id
                    ),
                    (7, 255, 255)
                );
                assert_eq!(
                    (value.mtu, value.window_size, value.channel_count),
                    (900, 65536, 48)
                );
                assert_eq!(
                    (
                        value.packet_throttle_interval,
                        value.packet_throttle_acceleration,
                        value.packet_throttle_deceleration
                    ),
                    (5000, 2, 2)
                );
                assert_eq!(value.connect_id, [0x12, 0x34, 0x56, 0x78]);
                assert_eq!(value.data, 0x11223344);
            }
            (CommandBody::VerifyConnect(value), "verify") => {
                assert_eq!(packet.header.session_id, 2);
                assert_eq!(
                    (value.incoming_session_id, value.outgoing_session_id),
                    (1, 2)
                );
                assert_eq!(value.connect_id, [0x12, 0x34, 0x56, 0x78]);
            }
            (
                CommandBody::Acknowledge {
                    received_sequence,
                    received_sent_time,
                },
                "ack",
            ) => {
                assert_eq!((*received_sequence, *received_sent_time), (65535, 0xbeef));
                assert_eq!(packet.header.sent_time, None);
            }
            (CommandBody::SendReliable { data }, "reliable") => {
                assert_eq!(data, b"abc");
                assert_eq!(command.reliable_sequence, 65535);
            }
            (CommandBody::SendUnreliable { sequence, data }, "unreliable") => {
                assert_eq!(*sequence, 65535);
                assert_eq!(data, b"xyz");
            }
            (CommandBody::SendUnsequenced { group, data }, "unsequenced") => {
                assert_eq!(*group, 0x1234);
                assert_eq!(data, b"def");
            }
            (CommandBody::SendFragment(value), "fragment") => {
                assert_eq!(
                    (
                        value.start_sequence,
                        value.fragment_count,
                        value.fragment_number,
                        value.total_length,
                        value.fragment_offset
                    ),
                    (65535, 2, 0, 8, 0)
                );
                assert_eq!(value.data, b"abcd");
            }
            (CommandBody::SendUnreliableFragment(value), "unreliable_fragment") => {
                assert_eq!(
                    (
                        value.start_sequence,
                        value.fragment_number,
                        value.fragment_offset
                    ),
                    (99, 1, 4)
                );
                assert_eq!(value.data, b"efgh");
            }
            (CommandBody::Disconnect { data }, "disconnect") => assert_eq!(*data, 0x55667788),
            (CommandBody::Ping, "ping") => {}
            (CommandBody::BandwidthLimit { incoming, outgoing }, "bandwidth") => {
                assert_eq!((*incoming, *outgoing), (12345, 67890))
            }
            (
                CommandBody::ThrottleConfigure {
                    interval,
                    acceleration,
                    deceleration,
                },
                "throttle",
            ) => assert_eq!((*interval, *acceleration, *deceleration), (5000, 2, 2)),
            other => panic!("fixture disagrees with command: {other:?}"),
        }
    }
}

#[test]
fn every_fixture_truncation_and_trailing_garbage_fails_without_partial_acceptance() {
    for (name, bytes) in fixtures() {
        for length in 0..bytes.len() {
            assert!(
                decode(&bytes[..length], &Limits::default()).is_err(),
                "{name} prefix {length}"
            );
        }
        let mut trailing = bytes;
        trailing.push(0);
        assert!(decode(&trailing, &Limits::default()).is_err());
    }
}

fn ping() -> Datagram {
    Datagram {
        header: Header {
            peer_id: 7,
            session_id: 1,
            sent_time: Some(900),
        },
        commands: vec![Command {
            channel_id: 255,
            reliable_sequence: 1,
            flags: FLAG_ACKNOWLEDGE,
            body: CommandBody::Ping,
        }],
    }
}

#[test]
fn command_and_datagram_bounds_limit_allocation_and_encoded_amplification() {
    let mut packet = ping();
    packet.commands = vec![packet.commands[0].clone(); 32];
    let encoded = encode(&packet, &Limits::default()).unwrap();
    assert_eq!(
        decode(&encoded, &Limits::default()).unwrap().commands.len(),
        32
    );
    packet.commands.push(packet.commands[0].clone());
    assert_eq!(
        encode(&packet, &Limits::default()),
        Err(Error::TooManyCommands)
    );
    let mut overflow = encoded;
    overflow.extend_from_slice(&[0x85, 255, 0, 1]);
    assert_eq!(
        decode(&overflow, &Limits::default()),
        Err(Error::TooManyCommands)
    );
    assert_eq!(
        decode(&vec![0; 4097], &Limits::default()),
        Err(Error::DatagramTooLarge)
    );
    let mut large = ping();
    large.commands[0].channel_id = 0;
    large.commands[0].body = CommandBody::SendReliable {
        data: vec![0; 4096],
    };
    assert_eq!(
        encode(&large, &Limits::default()),
        Err(Error::DatagramTooLarge)
    );
    assert_eq!(
        decode(
            &[0x80, 7, 0, 1, 0x86, 0, 0, 1, 255, 255],
            &Limits::default()
        ),
        Err(Error::Truncated)
    );
}

#[test]
fn flags_channels_and_explicit_unsupported_profiles_are_rejected() {
    let limits = Limits::default();
    let mut packet = ping();
    packet.commands[0].flags = FLAG_ACKNOWLEDGE | FLAG_UNSEQUENCED;
    assert_eq!(encode(&packet, &limits), Err(Error::InvalidFlags));
    packet = ping();
    packet.header.sent_time = None;
    assert_eq!(encode(&packet, &limits), Err(Error::InvalidHeader));
    packet = ping();
    packet.commands[0].channel_id = 0;
    assert_eq!(encode(&packet, &limits), Err(Error::InvalidChannel));
    packet = ping();
    packet.header.session_id = 4;
    assert_eq!(encode(&packet, &limits), Err(Error::InvalidHeader));
    assert_eq!(
        decode(&[0x40, 0], &limits),
        Err(Error::UnsupportedCompression)
    );
    let checksummed = Limits {
        checksum: true,
        ..limits
    };
    assert_eq!(
        decode(&[0, 0], &checksummed),
        Err(Error::UnsupportedChecksum)
    );
    assert_eq!(
        encode(&ping(), &checksummed),
        Err(Error::UnsupportedChecksum)
    );
    assert_eq!(
        decode(&[0, 0, 0x35, 255, 0, 1], &limits),
        Err(Error::InvalidFlags)
    );
    assert_eq!(
        decode(&[0, 0, 0x0d, 255, 0, 1], &limits),
        Err(Error::UnknownCommand)
    );
    assert_eq!(decode(&[0, 0], &limits), Err(Error::EmptyDatagram));
}

#[test]
fn fragment_fields_are_bounded_before_engine_reassembly_and_wrap_is_preserved() {
    let bytes = fixtures()
        .into_iter()
        .find(|(name, _)| *name == "fragment")
        .unwrap()
        .1;
    let packet = decode(&bytes, &Limits::default()).unwrap();
    for malicious in [
        Fragment {
            start_sequence: 65535,
            fragment_count: u32::MAX,
            fragment_number: 0,
            total_length: u32::MAX,
            fragment_offset: 0,
            data: vec![1],
        },
        Fragment {
            start_sequence: 65535,
            fragment_count: 2,
            fragment_number: 2,
            total_length: 8,
            fragment_offset: 0,
            data: vec![1],
        },
        Fragment {
            start_sequence: 65535,
            fragment_count: 2,
            fragment_number: 0,
            total_length: 8,
            fragment_offset: u32::MAX,
            data: vec![1],
        },
        Fragment {
            start_sequence: 65535,
            fragment_count: 2,
            fragment_number: 1,
            total_length: 8,
            fragment_offset: 4,
            data: vec![1],
        },
    ] {
        let mut changed = packet.clone();
        changed.commands[0].body = CommandBody::SendFragment(malicious);
        assert_eq!(
            encode(&changed, &Limits::default()),
            Err(Error::InvalidFragment)
        );
    }
    let mut second = packet;
    second.commands[0].reliable_sequence = 0;
    let CommandBody::SendFragment(fragment) = &mut second.commands[0].body else {
        unreachable!()
    };
    fragment.fragment_number = 1;
    fragment.fragment_offset = 4;
    assert_eq!(
        decode(
            &encode(&second, &Limits::default()).unwrap(),
            &Limits::default()
        )
        .unwrap(),
        second
    );
    // Directly mutate the actual wire's total length: no allocation of that size.
    let mut malicious = bytes;
    malicious[20..24].copy_from_slice(&u32::MAX.to_be_bytes());
    assert_eq!(
        decode(&malicious, &Limits::default()),
        Err(Error::InvalidFragment)
    );
}

#[test]
fn invalid_handshake_bounds_and_session_sentinels_are_visible() {
    let bytes = fixtures()
        .into_iter()
        .find(|(name, _)| *name == "connect")
        .unwrap()
        .1;
    for position in [10, 11] {
        let mut invalid = bytes.clone();
        invalid[position] = 4;
        assert_eq!(
            decode(&invalid, &Limits::default()),
            Err(Error::InvalidHandshake)
        );
    }
    let mut invalid = bytes;
    invalid[12..16].copy_from_slice(&4097u32.to_be_bytes());
    assert_eq!(
        decode(&invalid, &Limits::default()),
        Err(Error::InvalidHandshake)
    );
    for limits in [
        Limits {
            mtu: 575,
            ..Limits::default()
        },
        Limits {
            channels: 0,
            ..Limits::default()
        },
        Limits {
            max_fragments: 1025,
            ..Limits::default()
        },
    ] {
        assert_eq!(encode(&ping(), &limits), Err(Error::InvalidLimits));
    }
}

#[test]
#[ignore = "requires separately compiled pinned upstream ENet oracle"]
fn regenerate_fixtures_with_actual_upstream_packed_structs() {
    let path = std::env::var("AMATERASU_ENET_ORACLE").expect("build documented ENet oracle");
    let output = std::process::Command::new(path)
        .arg("fixtures")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        include_str!("fixtures.txt")
    );
}
