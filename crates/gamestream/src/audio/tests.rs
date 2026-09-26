use super::*;

fn config(bytes: usize) -> AudioConfig {
    AudioConfig {
        sample_rate: 48000,
        channels: 2,
        frames_per_packet: 480,
        wire_payload_bytes: bytes,
        ssrc: 0x01020304,
    }
}

fn packet(index: u8) -> Vec<u8> {
    vec![0x94, index * 3 + 1, index * 3 + 2, index * 3 + 3]
}

fn group(sender: &mut AudioPacketizer) -> Vec<Vec<u8>> {
    let mut packets = Vec::new();
    for index in 0..4 {
        packets.extend(sender.packetize(&packet(index), 480).unwrap());
    }
    packets
}

#[test]
fn explicit_rtp_and_fixed_matrix_fixtures_match_client_wire_format() {
    let packets = group(&mut AudioPacketizer::new(config(4)).unwrap());
    assert_eq!(
        packets[0],
        [0x80, 97, 0, 0, 0, 0, 0, 0, 1, 2, 3, 4, 0x94, 1, 2, 3]
    );
    // Parity bytes from the pinned upstream C oracle with the client's fixed matrix.
    assert_eq!(
        packets[4],
        [
            0x80, 127, 0, 4, 0, 0, 0, 0, 1, 2, 3, 4, 0, 97, 0, 0, 0, 0, 0, 0, 1, 2, 3, 4, 148, 174,
            12, 169
        ]
    );
    assert_eq!(
        packets[5],
        [
            0x80, 127, 0, 5, 0, 0, 0, 0, 1, 2, 3, 4, 1, 97, 0, 0, 0, 0, 0, 0, 1, 2, 3, 4, 148, 221,
            9, 46
        ]
    );
}

#[test]
fn independent_receiver_fields_preserve_duration_and_sequence_wrap() {
    let mut sender = AudioPacketizer::new(config(4)).unwrap();
    sender.sequence = 65532;
    sender.timestamp_ms = u32::MAX - 9;
    let packets = group(&mut sender);
    for (index, bytes) in packets[..4].iter().enumerate() {
        assert_eq!(bytes[0], 0x80);
        assert_eq!(bytes[1], 97);
        assert_eq!(
            u16::from_be_bytes(bytes[2..4].try_into().unwrap()),
            65532 + index as u16
        );
        assert_eq!(
            u32::from_be_bytes(bytes[4..8].try_into().unwrap()),
            (u32::MAX - 9).wrapping_add(index as u32 * 10)
        );
        assert_eq!(&bytes[12..], packet(index as u8));
    }
    for (index, bytes) in packets[4..].iter().enumerate() {
        assert_eq!(bytes[1], 127);
        assert_eq!(
            u16::from_be_bytes(bytes[2..4].try_into().unwrap()),
            index as u16
        );
        assert_eq!(bytes[12], index as u8);
        assert_eq!(bytes[13], 97);
        assert_eq!(&bytes[14..16], &65532u16.to_be_bytes());
        assert_eq!(&bytes[16..20], &(u32::MAX - 9).to_be_bytes());
        assert_eq!(&bytes[20..24], &0x01020304u32.to_be_bytes());
    }
    assert_eq!(sender.next_sequence(), 0);
    let next = sender.packetize(&packet(0), 480).unwrap();
    assert_eq!(&next[0][2..4], &[0, 0]); // Parity sequence is deliberately reused by data.
    assert_eq!(&next[0][4..8], &30u32.to_be_bytes());
}

#[test]
fn queue_preflight_is_bounded_and_partial_groups_never_emit_parity() {
    let mut sender = AudioPacketizer::new(config(4)).unwrap();
    for index in 0..4 {
        assert_eq!(sender.required_datagrams(), if index == 3 { 3 } else { 1 });
        assert_eq!(sender.maximum_datagram_bytes(), 28);
        assert_eq!(sender.next_sequence(), index);
        let output = sender.packetize(&packet(index as u8), 480).unwrap();
        assert_eq!(output.len(), if index == 3 { 3 } else { 1 });
        assert!(sender.pending.len() <= 3);
    }
    assert!(sender.pending.is_empty());
    for retained in 1..=3 {
        let mut sender = AudioPacketizer::new(config(4)).unwrap();
        for _ in 0..retained {
            assert_eq!(sender.packetize(&packet(0), 480).unwrap().len(), 1);
        }
        let reset = sender.reset_discontinuity();
        assert_eq!(reset.discarded_shards, retained);
        assert_eq!(reset.skipped_packets, 4 - retained);
        assert_eq!(reset.next_sequence, 4);
        assert_eq!(reset.timestamp_ms, 40);
        assert_eq!(sender.required_datagrams(), 1);
        assert_eq!(sender.reset_discontinuity().skipped_packets, 0);
    }
}

#[test]
fn invalid_opus_duration_or_length_does_not_consume_state_or_sealer() {
    let mut sender = AudioPacketizer::new(config(16)).unwrap();
    for (bytes, frames) in [
        (&[][..], 480),
        (&[0x94][..], 240),
        (&[0x95, 1][..], 480),
        (&[0x97][..], 480),
        (&[0x94; 17][..], 480),
    ] {
        assert!(
            sender
                .packetize_sealed(bytes, frames, |_, _| panic!(
                    "invalid input consumed cipher state"
                ))
                .is_err()
        );
        assert_eq!(sender.next_sequence(), 0);
        assert!(sender.pending.is_empty());
    }
    assert_eq!(
        sender.packetize_sealed(&packet(0), 480, |_, _| Err(AudioError::EncryptionFailed)),
        Err(AudioError::EncryptionFailed)
    );
    assert_eq!(sender.next_sequence(), 0);
    let mut plain = AudioPacketizer::new(config(4)).unwrap();
    assert_eq!(
        plain.packetize(&[0x94; 5], 480),
        Err(AudioError::PayloadLengthChanged)
    );
    assert_eq!(plain.next_sequence(), 0);
}

#[test]
fn ciphertext_is_the_fec_input_and_cbc_padding_is_not_added_after_parity() {
    let mut sender = AudioPacketizer::new(config(16)).unwrap();
    let mut packets = Vec::new();
    for index in 0..4 {
        packets.extend(
            sender
                .packetize_sealed(&packet(index), 480, |sequence, _| {
                    assert_eq!(sequence, u16::from(index));
                    Ok(vec![index + 1; 16])
                })
                .unwrap(),
        );
    }
    let expected = linear_combination(
        &AUDIO_MATRIX[0],
        &(1..=4).map(|byte| vec![byte; 16]).collect::<Vec<_>>(),
    );
    assert_eq!(&packets[4][24..], expected);
    assert!(packets[..4].iter().all(|packet| packet.len() == 28));
    assert!(packets[4..].iter().all(|packet| packet.len() == 40));
}

#[test]
fn negotiated_formats_and_opus_frame_packing_are_validated() {
    for settings in [
        AudioConfig {
            sample_rate: 44100,
            ..config(4)
        },
        AudioConfig {
            channels: 4,
            ..config(4)
        },
        AudioConfig {
            channels: 7,
            ..config(4)
        },
        AudioConfig {
            frames_per_packet: 960,
            ..config(4)
        },
    ] {
        assert!(matches!(
            AudioPacketizer::new(settings),
            Err(AudioError::UnsupportedFormat)
        ));
    }
    for bytes in [0, 1281, 1376, usize::MAX] {
        assert!(matches!(
            AudioPacketizer::new(config(bytes)),
            Err(AudioError::InvalidPayloadSize)
        ));
    }
    assert_eq!(opus::decoded_frames(&[0x94]), Ok(480));
    assert_eq!(opus::decoded_frames(&[0x8c]), Ok(240));
    assert_eq!(opus::decoded_frames(&[0x8d, 1, 2]), Ok(480));
    assert_eq!(opus::decoded_frames(&[0x8e, 1, 2, 3]), Ok(480));
    assert_eq!(opus::decoded_frames(&[0x87, 4, 1, 2, 3, 4]), Ok(480));
    assert_eq!(opus::decoded_frames(&[0x87, 0x42, 1, 1, 2, 0]), Ok(240));
    for malformed in [
        &[0x8d, 1][..],
        &[0x8e, 252][..],
        &[0x87, 0][..],
        &[0x87, 49][..],
        &[0x87, 0x42, 255][..],
        &[0x87, 0x82, 250, 1][..],
    ] {
        assert!(opus::decoded_frames(malformed).is_err());
    }
}

#[test]
fn surround_channels_preserve_rtp_fec_and_discontinuity_with_bounded_wire_shards() {
    for channels in [6, 8] {
        for frames in [240, 480] {
            let mut sender = AudioPacketizer::new(AudioConfig {
                channels,
                frames_per_packet: frames,
                ..config(4)
            })
            .unwrap();
            let opus = if frames == 240 {
                [0x8c, 1, 2, 3]
            } else {
                [0x94, 1, 2, 3]
            };
            let mut datagrams = Vec::new();
            for _ in 0..4 {
                datagrams.extend(sender.packetize(&opus, frames).unwrap());
            }
            assert_eq!(datagrams.len(), 6);
            assert!(
                datagrams[..4]
                    .iter()
                    .all(|packet| packet.len() == 16 && packet[1] == 97)
            );
            assert!(
                datagrams[4..]
                    .iter()
                    .all(|packet| packet.len() == 28 && packet[1] == 127)
            );
            assert_eq!(sender.next_sequence(), 4);
            sender.packetize(&opus, frames).unwrap();
            let reset = sender.reset_discontinuity();
            assert_eq!(
                (
                    reset.discarded_shards,
                    reset.skipped_packets,
                    reset.next_sequence
                ),
                (1, 3, 8)
            );
        }
        let mut oversized = AudioPacketizer::new(AudioConfig {
            channels,
            wire_payload_bytes: 1280,
            ..config(4)
        })
        .unwrap();
        assert_eq!(
            oversized.packetize(&vec![0x94; 1276], 480),
            Err(AudioError::InvalidPayloadSize)
        );
        assert_eq!(oversized.next_sequence(), 0);
        let maximum_plain = vec![0x94; 1275];
        let mut plain = AudioPacketizer::new(AudioConfig {
            channels,
            wire_payload_bytes: 1275,
            ..config(4)
        })
        .unwrap();
        assert_eq!(plain.packetize(&maximum_plain, 480).unwrap()[0].len(), 1287);
        let sealed = oversized
            .packetize_sealed(&maximum_plain, 480, |_, _| Ok(vec![0; 1280]))
            .unwrap();
        assert_eq!(sealed[0].len(), 1292);
    }
}

fn oracle(mode: &str, input: &[u8], bytes: usize) -> Vec<u8> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let path = std::env::var("AMATERASU_NANORS_ORACLE")
        .expect("build the documented upstream nanors oracle first");
    let mut child = Command::new(path)
        .args([mode, "4", "2", &bytes.to_string()])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    output.stdout
}

#[test]
#[ignore = "requires separately compiled pinned upstream nanors audio oracle"]
fn upstream_audio_encoder_and_decoder_match_all_two_erasure_patterns() {
    for bytes in [4, 16, 80, 1275] {
        let mut sender = AudioPacketizer::new(config(bytes)).unwrap();
        let mut packets = Vec::new();
        for index in 0..4 {
            let mut data = vec![index as u8 + 1; bytes];
            data[0] = 0x94;
            packets.extend(sender.packetize(&data, 480).unwrap());
        }
        let data: Vec<u8> = packets[..4]
            .iter()
            .flat_map(|packet| packet[12..].iter().copied())
            .collect();
        let parity: Vec<u8> = packets[4..]
            .iter()
            .flat_map(|packet| packet[24..].iter().copied())
            .collect();
        assert_eq!(oracle("audio-encode", &data, bytes), parity);
        let shards = [&data[..], &parity[..]].concat();
        for first in 0..6 {
            for second in first + 1..6 {
                let mut input = vec![0; 6];
                input[first] = 1;
                input[second] = 1;
                let mut erased = shards.clone();
                erased[first * bytes..(first + 1) * bytes].fill(0);
                erased[second * bytes..(second + 1) * bytes].fill(0);
                input.extend_from_slice(&erased);
                assert_eq!(oracle("audio-decode", &input, bytes), data);
            }
        }
    }
}

#[test]
#[ignore = "requires separately compiled pinned upstream nanors audio oracle"]
fn recovered_cbc_payload_decrypts_to_the_original_opus_after_discontinuity() {
    use crate::crypto::{AudioIvPolicy, AudioSealer};
    use aes::cipher::{BlockDecryptMut, KeyIvInit, block_padding::Pkcs7};
    let key = [0x35; 16];
    let key_id = 0xfffffffdu32;
    let mut sealer = AudioSealer::new(&key, key_id, AudioIvPolicy::RejectReuse);
    let mut sender = AudioPacketizer::new(config(16)).unwrap();
    sender
        .packetize_sealed(&packet(0), 480, |sequence, opus| {
            sealer
                .encrypt(sequence, opus)
                .map_err(|_| AudioError::EncryptionFailed)
        })
        .unwrap();
    let reset = sender.reset_discontinuity();
    sealer.skip_to(reset.next_sequence).unwrap();
    let mut packets = Vec::new();
    for index in 0..4 {
        packets.extend(
            sender
                .packetize_sealed(&packet(index), 480, |sequence, opus| {
                    sealer
                        .encrypt(sequence, opus)
                        .map_err(|_| AudioError::EncryptionFailed)
                })
                .unwrap(),
        );
    }
    let mut input = vec![1, 0, 0, 1, 0, 0];
    for (index, packet) in packets.iter().enumerate() {
        if index == 0 || index == 3 {
            input.extend_from_slice(&[0; 16]);
        } else {
            input.extend_from_slice(&packet[if index < 4 { 12 } else { 24 }..]);
        }
    }
    let recovered = oracle("audio-decode", &input, 16);
    for (index, ciphertext) in recovered.as_chunks::<16>().0.iter().enumerate() {
        let sequence = 4 + index as u16;
        let mut iv = [0; 16];
        iv[..4].copy_from_slice(&key_id.wrapping_add(u32::from(sequence)).to_be_bytes());
        let mut bytes = ciphertext.to_vec();
        let plain = cbc::Decryptor::<aes::Aes128>::new(&key.into(), &iv.into())
            .decrypt_padded_mut::<Pkcs7>(&mut bytes)
            .unwrap();
        assert_eq!(plain, packet(index as u8));
    }
}

#[test]
fn five_millisecond_audio_uses_the_millisecond_clock_not_sample_clock() {
    let mut sender = AudioPacketizer::new(AudioConfig {
        frames_per_packet: 240,
        ..config(4)
    })
    .unwrap();
    for index in 0..4 {
        let output = sender.packetize(&[0x8c, 1, 2, 3], 240).unwrap();
        assert_eq!(&output[0][4..8], &(index * 5u32).to_be_bytes());
    }
    assert_eq!(sender.timestamp_ms, 20);
}
