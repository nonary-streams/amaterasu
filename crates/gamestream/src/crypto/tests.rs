use super::*;
use aes::cipher::{BlockDecryptMut, KeyIvInit, block_padding::Pkcs7};
use ring::aead::{AES_128_GCM, Aad, LessSafeKey, Nonce, UnboundKey};

fn hex(value: &str) -> Vec<u8> {
    value
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}

fn open_video(key: &[u8; 16], packet: &[u8]) -> Result<Vec<u8>, ring::error::Unspecified> {
    // Receive-side offset interpretation deliberately separate from the sealer.
    let nonce: [u8; 12] = packet[..12].try_into().unwrap();
    let mut ciphertext = packet[32..].to_vec();
    ciphertext.extend_from_slice(&packet[16..32]);
    let key = LessSafeKey::new(UnboundKey::new(&AES_128_GCM, key).unwrap());
    key.open_in_place(
        Nonce::assume_unique_for_key(nonce),
        Aad::empty(),
        &mut ciphertext,
    )
    .map(|plaintext| plaintext.to_vec())
}

#[test]
fn matches_independent_python_aes_gcm_and_cbc_vectors() {
    let mut video = VideoSealer::new(
        &core::array::from_fn(|i| i as u8),
        VideoProfile::SunshineHostToClient,
    )
    .unwrap();
    for row in include_str!("vectors.txt").lines() {
        let columns: Vec<_> = row.split_whitespace().collect();
        let key: [u8; 16] = hex(columns[3]).try_into().unwrap();
        let plaintext = hex(columns[4]);
        let expected = hex(columns[5]);
        let actual = match columns[0] {
            "video" => video.seal(columns[2].parse().unwrap(), &plaintext).unwrap(),
            "audio" => AudioSealer::new(
                &key,
                columns[1].parse().unwrap(),
                AudioIvPolicy::RejectReuse,
            )
            .encrypt(columns[2].parse().unwrap(), &plaintext)
            .unwrap(),
            _ => panic!("invalid fixture"),
        };
        assert_eq!(actual, expected);
    }
}

#[test]
fn video_tampering_fails_but_outer_frame_hint_is_not_authenticated() {
    let key = [7; 16];
    let mut video = VideoSealer::new(&key, VideoProfile::SunshineHostToClient).unwrap();
    let packet = video.seal(0x01020304, &[0x55; 64]).unwrap();
    assert_eq!(&packet[12..16], &[4, 3, 2, 1]);
    assert_eq!(open_video(&key, &packet).unwrap(), [0x55; 64]);
    for position in [0, 11, 16, 31, 32, packet.len() - 1] {
        let mut tampered = packet.clone();
        tampered[position] ^= 1;
        assert!(open_video(&key, &tampered).is_err());
    }
    let mut hint = packet;
    hint[12] ^= 1;
    assert_eq!(open_video(&key, &hint).unwrap(), [0x55; 64]);
    assert!(open_video(&[8; 16], &hint).is_err());
}

#[test]
fn video_bounds_do_not_consume_nonces_and_each_valid_packet_gets_a_new_one() {
    let mut video = VideoSealer::new(&[0; 16], VideoProfile::SunshineHostToClient).unwrap();
    for size in [0, 15, MAX_VIDEO_PLAINTEXT_BYTES + 1] {
        assert_eq!(
            video.seal(0, &vec![0; size]),
            Err(CryptoError::InvalidPacketSize)
        );
    }
    let first = video.seal(1, &[0; 16]).unwrap();
    let second = video.seal(1, &[0; 16]).unwrap();
    assert_eq!(&first[..8], &0u64.to_le_bytes());
    assert_eq!(&second[..8], &1u64.to_le_bytes());
    assert_ne!(&first[16..], &second[16..]);
    assert_eq!(
        video
            .seal(2, &vec![0; MAX_VIDEO_PLAINTEXT_BYTES])
            .unwrap()
            .len(),
        65_507
    );
}

#[test]
fn audio_pkcs7_is_legacy_unauthenticated_encryption() {
    let key = [5; 16];
    let mut audio = AudioSealer::new(&key, 0, AudioIvPolicy::RejectReuse);
    let mut ciphertext = audio.encrypt(0, &[42; 64]).unwrap();
    assert_eq!(ciphertext.len(), 80);
    // Alter an early block, leaving the final padding block untouched. Successful
    // padding validation is not integrity verification or replay protection.
    ciphertext[0] ^= 1;
    let plaintext = cbc::Decryptor::<aes::Aes128>::new(&key.into(), &[0; 16].into())
        .decrypt_padded_mut::<Pkcs7>(&mut ciphertext)
        .unwrap();
    assert_ne!(plaintext, &[42; 64]);
}

#[test]
fn audio_wrap_policy_is_explicit_and_rejects_reuse_by_default_policy() {
    let mut guarded = AudioSealer::new(&[1; 16], 7, AudioIvPolicy::RejectReuse);
    for sequence in 0..=u16::MAX {
        guarded.encrypt(sequence, &[1]).unwrap();
    }
    assert!(!guarded.iv_reuse_started());
    assert_eq!(guarded.encrypt(0, &[1]), Err(CryptoError::AudioIvExhausted));
    let mut legacy = AudioSealer::new(&[1; 16], 7, AudioIvPolicy::PermitLegacyWrap);
    let first = legacy.encrypt(0, &[1]).unwrap();
    for sequence in 1..=u16::MAX {
        legacy.encrypt(sequence, &[1]).unwrap();
    }
    assert_eq!(legacy.encrypt(0, &[1]).unwrap(), first);
    assert!(legacy.iv_reuse_started());
}

#[test]
fn audio_discontinuities_burn_slots_without_resetting_key_or_budget() {
    let mut audio = AudioSealer::new(&[1; 16], 0, AudioIvPolicy::RejectReuse);
    audio.encrypt(0, &[1]).unwrap();
    assert_eq!(
        audio.encrypt(0, &[1]),
        Err(CryptoError::AudioSequenceMismatch)
    );
    assert_eq!(
        audio.encrypt(4, &[1]),
        Err(CryptoError::AudioSequenceMismatch)
    );
    audio.skip_to(4).unwrap();
    audio.encrypt(4, &[1]).unwrap();
    assert_eq!(audio.skip_to(3), Err(CryptoError::AudioSequenceMismatch));
    audio.skip_to(32772).unwrap();
    audio.skip_to(65535).unwrap();
    audio.encrypt(65535, &[1]).unwrap();
    assert_eq!(audio.encrypt(0, &[1]), Err(CryptoError::AudioIvExhausted));
}

#[test]
fn audio_size_failure_does_not_consume_sequence_and_padding_stays_bounded() {
    let mut audio = AudioSealer::new(&[0; 16], u32::MAX, AudioIvPolicy::RejectReuse);
    assert_eq!(
        audio.encrypt(65535, &[]),
        Err(CryptoError::InvalidPacketSize)
    );
    assert_eq!(
        audio.encrypt(65535, &vec![0; MAX_AUDIO_PLAINTEXT_BYTES + 1]),
        Err(CryptoError::InvalidPacketSize)
    );
    assert_eq!(
        audio
            .encrypt(65535, &vec![0; MAX_AUDIO_PLAINTEXT_BYTES])
            .unwrap()
            .len(),
        1280
    );
    assert_eq!(audio.encrypt(0, &[1; 16]).unwrap().len(), 32);
}
