use super::{
    tests::{connected, packet, reliable},
    *,
};
use crate::crypto::{
    ControlOpener, ControlOrdering, ControlProfile, ControlSealer, CryptoError, Direction,
};

struct FragmentLayout {
    count: u32,
    total: usize,
    offset: usize,
}

fn fragment(
    channel: u8,
    start: u16,
    number: u32,
    layout: FragmentLayout,
    data: &[u8],
    reliable: bool,
) -> Command {
    let value = Fragment {
        start_sequence: start,
        fragment_count: layout.count,
        fragment_number: number,
        total_length: layout.total as u32,
        fragment_offset: layout.offset as u32,
        data: data.to_vec(),
    };
    Command {
        channel_id: channel,
        reliable_sequence: if reliable {
            start.wrapping_add(number as u16)
        } else {
            0
        },
        flags: if reliable { FLAG_ACKNOWLEDGE } else { 0 },
        body: if reliable {
            CommandBody::SendFragment(value)
        } else {
            CommandBody::SendUnreliableFragment(value)
        },
    }
}

fn received(peer: &mut Peer) -> (u8, Vec<u8>) {
    match peer.pop_event().expect("delivery") {
        Event::Receive { channel, data, .. } => (channel, data),
        other => panic!("unexpected event: {other:?}"),
    }
}

#[test]
fn shared_crypto_replay_window_survives_cross_channel_fragment_reordering() {
    let key = [0x51; 16];
    let mut seal =
        ControlSealer::new(&key, ControlProfile::SunshineV2, Direction::ClientToHost).unwrap();
    let mut open = ControlOpener::new(
        &key,
        ControlProfile::SunshineV2,
        Direction::ClientToHost,
        ControlOrdering::MultipleChannels,
    )
    .unwrap();
    let first = seal.seal(0x301, &[0xa1; 2000]).unwrap();
    let second = seal.seal(0x302, b"independent channel").unwrap();
    let third = seal.seal(0x303, b"same channel successor").unwrap();
    let mut peer = connected();
    let fragments: Vec<_> = first
        .chunks(800)
        .enumerate()
        .map(|(number, bytes)| {
            fragment(
                0,
                1,
                number as u32,
                FragmentLayout {
                    count: 3,
                    total: first.len(),
                    offset: number * 800,
                },
                bytes,
                true,
            )
        })
        .collect();
    peer.receive(&packet(vec![fragments[2].clone()], 2), 2)
        .unwrap();
    peer.receive(&packet(vec![reliable(4, &third)], 3), 3)
        .unwrap();
    assert!(peer.pop_event().is_none());
    let mut independent = reliable(1, &second);
    independent.channel_id = 7;
    peer.receive(&packet(vec![independent], 4), 4).unwrap();
    let (channel, bytes) = received(&mut peer);
    assert_eq!(channel, 7);
    assert_eq!(open.open(&bytes).unwrap().sequence, 1);
    peer.receive(&packet(vec![fragments[0].clone()], 5), 5)
        .unwrap();
    peer.receive(&packet(vec![fragments[1].clone()], 6), 6)
        .unwrap();
    for sequence in [0, 2] {
        let (channel, bytes) = received(&mut peer);
        assert_eq!(channel, 0);
        assert_eq!(open.open(&bytes).unwrap().sequence, sequence);
    }
    let mut replay = reliable(1, &second);
    replay.channel_id = 3;
    peer.receive(&packet(vec![replay], 7), 7).unwrap();
    let (_, bytes) = received(&mut peer);
    assert!(matches!(
        open.open(&bytes),
        Err(CryptoError::ReplayRejected)
    ));
    peer.receive(&packet(vec![fragments[0].clone()], 8), 8)
        .unwrap();
    assert!(peer.pop_event().is_none());
}

#[test]
fn closed_peer_rejects_coalesced_application_data_atomically() {
    let mut peer = connected();
    let disconnect = Command {
        channel_id: CONTROL_CHANNEL,
        reliable_sequence: 2,
        flags: FLAG_ACKNOWLEDGE,
        body: CommandBody::Disconnect { data: 1 },
    };
    assert_eq!(
        peer.receive(&packet(vec![disconnect, reliable(1, b"after close")], 2), 2),
        Err(PeerError::NotConnected)
    );
    assert_eq!(peer.state(), State::Connected);
    assert!(peer.pop_event().is_none());
    assert!(peer.pop_datagram().is_none());
}

#[test]
fn fragment_sequence_collision_and_incomplete_coverage_are_not_acknowledged() {
    let mut peer = connected();
    let last = fragment(
        0,
        1,
        1,
        FragmentLayout {
            count: 2,
            total: 6,
            offset: 3,
        },
        b"def",
        true,
    );
    peer.receive(&packet(vec![last], 2), 2).unwrap();
    peer.pop_datagram().unwrap();
    assert_eq!(
        peer.receive(&packet(vec![reliable(1, b"not the fragment")], 3), 3),
        Err(PeerError::InvalidPacket)
    );
    assert!(peer.pop_datagram().is_none());
    let gap = fragment(
        0,
        1,
        0,
        FragmentLayout {
            count: 2,
            total: 6,
            offset: 0,
        },
        b"ab",
        true,
    );
    assert_eq!(
        peer.receive(&packet(vec![gap], 4), 4),
        Err(PeerError::InvalidPacket)
    );
    assert!(peer.pop_datagram().is_none());
    assert!(peer.pop_event().is_none());
    let first = fragment(
        0,
        1,
        0,
        FragmentLayout {
            count: 2,
            total: 6,
            offset: 0,
        },
        b"abc",
        true,
    );
    peer.receive(&packet(vec![first], 5), 5).unwrap();
    assert_eq!(received(&mut peer).1, b"abcdef");
}

#[test]
fn conflicting_queued_reliable_duplicate_cannot_ack_different_bytes() {
    let mut peer = connected();
    peer.receive(&packet(vec![reliable(2, b"original")], 2), 2)
        .unwrap();
    peer.pop_datagram().unwrap();
    assert_eq!(
        peer.receive(&packet(vec![reliable(2, b"replacement")], 3), 3),
        Err(PeerError::InvalidPacket)
    );
    assert!(peer.pop_datagram().is_none());
    peer.receive(&packet(vec![reliable(2, b"original")], 4), 4)
        .unwrap();
    assert!(peer.pop_datagram().is_some());
}

#[test]
fn full_delivery_queue_rejects_without_ack_then_retry_delivers() {
    let mut peer = connected();
    for sequence in 1..=256 {
        peer.receive(
            &packet(vec![reliable(sequence, b"event")], sequence),
            sequence as u64 + 1,
        )
        .unwrap();
        peer.pop_datagram().unwrap();
    }
    assert_eq!(
        peer.receive(&packet(vec![reliable(257, b"retry")], 258), 258),
        Err(PeerError::Busy)
    );
    assert_eq!(peer.channels[0].received_reliable, 256);
    assert_eq!(peer.events.len(), MAX_EVENTS);
    assert!(peer.pop_datagram().is_none());
    peer.pop_event().unwrap();
    peer.receive(&packet(vec![reliable(257, b"retry")], 259), 259)
        .unwrap();
    assert_eq!(peer.channels[0].received_reliable, 257);
    assert!(peer.pop_datagram().is_some());
}

#[test]
fn hostile_fragment_declarations_hit_group_and_byte_bounds_transactionally() {
    let mut peer = connected();
    for sequence in 1..=128 {
        let command = fragment(
            0,
            sequence,
            0,
            FragmentLayout {
                count: 128,
                total: 128,
                offset: 0,
            },
            &[1],
            false,
        );
        peer.receive(&packet(vec![command], sequence), sequence as u64 + 1)
            .unwrap();
    }
    let overflow = fragment(
        0,
        129,
        0,
        FragmentLayout {
            count: 128,
            total: 128,
            offset: 0,
        },
        &[1],
        false,
    );
    assert_eq!(
        peer.receive(&packet(vec![overflow], 130), 130),
        Err(PeerError::Busy)
    );
    assert_eq!(peer.channels[0].unreliable.len(), 128);
    peer.poll(6000).unwrap();
    assert!(peer.channels[0].unreliable.is_empty());
    let mut peer = connected();
    for sequence in 1..=4 {
        let command = fragment(
            0,
            sequence,
            0,
            FragmentLayout {
                count: 2,
                total: 65500,
                offset: 0,
            },
            &[1],
            false,
        );
        peer.receive(&packet(vec![command], sequence), sequence as u64 + 1)
            .unwrap();
    }
    let overflow = fragment(
        0,
        5,
        0,
        FragmentLayout {
            count: 2,
            total: 65500,
            offset: 0,
        },
        &[1],
        false,
    );
    assert_eq!(
        peer.receive(&packet(vec![overflow], 6), 6),
        Err(PeerError::Busy)
    );
    assert_eq!(peer.channels[0].unreliable.len(), 4);
}

#[test]
fn unreliable_wrap_promotes_reliable_and_large_numeric_gap_is_valid() {
    let mut peer = connected();
    peer.channels[0].sent_unreliable = u16::MAX;
    peer.can_send(0, 3, Delivery::Unreliable).unwrap();
    peer.send(0, b"new", Delivery::Unreliable, 2).unwrap();
    let sent = decode(&peer.pop_datagram().unwrap(), &Limits::default()).unwrap();
    assert!(matches!(
        sent.commands[0].body,
        CommandBody::SendReliable { .. }
    ));
    assert_eq!(peer.channels[0].sent_unreliable, 0);
    let unreliable = Command {
        channel_id: 0,
        reliable_sequence: 0,
        flags: 0,
        body: CommandBody::SendUnreliable {
            sequence: 50000,
            data: vec![1],
        },
    };
    peer.receive(&packet(vec![unreliable], 3), 3).unwrap();
    assert_eq!(received(&mut peer).1, [1]);
    peer.send(0, b"free", Delivery::Unsequenced, 4).unwrap();
    assert_eq!(peer.channels[0].sent_unreliable, 0);
}

#[test]
fn bounded_retry_history_and_spoofed_timestamp_do_not_drop_reliable() {
    let mut peer = connected();
    peer.config.idle_timeout_ms = 120000;
    peer.config.ping_interval_ms = 100000;
    peer.send(0, b"must retry", Delivery::Reliable, 2).unwrap();
    peer.pop_datagram().unwrap();
    let false_ack = Command {
        channel_id: 0,
        reliable_sequence: 1,
        flags: 0,
        body: CommandBody::Acknowledge {
            received_sequence: 1,
            received_sent_time: 999,
        },
    };
    peer.receive(&packet(vec![false_ack], 3), 3).unwrap();
    assert_eq!(peer.pending.len(), 1);
    for attempt in 1..16 {
        peer.poll(2 + attempt * 2000).unwrap();
        let bytes = peer.pop_datagram().unwrap();
        assert_eq!(
            decode(&bytes, &Limits::default()).unwrap().commands[0],
            reliable(1, b"must retry")
        );
        assert!(peer.pending[0].sent_times.len() <= 16);
    }
    peer.poll(32002).unwrap();
    assert_eq!(peer.state(), State::TimedOut);
    assert!(peer.pending.is_empty());
    assert!(peer.pop_datagram().is_none());
}

#[test]
fn retransmission_budget_rotates_when_polling_is_slower_than_retry_interval() {
    let mut peer = connected();
    for _ in 0..128 {
        peer.send(0, b"x", Delivery::Reliable, 2).unwrap();
    }
    while peer.pop_datagram().is_some() {}
    for (now, expected_start) in [(2002, 1), (4002, 17), (6002, 33)] {
        peer.poll(now).unwrap();
        let sequences: Vec<_> = std::iter::from_fn(|| peer.pop_datagram())
            .map(|bytes| decode(&bytes, &Limits::default()).unwrap().commands[0].reliable_sequence)
            .collect();
        assert_eq!(
            sequences,
            (expected_start..expected_start + 16).collect::<Vec<_>>()
        );
    }
}
