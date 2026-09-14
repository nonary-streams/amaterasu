use super::*;

fn connect_packet() -> Vec<u8> {
    encode(
        &Datagram {
            header: Header {
                peer_id: UNCONNECTED_PEER,
                session_id: 0,
                sent_time: Some(0),
            },
            commands: vec![Command {
                channel_id: CONTROL_CHANNEL,
                reliable_sequence: 1,
                flags: FLAG_ACKNOWLEDGE,
                body: CommandBody::Connect(Connect {
                    outgoing_peer_id: 0,
                    incoming_session_id: 255,
                    outgoing_session_id: 255,
                    mtu: 900,
                    window_size: 65536,
                    channel_count: 48,
                    incoming_bandwidth: 0,
                    outgoing_bandwidth: 0,
                    packet_throttle_interval: 5000,
                    packet_throttle_acceleration: 2,
                    packet_throttle_deceleration: 2,
                    connect_id: [1, 2, 3, 4],
                    data: 0x11223344,
                }),
            }],
        },
        &Limits::default(),
    )
    .unwrap()
}

pub(super) fn packet(commands: Vec<Command>, time: u16) -> Vec<u8> {
    encode(
        &Datagram {
            header: Header {
                peer_id: 0,
                session_id: 0,
                sent_time: Some(time),
            },
            commands,
        },
        &Limits::default(),
    )
    .unwrap()
}

pub(super) fn reliable(sequence: u16, bytes: &[u8]) -> Command {
    Command {
        channel_id: 0,
        reliable_sequence: sequence,
        flags: FLAG_ACKNOWLEDGE,
        body: CommandBody::SendReliable {
            data: bytes.to_vec(),
        },
    }
}

pub(super) fn connected() -> Peer {
    let mut peer = Peer::new(
        Config {
            expected_connect_data: 0x11223344,
            ..Config::default()
        },
        0,
    )
    .unwrap();
    assert!(peer.receive(&connect_packet(), 0).unwrap().associated);
    peer.pop_datagram().unwrap();
    peer.receive(
        &packet(
            vec![Command {
                channel_id: CONTROL_CHANNEL,
                reliable_sequence: 1,
                flags: 0,
                body: CommandBody::Acknowledge {
                    received_sequence: 1,
                    received_sent_time: 0,
                },
            }],
            1,
        ),
        1,
    )
    .unwrap();
    assert_eq!(
        peer.pop_event(),
        Some(Event::Connected {
            channels: 48,
            mtu: 900
        })
    );
    peer
}

#[test]
fn association_requires_expected_hint_and_verified_ack() {
    let mut peer = Peer::new(Config::default(), 0).unwrap();
    assert!(!peer.receive(&connect_packet(), 1).unwrap().associated);
    assert!(peer.pop_datagram().is_none());
    assert_eq!(peer.state(), State::Listening);
    assert_eq!(connected().state(), State::Connected);
}

#[test]
fn reliable_reordering_duplicates_and_channel_independence() {
    let mut peer = connected();
    peer.receive(&packet(vec![reliable(2, b"two")], 2), 2)
        .unwrap();
    assert!(peer.pop_event().is_none());
    peer.receive(&packet(vec![reliable(1, b"one")], 3), 3)
        .unwrap();
    for bytes in [b"one", b"two"] {
        assert_eq!(
            peer.pop_event(),
            Some(Event::Receive {
                channel: 0,
                delivery: Delivery::Reliable,
                data: bytes.to_vec()
            })
        );
    }
    peer.receive(&packet(vec![reliable(1, b"one")], 4), 4)
        .unwrap();
    assert!(peer.pop_event().is_none());
    let mut other = reliable(1, b"channel");
    other.channel_id = 7;
    peer.receive(&packet(vec![other], 5), 5).unwrap();
    assert!(matches!(
        peer.pop_event(),
        Some(Event::Receive { channel: 7, .. })
    ));
}

#[test]
fn retransmission_is_exact_command_and_timeout_is_bounded() {
    let mut peer = connected();
    peer.send(0, b"message", Delivery::Reliable, 2).unwrap();
    let first = decode(&peer.pop_datagram().unwrap(), &Limits::default()).unwrap();
    peer.poll(252).unwrap();
    let retry = decode(&peer.pop_datagram().unwrap(), &Limits::default()).unwrap();
    assert_eq!(first.commands, retry.commands);
    assert_ne!(first.header.sent_time, retry.header.sent_time);
    peer.poll(16000).unwrap();
    assert_eq!(peer.state(), State::TimedOut);
    assert_eq!(peer.pop_event(), Some(Event::TimedOut));
    assert!(peer.pop_datagram().is_none());
}

#[test]
fn busy_admission_preserves_outgoing_sequences() {
    let mut peer = connected();
    for _ in 0..128 {
        peer.send(0, b"x", Delivery::Reliable, 2).unwrap();
    }
    assert_eq!(
        peer.can_send(0, 1, Delivery::Reliable),
        Err(PeerError::Busy)
    );
    let sequence = peer.channels[0].sent_reliable;
    assert_eq!(
        peer.send(0, b"x", Delivery::Reliable, 3),
        Err(PeerError::Busy)
    );
    assert_eq!(peer.channels[0].sent_reliable, sequence);
}

#[test]
fn malformed_tail_does_not_commit_valid_prefix() {
    let mut peer = connected();
    let invalid = Command {
        channel_id: CONTROL_CHANNEL,
        reliable_sequence: 2,
        flags: FLAG_ACKNOWLEDGE,
        body: CommandBody::VerifyConnect(VerifyConnect {
            outgoing_peer_id: 0,
            incoming_session_id: 0,
            outgoing_session_id: 0,
            mtu: 900,
            window_size: 65536,
            channel_count: 48,
            incoming_bandwidth: 0,
            outgoing_bandwidth: 0,
            packet_throttle_interval: 5000,
            packet_throttle_acceleration: 2,
            packet_throttle_deceleration: 2,
            connect_id: [1; 4],
        }),
    };
    assert_eq!(
        peer.receive(&packet(vec![reliable(1, b"prefix"), invalid], 2), 2),
        Err(PeerError::InvalidPacket)
    );
    assert!(peer.pop_event().is_none());
    assert!(peer.pop_datagram().is_none());
    assert_eq!(peer.channels[0].received_reliable, 0);
}

#[test]
fn fragment_overlap_rejected_and_reverse_order_reassembled() {
    let mut peer = connected();
    let fragment = |number, offset, data: &[u8]| Command {
        channel_id: 0,
        reliable_sequence: 1 + number as u16,
        flags: FLAG_ACKNOWLEDGE,
        body: CommandBody::SendFragment(Fragment {
            start_sequence: 1,
            fragment_count: 2,
            fragment_number: number,
            total_length: 6,
            fragment_offset: offset,
            data: data.to_vec(),
        }),
    };
    peer.receive(&packet(vec![fragment(1, 3, b"def")], 2), 2)
        .unwrap();
    assert_eq!(
        peer.receive(&packet(vec![fragment(0, 0, b"abcd")], 3), 3),
        Err(PeerError::InvalidPacket)
    );
    peer.receive(&packet(vec![fragment(0, 0, b"abc")], 4), 4)
        .unwrap();
    assert_eq!(
        peer.pop_event(),
        Some(Event::Receive {
            channel: 0,
            delivery: Delivery::Reliable,
            data: b"abcdef".to_vec()
        })
    );
    assert_eq!(peer.channels[0].received_reliable, 2);
}

#[test]
fn unreliable_waits_for_baseline_and_unsequenced_deduplicates() {
    let mut peer = connected();
    let unreliable = Command {
        channel_id: 0,
        reliable_sequence: 1,
        flags: 0,
        body: CommandBody::SendUnreliable {
            sequence: 1,
            data: b"later".to_vec(),
        },
    };
    peer.receive(&packet(vec![unreliable], 2), 2).unwrap();
    assert!(peer.pop_event().is_none());
    peer.receive(&packet(vec![reliable(1, b"first")], 3), 3)
        .unwrap();
    assert!(matches!(
        peer.pop_event(),
        Some(Event::Receive {
            delivery: Delivery::Reliable,
            ..
        })
    ));
    assert!(matches!(
        peer.pop_event(),
        Some(Event::Receive {
            delivery: Delivery::Unreliable,
            ..
        })
    ));
    let unsequenced = Command {
        channel_id: 0,
        reliable_sequence: 0,
        flags: FLAG_UNSEQUENCED,
        body: CommandBody::SendUnsequenced {
            group: 1,
            data: vec![1],
        },
    };
    for now in 4..6 {
        peer.receive(&packet(vec![unsequenced.clone()], now as u16), now)
            .unwrap();
    }
    assert!(peer.pop_event().is_some());
    assert!(peer.pop_event().is_none());
}

#[test]
#[ignore = "requires pinned C ENet oracle and loopback socket permission"]
fn pinned_c_client_handshake_fragment_echo_and_disconnect() {
    use std::net::UdpSocket;
    use std::time::{Duration, Instant};
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    socket.set_nonblocking(true).unwrap();
    let oracle =
        std::env::var("AMATERASU_ENET_ORACLE").expect("build the pinned ENet oracle first");
    let mut child = std::process::Command::new(oracle)
        .args(["client", &socket.local_addr().unwrap().port().to_string()])
        .spawn()
        .unwrap();
    let mut peer = Peer::new(
        Config {
            expected_connect_data: 0x11223344,
            ..Config::default()
        },
        0,
    )
    .unwrap();
    let start = Instant::now();
    let mut remote = None;
    let mut delivered = 0;
    let mut buffer = [0; 4096];
    loop {
        let now = start.elapsed().as_millis() as u64;
        assert!(now < 6000, "oracle timeout");
        if let Ok((len, address)) = socket.recv_from(&mut buffer) {
            assert!(remote.is_none_or(|expected| expected == address));
            if peer.receive(&buffer[..len], now).unwrap().associated {
                remote = Some(address);
            }
        }
        while let Some(event) = peer.pop_event() {
            match event {
                Event::Receive { channel, data, .. } => {
                    peer.send(channel, &data, Delivery::Reliable, now).unwrap();
                    delivered += 1;
                }
                Event::Disconnected { data } => assert_eq!(data, 0x55667788),
                _ => {}
            }
        }
        peer.poll(now).unwrap();
        while let Some(bytes) = peer.pop_datagram() {
            socket.send_to(&bytes, remote.unwrap()).unwrap();
        }
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(delivered, 2);
    assert_eq!(peer.state(), State::Disconnected);
}

#[test]
fn negotiated_reliable_window_defers_fragments_until_ack() {
    let mut peer = connected();
    peer.connect.as_mut().unwrap().window_size = 4096;
    peer.send(0, &[7; 6000], Delivery::Reliable, 2).unwrap();
    let mut first = Vec::new();
    while let Some(bytes) = peer.pop_datagram() {
        first.push(decode(&bytes, &Limits::default()).unwrap());
    }
    assert_eq!(first.len(), 4);
    assert_eq!(peer.pending.iter().filter(|p| p.attempts == 0).count(), 3);
    let acks = first
        .iter()
        .map(|packet| Command {
            channel_id: 0,
            reliable_sequence: packet.commands[0].reliable_sequence,
            flags: 0,
            body: CommandBody::Acknowledge {
                received_sequence: packet.commands[0].reliable_sequence,
                received_sent_time: packet.header.sent_time.unwrap(),
            },
        })
        .collect();
    peer.receive(&packet(acks, 3), 3).unwrap();
    peer.poll(3).unwrap();
    assert_eq!(peer.pending.iter().filter(|p| p.attempts == 0).count(), 0);
    assert_eq!(std::iter::from_fn(|| peer.pop_datagram()).count(), 3);
}

#[test]
fn incomplete_reliable_gap_times_out_even_with_peer_pings() {
    let mut peer = connected();
    peer.receive(&packet(vec![reliable(2, b"gap")], 2), 2)
        .unwrap();
    let ping = Command {
        channel_id: CONTROL_CHANNEL,
        reliable_sequence: 2,
        flags: FLAG_ACKNOWLEDGE,
        body: CommandBody::Ping,
    };
    peer.receive(&packet(vec![ping], 4999), 4999).unwrap();
    peer.poll(5002).unwrap();
    assert_eq!(peer.state(), State::TimedOut);
}

#[test]
fn wrong_session_and_backward_clock_do_not_mutate_or_ack() {
    let mut peer = connected();
    let mut bytes = packet(vec![reliable(1, b"bad")], 2);
    bytes[0] |= 0x10;
    assert!(!peer.receive(&bytes, 2).unwrap().associated);
    assert!(peer.pop_datagram().is_none());
    assert_eq!(
        peer.receive(&packet(vec![reliable(1, b"bad")], 0), 0),
        Err(PeerError::ClockRegression)
    );
    assert_eq!(peer.channels[0].received_reliable, 0);
}

#[test]
fn lost_disconnect_ack_can_be_repeated_without_duplicate_event() {
    let mut peer = connected();
    let disconnect = Command {
        channel_id: CONTROL_CHANNEL,
        reliable_sequence: 2,
        flags: FLAG_ACKNOWLEDGE,
        body: CommandBody::Disconnect { data: 42 },
    };
    for now in 2..4 {
        peer.receive(&packet(vec![disconnect.clone()], now as u16), now)
            .unwrap();
    }
    assert_eq!(peer.pop_event(), Some(Event::Disconnected { data: 42 }));
    assert!(peer.pop_event().is_none());
    assert_eq!(std::iter::from_fn(|| peer.pop_datagram()).count(), 2);
}
