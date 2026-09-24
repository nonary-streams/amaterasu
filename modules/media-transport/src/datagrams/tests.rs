use super::*;

const VIDEO_TOKEN: [u8; 16] = *b"video_token-1234";
const AUDIO_TOKEN: [u8; 16] = *b"audio_token-1234";

fn ping(token: [u8; 16], sequence: u32) -> Vec<u8> {
    let mut bytes = token.to_vec();
    bytes.extend_from_slice(&sequence.to_be_bytes());
    bytes
}

fn sender() -> UdpSocket {
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    socket
        .set_read_timeout(Some(Duration::from_millis(200)))
        .unwrap();
    socket
}

fn delivery() -> Delivery {
    Delivery::bind(
        "127.0.0.1:0".parse().unwrap(),
        "127.0.0.1:0".parse().unwrap(),
        "127.0.0.1:49997".parse().unwrap(),
        "127.0.0.1:49998".parse().unwrap(),
    )
    .unwrap()
}

#[test]
fn would_block_and_interrupt_retain_exact_datagram_then_fatal_error_clears_visible_queue() {
    let mut delivery = delivery();
    delivery.push(Medium::Video, vec![vec![1, 2, 3], vec![4, 5]]);
    assert!(!delivery.record_send(Err(io::Error::from(io::ErrorKind::WouldBlock))));
    assert!(!delivery.record_send(Err(io::Error::from(io::ErrorKind::Interrupted))));
    assert_eq!(delivery.queue.front().unwrap().bytes, [1, 2, 3]);
    assert_eq!(delivery.bytes, 5);
    assert!(delivery.record_send(Ok(3)));
    assert_eq!(delivery.bytes, 2);
    assert!(!delivery.record_send(Err(io::Error::from(io::ErrorKind::PermissionDenied))));
    assert_eq!(delivery.failure, Some("udp_send_failed"));
    assert_eq!(delivery.discarded_datagrams, 1);
    assert_eq!(delivery.queued(), 0);
    assert_eq!(delivery.bytes, 0);
}

#[test]
fn reservation_bounds_and_per_stream_send_budget_are_enforced() {
    let peer = UdpSocket::bind("127.0.0.1:0").unwrap();
    let local = "127.0.0.1:0".parse().unwrap();
    let addr = peer.local_addr().unwrap();
    let mut delivery = Delivery::bind(local, local, addr, addr).unwrap();
    assert!(!delivery.can_queue(MAX_DATAGRAMS + 1, 1));
    assert!(!delivery.can_queue(1, MAX_QUEUE_BYTES + 1));
    delivery.push(Medium::Audio, vec![vec![1]; 32]);
    delivery.tick();
    assert_eq!(delivery.sent_datagrams, 16);
    assert_eq!(delivery.queued(), 16);
}

#[test]
fn configured_video_batch_size_changes_the_per_frame_send_allowance() {
    for (batch_size_kb, expected_sends) in [(16, 16), (32, 32), (64, 64)] {
        let peer = UdpSocket::bind("127.0.0.1:0").unwrap();
        let local = "127.0.0.1:0".parse().unwrap();
        let address = peer.local_addr().unwrap();
        let mut delivery = Delivery::bind(local, local, address, address).unwrap();
        delivery.set_video_max_batch_size_kb(batch_size_kb);
        delivery.push_video(1, vec![vec![0; 1024]; 72]);

        let sent = delivery.tick_with_limit(MAX_VIDEO_BATCH_PACKETS);

        assert_eq!(sent, expected_sends);
        assert_eq!(delivery.sent_datagrams, expected_sends as u64);
        assert_eq!(delivery.queued(), 72 - expected_sends);
    }
}

#[test]
fn video_batch_allowance_resets_at_frame_boundaries() {
    let peer = UdpSocket::bind("127.0.0.1:0").unwrap();
    let local = "127.0.0.1:0".parse().unwrap();
    let address = peer.local_addr().unwrap();
    let mut delivery = Delivery::bind(local, local, address, address).unwrap();
    delivery.set_video_max_batch_size_kb(16);
    delivery.push_video(1, vec![vec![1; 1024]; 8]);
    delivery.push_video(2, vec![vec![2; 1024]; 8]);

    assert_eq!(delivery.tick_with_limit(MAX_VIDEO_BATCH_PACKETS), 16);
    assert_eq!(delivery.queued(), 0);
}

#[test]
fn prolonged_queue_stall_fails_visibly_without_a_blocking_wait() {
    let mut delivery = delivery();
    delivery.push(Medium::Video, vec![vec![1, 2, 3]]);
    delivery.last_progress = Instant::now() - Duration::from_secs(6);
    delivery.tick();
    assert_eq!(delivery.failure, Some("udp_stalled"));
    assert_eq!(delivery.discarded_datagrams, 1);
    assert_eq!(delivery.queued(), 0);
}

#[test]
fn exact_lane_pings_freeze_source_ports_and_rebinding_cannot_redirect_media() {
    let mut delivery = Delivery::discover(
        "127.0.0.1:0".parse().unwrap(),
        "127.0.0.1:0".parse().unwrap(),
        "127.0.0.1".parse().unwrap(),
        DiscoveryTokens {
            video: VIDEO_TOKEN,
            audio: AUDIO_TOKEN,
        },
        Duration::from_secs(10),
    )
    .unwrap();
    let (video_local, audio_local) = delivery.addresses().unwrap();
    let video = sender();
    let audio = sender();
    let rebinding = sender();

    video.send_to(b"short", video_local).unwrap();
    video.send_to(&[0x55; 128], video_local).unwrap();
    video.send_to(&ping(AUDIO_TOKEN, 1), video_local).unwrap();
    video.send_to(&ping(VIDEO_TOKEN, 0), video_local).unwrap();
    video.send_to(&ping(VIDEO_TOKEN, 1), video_local).unwrap();
    video.send_to(&ping(VIDEO_TOKEN, 1), audio_local).unwrap();
    for _ in 0..20 {
        delivery.tick();
        if delivery.peer_port(Medium::Video).is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }

    assert!(!delivery.ready());
    assert_eq!(
        delivery.peer_port(Medium::Video),
        Some(video.local_addr().unwrap().port())
    );
    assert_eq!(delivery.peer_port(Medium::Audio), None);

    audio.send_to(&ping(AUDIO_TOKEN, 1), audio_local).unwrap();
    for _ in 0..20 {
        delivery.tick();
        if delivery.ready() {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(delivery.ready());
    assert_eq!(
        delivery.peer_port(Medium::Audio),
        Some(audio.local_addr().unwrap().port())
    );

    rebinding
        .send_to(&ping(VIDEO_TOKEN, 2), video_local)
        .unwrap();
    delivery.push(Medium::Video, vec![vec![1, 2, 3]]);
    delivery.push(Medium::Audio, vec![vec![4, 5]]);
    delivery.tick();
    assert_eq!(
        delivery.peer_port(Medium::Video),
        Some(video.local_addr().unwrap().port())
    );
    let mut bytes = [0; 8];
    assert_eq!(video.recv(&mut bytes).unwrap(), 3);
    assert_eq!(&bytes[..3], &[1, 2, 3]);
    assert_eq!(audio.recv(&mut bytes).unwrap(), 2);
    assert_eq!(&bytes[..2], &[4, 5]);
    assert!(rebinding.recv(&mut bytes).is_err());
}

#[test]
fn expired_discovery_rejects_already_queued_valid_pings() {
    let mut delivery = Delivery::discover(
        "127.0.0.1:0".parse().unwrap(),
        "127.0.0.1:0".parse().unwrap(),
        "127.0.0.1".parse().unwrap(),
        DiscoveryTokens {
            video: VIDEO_TOKEN,
            audio: AUDIO_TOKEN,
        },
        Duration::from_secs(10),
    )
    .unwrap();
    let (video_local, audio_local) = delivery.addresses().unwrap();
    let video = sender();
    let audio = sender();
    video.send_to(&ping(VIDEO_TOKEN, 1), video_local).unwrap();
    audio.send_to(&ping(AUDIO_TOKEN, 1), audio_local).unwrap();
    delivery.discovery_deadline = Some(Instant::now() - Duration::from_millis(1));

    delivery.tick();

    assert!(!delivery.ready());
    assert_eq!(delivery.failure, Some("udp_discovery_timeout"));
    assert_eq!(delivery.peer_port(Medium::Video), None);
    assert_eq!(delivery.peer_port(Medium::Audio), None);
}

#[test]
fn valid_payload_from_a_different_ip_is_ignored() {
    let mut delivery = Delivery::discover(
        "127.0.0.1:0".parse().unwrap(),
        "127.0.0.1:0".parse().unwrap(),
        "127.0.0.2".parse().unwrap(),
        DiscoveryTokens {
            video: VIDEO_TOKEN,
            audio: AUDIO_TOKEN,
        },
        Duration::from_secs(10),
    )
    .unwrap();
    let (video_local, _) = delivery.addresses().unwrap();
    let wrong_ip = sender();
    wrong_ip
        .send_to(&ping(VIDEO_TOKEN, 1), video_local)
        .unwrap();
    for _ in 0..5 {
        delivery.tick();
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(delivery.peer_port(Medium::Video), None);
    assert_eq!(delivery.failure, None);
}

#[test]
fn receive_work_per_tick_is_bounded() {
    let mut delivery = Delivery::discover(
        "127.0.0.1:0".parse().unwrap(),
        "127.0.0.1:0".parse().unwrap(),
        "127.0.0.1".parse().unwrap(),
        DiscoveryTokens {
            video: VIDEO_TOKEN,
            audio: AUDIO_TOKEN,
        },
        Duration::from_secs(10),
    )
    .unwrap();
    let (video_local, _) = delivery.addresses().unwrap();
    let video = sender();
    for _ in 0..RECEIVES_PER_LANE {
        video.send_to(b"bad", video_local).unwrap();
    }
    video.send_to(&ping(VIDEO_TOKEN, 1), video_local).unwrap();

    delivery.tick();
    assert_eq!(delivery.peer_port(Medium::Video), None);
    delivery.tick();
    assert_eq!(
        delivery.peer_port(Medium::Video),
        Some(video.local_addr().unwrap().port())
    );
}
