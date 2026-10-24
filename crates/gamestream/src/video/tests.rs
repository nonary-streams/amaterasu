use super::*;

fn packetizer(packet_size: usize) -> VideoPacketizer {
    VideoPacketizer::new(VideoConfig {
        packet_size,
        fec_percentage: 0,
        min_parity_shards: 0,
        max_fec_blocks: 1,
        encrypted: false,
    })
    .unwrap()
}

fn hevc_packetizer(packet_size: usize, fec_percentage: u8) -> VideoPacketizer {
    VideoPacketizer::new_with_codec(
        VideoConfig {
            packet_size,
            fec_percentage,
            min_parity_shards: if fec_percentage == 0 { 0 } else { 2 },
            max_fec_blocks: 4,
            encrypted: false,
        },
        VideoCodec::Hevc,
    )
    .unwrap()
}

fn hevc_nal(kind: u8) -> [u8; 6] {
    [0, 0, 0, 1, kind << 1, 1]
}

#[test]
fn hevc_closed_gop_recovery_keeps_parameter_sets_and_frame_type() {
    for idr_kind in [19, 20] {
        let access_unit: Vec<_> = [32, 33, 34, idr_kind]
            .into_iter()
            .flat_map(hevc_nal)
            .collect();
        for fec in [0, 20] {
            let packets = hevc_packetizer(1392, fec)
                .packetize(&access_unit, 1, 90000)
                .unwrap();
            assert_eq!(packets[0][35], 2);
            if fec == 0 {
                assert_eq!(reassemble(&packets, 1392), access_unit);
            }
        }
    }
    let ordinary = hevc_nal(1);
    let packets = hevc_packetizer(1392, 0)
        .packetize(&ordinary, 2, 93000)
        .unwrap();
    assert_eq!(packets[0][35], 1);
}

#[test]
fn decoder_valid_x265_access_unit_survives_wire_packetization() {
    // One synthetic 64x64 blue frame, generated with FFmpeg/libx265 4.3.
    // An independent FFmpeg HEVC decode succeeded before this fixture was
    // committed. This qualifies Annex B framing, not the eventual Main-only
    // encoder capability or a Moonlight session.
    let access_unit = include_bytes!("fixtures/x265_blue_64.hevc");
    for fec in [0, 20] {
        let packets = hevc_packetizer(1392, fec)
            .packetize(access_unit, 1, 90_000)
            .unwrap();
        assert_eq!(packets[0][35], 2);
        if fec == 0 {
            assert_eq!(reassemble(&packets, 1392), access_unit);
        }
    }
}

#[test]
fn hevc_invalid_headers_and_open_gop_do_not_consume_sequence() {
    let valid: Vec<_> = [32, 33, 34, 19].into_iter().flat_map(hevc_nal).collect();
    let mut sender = hevc_packetizer(1392, 0);
    let mut invalid = vec![
        hevc_nal(21).to_vec(), // CRA needs leading-picture recovery semantics.
        hevc_nal(19).to_vec(), // IDR without ordered parameter sets.
        [hevc_nal(32), hevc_nal(34), hevc_nal(33), hevc_nal(19)].concat(),
        [hevc_nal(32), hevc_nal(33), hevc_nal(34)].concat(),
        [hevc_nal(32), hevc_nal(1)].concat(),
        [hevc_nal(1), hevc_nal(19)].concat(),
        [hevc_nal(0), hevc_nal(1)].concat(),
        [
            hevc_nal(32),
            hevc_nal(33),
            hevc_nal(34),
            hevc_nal(19),
            hevc_nal(20),
        ]
        .concat(),
        vec![0, 0, 1, 19 << 1], // Truncated two-byte header.
    ];
    for header in [
        [0x80 | (19 << 1), 1],
        [(19 << 1) | 1, 1],
        [19 << 1, 0],
        [19 << 1, 0x09],
    ] {
        let mut malformed = valid.clone();
        let last = malformed.len() - 2;
        malformed[last..].copy_from_slice(&header);
        invalid.push(malformed);
    }
    for access_unit in invalid {
        assert_eq!(
            sender.packetize(&access_unit, 1, 0),
            Err(VideoError::InvalidAnnexB)
        );
        assert_eq!(sender.stream_index, 0);
    }
}

// Receive-side model of the documented Moonlight layout. This deliberately uses
// literal wire offsets and decoding operations, not the packetizer's helpers.
fn reassemble(packets: &[Vec<u8>], negotiated_size: usize) -> Vec<u8> {
    let first_sequence = u16::from_be_bytes([packets[0][2], packets[0][3]]);
    let first_index = u32::from_le_bytes(packets[0][16..20].try_into().unwrap()) >> 8;
    let mut ordered: Vec<_> = packets.iter().collect();
    ordered.sort_by_key(|packet| {
        let info = u32::from_le_bytes(packet[28..32].try_into().unwrap());
        (info >> 12) & 1023
    });
    let mut payload = Vec::new();
    for (index, packet) in ordered.iter().enumerate() {
        assert_eq!(packet[0], 0x90);
        assert_eq!(packet[1], 0);
        assert_eq!(&packet[8..16], &[0; 8]);
        let sequence = u16::from_be_bytes([packet[2], packet[3]]);
        assert_eq!(sequence, first_sequence.wrapping_add(index as u16));
        let stream = u32::from_le_bytes(packet[16..20].try_into().unwrap()) >> 8;
        assert_eq!(stream, first_index.wrapping_add(index as u32) & 0xffffff);
        assert_eq!(&packet[20..24], &ordered[0][20..24]);
        assert_eq!(&packet[4..8], &ordered[0][4..8]);
        assert_eq!(&packet[25..28], &[0, 0x10, 0]);
        let info = u32::from_le_bytes(packet[28..32].try_into().unwrap());
        assert_eq!((info >> 22) & 1023, packets.len() as u32);
        assert_eq!((info >> 12) & 1023, index as u32);
        assert_eq!((info >> 4) & 255, 0);
        assert_eq!(packet[24] & 1, 1);
        assert_eq!(packet[24] & 4 != 0, index == 0);
        assert_eq!(packet[24] & 2 != 0, index + 1 == packets.len());
        if index + 1 < packets.len() {
            assert_eq!(packet.len(), negotiated_size + 16);
        } else {
            assert!(packet.len() <= negotiated_size + 16);
        }
        payload.extend_from_slice(&packet[32..]);
    }
    assert_eq!(payload[0], 1); // Short frame header, independently skipped by client.
    assert_eq!(payload[1..3], [0, 0]);
    assert_eq!(payload[6..8], [0, 0]);
    let final_size = u16::from_le_bytes(payload[4..6].try_into().unwrap()) as usize;
    assert_eq!(final_size, ordered.last().unwrap().len() - 32);
    payload[8..].to_vec()
}

#[test]
fn single_idr_matches_explicit_wire_fixture() {
    let mut sender = packetizer(1392);
    let packets = sender
        .packetize(&[0, 0, 0, 1, 0x65, 0x88], 0x12345678, 0x01020304)
        .unwrap();
    assert_eq!(
        packets,
        vec![vec![
            0x90, 0, 0, 0, 1, 2, 3, 4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x78, 0x56, 0x34, 0x12,
            7, 0, 0x10, 0, 0, 0, 0x40, 0, 1, 0, 0, 2, 14, 0, 0, 0, 0, 0, 0, 1, 0x65, 0x88,
        ]]
    );
}

#[test]
fn receiver_reassembles_fragmented_annex_b_without_nal_rewriting() {
    let mut access_unit = vec![
        0, 0, 0, 1, 0x67, 0x42, 0x80, 0, 0, 1, 0x68, 0x88, 0, 0, 1, 0x65,
    ];
    access_unit.extend(std::iter::repeat_n(0x55, 4000));
    for size in [29, 64, 1024, 1392, 65491] {
        let packets = packetizer(size).packetize(&access_unit, 1, 90000).unwrap();
        assert_eq!(reassemble(&packets, size), access_unit);
        assert_eq!(packets[0][35], 2);
    }
}

#[test]
fn sequence_stream_frame_and_timestamp_wrap_without_overflow() {
    let mut sender = packetizer(64);
    let mut access_unit = vec![0, 0, 1, 0x61];
    access_unit.extend([0x55; 100]);
    for start in [65535, 0xffffff] {
        sender.stream_index = start;
        let packets = sender.packetize(&access_unit, u32::MAX, u32::MAX).unwrap();
        assert_eq!(reassemble(&packets, 64), access_unit);
        let next = sender.packetize(&[0, 0, 1, 0x61, 0x80], 0, 0).unwrap();
        assert_eq!(
            u32::from_le_bytes(next[0][16..20].try_into().unwrap()) >> 8,
            (start + 3) & 0xffffff
        );
        assert_eq!(&next[0][4..8], &[0; 4]);
        assert_eq!(&next[0][20..24], &[0; 4]);
        assert_eq!(next[0][35], 1);
    }
}

#[test]
fn maximum_single_block_and_exact_final_payload_are_representable() {
    let mut access_unit = vec![0x55; 48 * 1023 - 8];
    access_unit[..4].copy_from_slice(&[0, 0, 1, 0x65]);
    let mut sender = packetizer(64);
    let packets = sender.packetize(&access_unit, 1, 0).unwrap();
    assert_eq!(packets.len(), 1023);
    assert_eq!(reassemble(&packets, 64), access_unit);
    let next_index = sender.stream_index;
    access_unit.push(0x55);
    assert_eq!(
        sender.packetize(&access_unit, 2, 0),
        Err(VideoError::TooManyPackets)
    );
    assert_eq!(sender.stream_index, next_index);
}

#[test]
fn unsupported_modes_and_datagram_overflow_fail_explicitly() {
    for size in [0, 28, 65492, usize::MAX] {
        assert!(matches!(
            VideoPacketizer::new(VideoConfig {
                packet_size: size,
                fec_percentage: 0,
                min_parity_shards: 0,
                max_fec_blocks: 1,
                encrypted: false
            }),
            Err(VideoError::InvalidPacketSize)
        ));
    }
    assert!(matches!(
        VideoPacketizer::new(VideoConfig {
            packet_size: 1392,
            fec_percentage: 0,
            min_parity_shards: 2,
            max_fec_blocks: 1,
            encrypted: false
        }),
        Err(VideoError::InvalidFec)
    ));
    assert!(matches!(
        VideoPacketizer::new(VideoConfig {
            packet_size: 1392,
            fec_percentage: 0,
            min_parity_shards: 0,
            max_fec_blocks: 1,
            encrypted: true
        }),
        Err(VideoError::UnsupportedEncryption)
    ));
    assert_eq!(
        packetizer(1392).packetize(&vec![0; 8 * 1024 * 1024 + 1], 1, 0),
        Err(VideoError::AccessUnitTooLarge)
    );
}

#[test]
fn invalid_annex_b_does_not_consume_sequence_state() {
    let mut sender = packetizer(1392);
    for invalid in [
        &[][..],
        &[0, 0, 0, 2, 0x65],
        &[1, 0, 0, 1, 0x65],
        &[0, 0, 1],
        &[0, 0, 1, 0, 0, 1, 0x65],
        &[0, 0, 1, 0xe5],
        &[0, 0, 1, 0x67, 0x42],
        &[0, 0, 1, 0x78, 0x42],
    ] {
        assert_eq!(
            sender.packetize(invalid, 1, 0),
            Err(VideoError::InvalidAnnexB)
        );
        assert_eq!(sender.stream_index, 0);
    }
}

#[test]
fn reservation_matches_generated_datagrams_without_consuming_state() {
    for percentage in [0, 20, 100] {
        let mut sender = VideoPacketizer::new(VideoConfig {
            packet_size: 1392,
            fec_percentage: percentage,
            min_parity_shards: if percentage == 0 { 0 } else { 2 },
            max_fec_blocks: 4,
            encrypted: false,
        })
        .unwrap();
        for length in [5, 1376, 262144] {
            let mut frame = vec![0x55; length];
            frame[..5].copy_from_slice(&[0, 0, 0, 1, 0x65]);
            let before = sender.stream_index;
            let reserved = sender.reservation(frame.len()).unwrap();
            assert_eq!(sender.stream_index, before);
            let packets = sender.packetize(&frame, 1, 0).unwrap();
            assert_eq!(reserved.datagrams, packets.len());
            assert_eq!(reserved.bytes, packets.iter().map(Vec::len).sum::<usize>());
        }
    }
}
