use super::*;
use std::{net::UdpSocket, time::Duration};

pub(crate) fn receivers() -> (UdpSocket, UdpSocket) {
    let video = UdpSocket::bind("127.0.0.1:0").unwrap();
    let audio = UdpSocket::bind("127.0.0.1:0").unwrap();
    for socket in [&video, &audio] {
        socket
            .set_read_timeout(Some(Duration::from_millis(200)))
            .unwrap();
    }
    (video, audio)
}

pub(crate) fn request(video: &UdpSocket, audio: &UdpSocket, encrypted: bool) -> Open {
    Open {
        peer_ip: "127.0.0.1".parse().unwrap(),
        video_port: Some(video.local_addr().unwrap().port()),
        audio_port: Some(audio.local_addr().unwrap().port()),
        discovery: None,
        video_bind: None,
        audio_bind: None,
        video: VideoSettings::default(),
        audio: AudioSettings {
            packet_ms: 10,
            channels: 2,
            wire_payload_bytes: if encrypted { 16 } else { 4 },
            ssrc: 0,
        },
        stream_audio: true,
        encryption: encrypted.then(|| Encryption {
            key_hex: "000102030405060708090a0b0c0d0e0f".into(),
            key_id: 9,
            video: true,
            audio: true,
            permit_legacy_audio_iv_wrap: false,
        }),
        operation_id: None,
    }
}

pub(crate) fn video_packet(bytes: &[u8], sequence: u64) -> EncodedPacket {
    EncodedPacket {
        codec: CODEC_H264,
        flags: FLAG_KEYFRAME,
        data: bytes.as_ptr(),
        len: bytes.len(),
        timestamp_us: sequence * 16667,
        sequence,
    }
}

pub(crate) fn audio_packet(bytes: &[u8], sequence: u64) -> EncodedAudioPacket {
    EncodedAudioPacket {
        codec: CODEC_OPUS,
        flags: 0,
        sample_rate: 48000,
        channels: 2,
        frames: 480,
        data: bytes.as_ptr(),
        len: bytes.len(),
        timestamp_us: sequence * 10000,
        sequence,
    }
}

fn receive(socket: &UdpSocket) -> Vec<u8> {
    let mut bytes = vec![0u8; 65507];
    let length = socket.recv(&mut bytes).unwrap();
    bytes.truncate(length);
    bytes
}

fn discovery_ping(token: &str) -> Vec<u8> {
    let mut bytes = token.as_bytes().to_vec();
    bytes.extend_from_slice(&1u32.to_be_bytes());
    bytes
}

#[test]
fn real_udp_video_fec_and_audio_groups_flush_with_no_invented_eos_datagram() {
    let (video, audio) = receivers();
    let mut stream = Stream::open("owner", &request(&video, &audio, false)).unwrap();
    let frame = [0, 0, 0, 1, 0x65, 1, 2, 3];
    // SAFETY: the locally owned encoded buffers remain live through submit.
    assert_eq!(
        unsafe { stream.submit_video(&video_packet(&frame, 0)) },
        STATUS_OK
    );
    let opus = [0x94, 1, 2, 3];
    for sequence in 0..4 {
        assert_eq!(
            unsafe { stream.submit_audio(&audio_packet(&opus, sequence)) },
            STATUS_OK
        );
    }
    assert_eq!(stream.delivery.queued(), 9); // One data+two parity video; four+two audio.
    assert_eq!(stream.drain(), STATUS_OK);
    assert_eq!(stream.state(), "draining");
    stream.delivery.tick();
    assert_eq!(stream.state(), "drained");
    for _ in 0..3 {
        assert!(receive(&video).len() > 32);
    }
    for index in 0..6 {
        let bytes = receive(&audio);
        assert_eq!(bytes[1], if index < 4 { 97 } else { 127 });
        if index < 4 {
            assert_eq!(&bytes[12..], &opus);
        }
    }
    assert_eq!(stream.delivery.sent_datagrams, 9);
    assert_eq!(stream.drain(), STATUS_OK);
    assert_eq!(
        unsafe { stream.submit_video(&video_packet(&frame, 1)) },
        MEDIA_CLOSED
    );
    assert_eq!(
        unsafe { stream.submit_audio(&audio_packet(&opus, 4)) },
        MEDIA_CLOSED
    );
    assert!(video.recv(&mut [0; 32]).is_err());
    assert!(audio.recv(&mut [0; 32]).is_err());
}

#[test]
fn queue_busy_does_not_consume_video_indices_or_crypto_nonces() {
    let (video, audio) = receivers();
    let mut stream = Stream::open("owner", &request(&video, &audio, true)).unwrap();
    stream
        .delivery
        .push(Medium::Video, vec![vec![0]; MAX_DATAGRAMS]);
    let frame = [0, 0, 0, 1, 0x65, 1, 2, 3];
    assert_eq!(
        unsafe { stream.submit_video(&video_packet(&frame, 0)) },
        MEDIA_BUSY
    );
    assert_eq!(stream.next_frame_index, Some(1));
    assert_eq!(stream.video_sequence, None);
    stream.delivery = Delivery::bind(
        "127.0.0.1:0".parse().unwrap(),
        "127.0.0.1:0".parse().unwrap(),
        video.local_addr().unwrap(),
        audio.local_addr().unwrap(),
    )
    .unwrap();
    assert_eq!(
        unsafe { stream.submit_video(&video_packet(&frame, 0)) },
        STATUS_OK
    );
    stream.delivery.tick();
    let packet = receive(&video);
    assert_eq!(&packet[..8], &0u64.to_le_bytes());
    assert_eq!(packet[11], b'V');
    assert_eq!(&packet[12..16], &1u32.to_le_bytes());
    assert_eq!(&receive(&video)[..8], &1u64.to_le_bytes());
    assert_eq!(&receive(&video)[..8], &2u64.to_le_bytes());
}

#[test]
fn audio_ciphertext_is_queued_once_and_discontinuity_skips_iv_slots_without_resetting_key() {
    let (video, audio) = receivers();
    let mut stream = Stream::open("owner", &request(&video, &audio, true)).unwrap();
    let opus = [0x94, 1, 2, 3];
    stream
        .delivery
        .push(Medium::Audio, vec![vec![0]; MAX_DATAGRAMS]);
    assert_eq!(
        unsafe { stream.submit_audio(&audio_packet(&opus, 0)) },
        MEDIA_BUSY
    );
    assert_eq!(stream.audio.next_sequence(), 0);
    stream.delivery = Delivery::bind(
        "127.0.0.1:0".parse().unwrap(),
        "127.0.0.1:0".parse().unwrap(),
        video.local_addr().unwrap(),
        audio.local_addr().unwrap(),
    )
    .unwrap();
    assert_eq!(
        unsafe { stream.submit_audio(&audio_packet(&opus, 0)) },
        STATUS_OK
    );
    let reset = EncodedAudioPacket {
        flags: AUDIO_PACKET_DISCONTINUITY,
        ..audio_packet(&opus, 9)
    };
    assert_eq!(unsafe { stream.submit_audio(&reset) }, STATUS_OK);
    assert_eq!(stream.discarded_audio_fec_shards, 1);
    stream.delivery.tick();
    let first = receive(&audio);
    let second = receive(&audio);
    assert_eq!(&first[2..4], &0u16.to_be_bytes());
    assert_eq!(&second[2..4], &4u16.to_be_bytes());
    assert_ne!(&first[12..], &second[12..]);
    let key: [u8; 16] = std::array::from_fn(|i| i as u8);
    let mut oracle = AudioSealer::new(&key, 9, AudioIvPolicy::RejectReuse);
    assert_eq!(&first[12..], oracle.encrypt(0, &opus).unwrap());
    oracle.skip_to(4).unwrap();
    assert_eq!(&second[12..], oracle.encrypt(4, &opus).unwrap());
}

#[test]
fn invalid_media_changes_no_queued_bytes_or_sequence_and_eos_closes_only_its_medium() {
    let (video, audio) = receivers();
    let mut stream = Stream::open("owner", &request(&video, &audio, true)).unwrap();
    let bad = [1, 2, 3, 4, 5];
    assert_eq!(
        unsafe { stream.submit_video(&video_packet(&bad, 0)) },
        STATUS_INVALID_ARGUMENT
    );
    let bad_audio = [0x94; 17]; // PKCS7 ciphertext would exceed negotiated 16 bytes.
    assert_eq!(
        unsafe { stream.submit_audio(&audio_packet(&bad_audio, 0)) },
        STATUS_INVALID_ARGUMENT
    );
    assert_eq!(stream.delivery.queued_bytes(), 0);
    assert_eq!(stream.audio.next_sequence(), 0);
    let eos = EncodedPacket {
        flags: FLAG_END_OF_STREAM,
        data: std::ptr::null(),
        len: 0,
        ..video_packet(&bad, 0)
    };
    assert_eq!(unsafe { stream.submit_video(&eos) }, STATUS_OK);
    assert_eq!(stream.state(), "sending");
    assert_eq!(
        unsafe { stream.submit_audio(&audio_packet(&[0x94, 1, 2, 3], 0)) },
        STATUS_OK
    );
}

#[test]
fn disabled_audio_accepts_only_eos_and_never_queues_audio_datagrams() {
    let (video, audio) = receivers();
    let mut settings = request(&video, &audio, false);
    settings.stream_audio = false;
    settings.audio.wire_payload_bytes = 16;
    let mut stream = Stream::open("owner", &settings).unwrap();
    assert_eq!(stream.encryption(), (false, false));

    let opus = [0x94, 1, 2, 3];
    assert_eq!(
        unsafe { stream.submit_audio(&audio_packet(&opus, 0)) },
        MEDIA_UNSUPPORTED
    );
    assert_eq!(stream.delivery.queued(), 0);
    assert_eq!(stream.audio_sequence, None);

    let eos = EncodedAudioPacket {
        flags: AUDIO_PACKET_END_OF_STREAM,
        frames: 0,
        data: std::ptr::null(),
        len: 0,
        ..audio_packet(&[], 0)
    };
    assert_eq!(unsafe { stream.submit_audio(&eos) }, STATUS_OK);
    assert_eq!(stream.state(), "sending");
    assert_eq!(
        unsafe { stream.submit_audio(&audio_packet(&opus, 0)) },
        MEDIA_CLOSED
    );
    let video_eos = EncodedPacket {
        flags: FLAG_END_OF_STREAM,
        data: std::ptr::null(),
        len: 0,
        ..video_packet(&[], 0)
    };
    assert_eq!(unsafe { stream.submit_video(&video_eos) }, STATUS_OK);
    assert_eq!(stream.state(), "drained");
    assert_eq!(stream.delivery.queued(), 0);
    assert!(audio.recv(&mut [0; 32]).is_err());
}

#[test]
fn one_batch_larger_than_hard_byte_limit_is_invalid_instead_of_permanently_busy() {
    let (video, audio) = receivers();
    let mut settings = request(&video, &audio, false);
    settings.video = VideoSettings {
        packet_size: 65_507 - 16,
        fec_percentage: 0,
        min_parity_shards: 0,
        max_fec_blocks: 1,
    };
    let mut stream = Stream::open("owner", &settings).unwrap();
    let mut bytes = vec![0x55; MAX_QUEUE_BYTES];
    bytes[..5].copy_from_slice(&[0, 0, 0, 1, 0x65]);
    // The access unit itself fits the packetizer, but its added RTP/NV/frame
    // headers exceed the transport's total byte allowance even on an empty queue.
    assert_eq!(
        unsafe { stream.submit_video(&video_packet(&bytes, 0)) },
        STATUS_INVALID_ARGUMENT
    );
    assert_eq!(stream.delivery.queued(), 0);
    assert_eq!(stream.next_frame_index, Some(1));
}

#[test]
fn discovery_admission_is_busy_without_consuming_media_state_until_both_lanes_freeze() {
    let (unused_video, unused_audio) = receivers();
    let mut settings = request(&unused_video, &unused_audio, true);
    settings.video_port = None;
    settings.audio_port = None;
    settings.discovery = Some(Box::new(Discovery {
        video_ping_payload: "video_token-1234".into(),
        audio_ping_payload: "audio_token-1234".into(),
    }));
    let mut stream = Stream::open("owner", &settings).unwrap();
    let video_peer = UdpSocket::bind("127.0.0.1:0").unwrap();
    let audio_peer = UdpSocket::bind("127.0.0.1:0").unwrap();
    let frame = [0, 0, 0, 1, 0x65, 1, 2, 3];
    let opus = [0x94, 1, 2, 3];

    video_peer
        .send_to(&discovery_ping("video_token-1234"), stream.video_local)
        .unwrap();
    wait_for_peer(&mut stream, crate::datagrams::Medium::Video);
    assert_eq!(stream.state(), "discovering");
    // SAFETY: locally owned packet bytes remain live through admission.
    assert_eq!(
        unsafe { stream.submit_video(&video_packet(&frame, 0)) },
        MEDIA_BUSY
    );
    assert_eq!(
        unsafe { stream.submit_audio(&audio_packet(&opus, 0)) },
        MEDIA_BUSY
    );
    assert_eq!(stream.next_frame_index, Some(1));
    assert_eq!(stream.video_sequence, None);
    assert_eq!(stream.audio.next_sequence(), 0);
    assert_eq!(stream.delivery.queued(), 0);

    audio_peer
        .send_to(&discovery_ping("audio_token-1234"), stream.audio_local)
        .unwrap();
    wait_for_peer(&mut stream, crate::datagrams::Medium::Audio);
    assert_eq!(stream.state(), "sending");
    assert_eq!(
        unsafe { stream.submit_video(&video_packet(&frame, 0)) },
        STATUS_OK
    );
    assert_eq!(
        unsafe { stream.submit_audio(&audio_packet(&opus, 0)) },
        STATUS_OK
    );
}

fn wait_for_peer(stream: &mut Stream, medium: crate::datagrams::Medium) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
    while stream.peer_port(medium).is_none() {
        stream.delivery.tick();
        assert!(
            std::time::Instant::now() < deadline,
            "UDP peer was not discovered"
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}
