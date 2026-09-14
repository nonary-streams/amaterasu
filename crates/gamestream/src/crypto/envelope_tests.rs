use super::*;

fn hex(value: &str) -> Vec<u8> {
    value
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}

#[test]
fn both_directions_match_independent_python_vectors_including_sequence_endianness() {
    for row in include_str!("envelope_vectors.txt").lines() {
        let fields: Vec<_> = row.split_whitespace().collect();
        let direction = if fields[1] == "H" {
            Direction::HostToClient
        } else {
            Direction::ClientToHost
        };
        let sequence: u32 = fields[2].parse().unwrap();
        let key: [u8; 16] = hex(fields[3]).try_into().unwrap();
        let plaintext = hex(fields[4]);
        let expected = hex(fields[5]);
        if fields[0] == "control" {
            let mut opener = ControlOpener::new(
                &key,
                ControlProfile::SunshineV2,
                direction,
                ControlOrdering::MultipleChannels,
            )
            .unwrap();
            let message = opener.open(&expected).unwrap();
            assert_eq!(message.sequence, sequence);
            assert_eq!(message.kind, 0x0305);
            assert_eq!(message.payload, plaintext[4..]);
            if sequence == 0 {
                assert_eq!(
                    ControlSealer::new(&key, ControlProfile::SunshineV2, direction)
                        .unwrap()
                        .seal(0x0305, &plaintext[4..])
                        .unwrap(),
                    expected
                );
            }
        } else {
            let mut opener = RtspOpener::new(&key, direction).unwrap();
            let message = opener.open(&expected).unwrap();
            assert_eq!(message.sequence, sequence);
            assert_eq!(message.plaintext, plaintext);
            if sequence == 1 {
                assert_eq!(
                    RtspSealer::new(&key, direction)
                        .unwrap()
                        .seal(&plaintext)
                        .unwrap(),
                    expected
                );
            }
        }
    }
}

#[test]
fn failed_authentication_never_consumes_replay_state_or_allows_reflection() {
    let key = [4; 16];
    let mut sealer =
        ControlSealer::new(&key, ControlProfile::SunshineV2, Direction::ClientToHost).unwrap();
    let packet = sealer.seal(0x205, &[1, 2, 3]).unwrap();
    let mut opener = ControlOpener::new(
        &key,
        ControlProfile::SunshineV2,
        Direction::ClientToHost,
        ControlOrdering::MultipleChannels,
    )
    .unwrap();
    for position in [4, 7, 8, 23, 24, packet.len() - 1] {
        let mut bad = packet.clone();
        bad[position] ^= 1;
        assert_eq!(
            opener.open(&bad).err(),
            Some(CryptoError::AuthenticationFailed)
        );
    }
    let mut reflected = ControlOpener::new(
        &key,
        ControlProfile::SunshineV2,
        Direction::HostToClient,
        ControlOrdering::MultipleChannels,
    )
    .unwrap();
    assert_eq!(
        reflected.open(&packet).err(),
        Some(CryptoError::AuthenticationFailed)
    );
    assert_eq!(opener.open(&packet).unwrap().sequence, 0);
    assert_eq!(
        opener.open(&packet).err(),
        Some(CryptoError::ReplayRejected)
    );
    let rtsp = RtspSealer::new(&key, Direction::ClientToHost)
        .unwrap()
        .seal(b"test")
        .unwrap();
    let mut rtsp_opener = RtspOpener::new(&key, Direction::ClientToHost).unwrap();
    let mut bad = rtsp.clone();
    bad[8] ^= 1;
    assert_eq!(
        rtsp_opener.open(&bad).err(),
        Some(CryptoError::AuthenticationFailed)
    );
    rtsp_opener.open(&rtsp).unwrap();
    assert_eq!(
        rtsp_opener.open(&rtsp).err(),
        Some(CryptoError::ReplayRejected)
    );
}

#[test]
fn control_channel_policy_handles_reordering_with_a_bounded_window() {
    let key = [3; 16];
    let mut sealer =
        ControlSealer::new(&key, ControlProfile::SunshineV2, Direction::ClientToHost).unwrap();
    let first = sealer.seal(1, &[]).unwrap();
    let second = sealer.seal(1, &[]).unwrap();
    let mut ordered = ControlOpener::new(
        &key,
        ControlProfile::SunshineV2,
        Direction::ClientToHost,
        ControlOrdering::SingleReliableChannel,
    )
    .unwrap();
    ordered.open(&second).unwrap();
    assert_eq!(
        ordered.open(&first).err(),
        Some(CryptoError::ReplayRejected)
    );
    let mut multiple = ControlOpener::new(
        &key,
        ControlProfile::SunshineV2,
        Direction::ClientToHost,
        ControlOrdering::MultipleChannels,
    )
    .unwrap();
    let mut latest = Vec::new();
    for _ in 2..CONTROL_REPLAY_WINDOW {
        latest = sealer.seal(1, &[]).unwrap();
    }
    multiple.open(&latest).unwrap();
    multiple.open(&first).unwrap(); // Exactly oldest retained slot.
    assert_eq!(
        multiple.open(&first).err(),
        Some(CryptoError::ReplayRejected)
    );
    let next = sealer.seal(1, &[]).unwrap();
    multiple.open(&next).unwrap();
    multiple.open(&second).unwrap();
    assert_eq!(
        multiple.open(&first).err(),
        Some(CryptoError::ReplayRejected)
    );
}

#[test]
fn rtsp_window_survives_connection_decoders_and_rejects_expired_valid_packets() {
    let key = [1; 16];
    let mut sealer = RtspSealer::new(&key, Direction::ClientToHost).unwrap();
    let packets: Vec<_> = (0..130).map(|_| sealer.seal(b"x").unwrap()).collect();
    let mut opener = RtspOpener::new(&key, Direction::ClientToHost).unwrap();
    for index in [128, 127, 1] {
        let mut connection = RtspEnvelopeDecoder::default();
        connection.push(&packets[index]).unwrap();
        opener
            .open(&connection.next_frame().unwrap().unwrap())
            .unwrap();
        connection.finish().unwrap();
    }
    assert_eq!(
        opener.open(&packets[0]).err(),
        Some(CryptoError::ReplayRejected)
    );
    assert_eq!(
        opener.open(&packets[127]).err(),
        Some(CryptoError::ReplayRejected)
    );
    opener.open(&packets[129]).unwrap();
}

#[test]
fn authenticated_bad_inner_control_length_does_not_commit_replay() {
    // Deliberate fixture-only nonce reuse constructs a valid MAC with invalid
    // inner framing; production callers must never recreate keys this way.
    let key = [9; 16];
    let mut raw = envelope::Sealer::new(&key, Direction::ClientToHost, b'C', 0).unwrap();
    let (sequence, tag, ciphertext) = raw.seal(&[5, 3, 99, 0, 1]).unwrap();
    let mut packet = vec![1, 0, 25, 0];
    packet.extend_from_slice(&sequence.to_le_bytes());
    packet.extend_from_slice(&tag);
    packet.extend_from_slice(&ciphertext);
    let mut opener = ControlOpener::new(
        &key,
        ControlProfile::SunshineV2,
        Direction::ClientToHost,
        ControlOrdering::SingleReliableChannel,
    )
    .unwrap();
    assert_eq!(
        opener.open(&packet).err(),
        Some(CryptoError::InvalidEnvelope)
    );
    let valid = ControlSealer::new(&key, ControlProfile::SunshineV2, Direction::ClientToHost)
        .unwrap()
        .seal(0x305, &[1])
        .unwrap();
    opener.open(&valid).unwrap();
}

#[test]
fn envelope_bounds_and_legacy_profile_are_explicit() {
    assert!(
        ControlSealer::new(
            &[0; 16],
            ControlProfile::NvidiaLegacy,
            Direction::HostToClient
        )
        .is_err()
    );
    let mut control = ControlSealer::new(
        &[0; 16],
        ControlProfile::SunshineV2,
        Direction::HostToClient,
    )
    .unwrap();
    assert_eq!(
        control.seal(1, &vec![0; MAX_CONTROL_PAYLOAD_BYTES + 1]),
        Err(CryptoError::InvalidPacketSize)
    );
    let maximum = control
        .seal(1, &vec![0; MAX_CONTROL_PAYLOAD_BYTES])
        .unwrap();
    assert_eq!(maximum.len(), 65_539);
    assert_eq!(&maximum[2..4], &[255, 255]);
    let packet = control.seal(1, &[]).unwrap();
    let mut opener = ControlOpener::new(
        &[0; 16],
        ControlProfile::SunshineV2,
        Direction::HostToClient,
        ControlOrdering::MultipleChannels,
    )
    .unwrap();
    for bytes in [&packet[..27], &packet[..packet.len() - 1]] {
        assert!(opener.open(bytes).is_err());
    }
    let mut extra = packet.clone();
    extra.push(0);
    assert!(opener.open(&extra).is_err());
    let mut wrong = packet.clone();
    wrong.swap(2, 3);
    assert!(opener.open(&wrong).is_err());
    opener.open(&packet).unwrap();
    assert_eq!(encrypted_rtsp_frame_length(&[0x80, 0, 0]), Ok(None));
    assert_eq!(
        encrypted_rtsp_frame_length(&[0, 0, 0, 1]),
        Err(CryptoError::UnsupportedProfile)
    );
    assert_eq!(
        encrypted_rtsp_frame_length(&[0xff; 4]),
        Err(CryptoError::InvalidPacketSize)
    );
}

#[test]
fn encrypted_rtsp_fragmentation_coalescing_and_eof_are_strict() {
    let mut sealer = RtspSealer::new(&[7; 16], Direction::HostToClient).unwrap();
    let packet = sealer.seal(b"RTSP/1.0 200 OK\r\nCSeq: 1\r\n\r\n").unwrap();
    for split in 0..packet.len() {
        let mut decoder = RtspEnvelopeDecoder::default();
        decoder.push(&packet[..split]).unwrap();
        assert!(decoder.next_frame().unwrap().is_none());
        decoder.push(&packet[split..]).unwrap();
        decoder.push(&packet).unwrap();
        assert_eq!(decoder.next_frame().unwrap().unwrap(), packet);
        assert_eq!(decoder.next_frame().unwrap().unwrap(), packet);
        decoder.finish().unwrap();
    }
    let mut decoder = RtspEnvelopeDecoder::default();
    decoder.push(&packet[..5]).unwrap();
    assert_eq!(decoder.finish(), Err(CryptoError::IncompleteEnvelope));
    assert_eq!(decoder.push(&packet), Err(CryptoError::DecoderPoisoned));
    let mut decoder = RtspEnvelopeDecoder::default();
    decoder.push(b"RTSP").unwrap();
    assert_eq!(decoder.next_frame(), Err(CryptoError::UnsupportedProfile));
}
