use super::*;
use std::net::UdpSocket;

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
fn timing_counts_only_accepted_datagrams_and_preserves_pending_age() {
    let mut delivery = delivery();
    delivery.push(Medium::Video, vec![vec![1; 8], vec![2; 8]]);
    delivery.push(Medium::Audio, vec![vec![3; 4]]);
    for packet in &mut delivery.queue {
        packet.queued_at = Instant::now() - Duration::from_millis(20);
    }
    assert!(!delivery.record_batch_send(2, 16, Err(io::ErrorKind::WouldBlock.into())));
    let pending = delivery.take_timing();
    assert_eq!(pending["timing_us"]["video_queue_wait_us"]["calls"], 0);
    assert_eq!(pending["timing_us"]["send_retries"], 1);
    assert_eq!(pending["timing_us"]["queue_high_water_datagrams"], 3);
    assert!(pending["oldest_queued_us"].as_u64().unwrap() >= 20_000);
    assert!(delivery.record_batch_send(2, 16, Ok(16)));
    assert!(delivery.record_send(Ok(4)));
    let accepted = delivery.take_timing();
    assert_eq!(accepted["timing_us"]["video_queue_wait_us"]["calls"], 2);
    assert_eq!(accepted["timing_us"]["audio_queue_wait_us"]["calls"], 1);
    assert!(
        accepted["timing_us"]["video_queue_wait_us"]["max_us"]
            .as_u64()
            .unwrap()
            >= 20_000
    );
    assert_eq!(accepted["queued_datagrams"], 0);
    assert_eq!(
        delivery.take_timing()["timing_us"]["video_queue_wait_us"]["calls"],
        0
    );
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
fn configured_video_batch_size_limits_calls_without_deferring_the_frame() {
    for (batch_size_kb, packet_bytes) in [(16, 1024), (32, 1024), (64, 1024), (64, 1440)] {
        let mut delivery = delivery();
        delivery.set_video_max_batch_size_kb(batch_size_kb);
        delivery.push_video(1, vec![vec![0; packet_bytes]; 72]);
        let mut batches = Vec::new();

        let sent = delivery.tick_with_batch_sender(64, |_, payload, segment_size| {
            assert_eq!(segment_size, packet_bytes);
            assert!(payload.len() <= usize::from(batch_size_kb) * 1024);
            batches.push(payload.len());
            Ok(payload.len())
        });

        assert_eq!(sent, 64, "batch size must not reduce the pass allowance");
        assert_eq!(delivery.sent_datagrams, 64);
        assert_eq!(delivery.queued(), 8, "the shared pass budget still applies");
        assert_eq!(
            batches.len(),
            64_usize.div_ceil(usize::from(batch_size_kb) * 1024 / packet_bytes)
        );
    }
}

#[test]
fn video_grouping_requires_equal_payload_sizes_and_the_same_frame() {
    let mut delivery = delivery();
    delivery.push_video(1, vec![vec![1; 4], vec![2; 4], vec![3; 2], vec![4; 4]]);
    delivery.push_video(2, vec![vec![5; 4]]);

    assert_eq!(delivery.video_group_len(Some(1), 4, 64), 2);
    assert_eq!(delivery.video_group_payload(2, 4), [1, 1, 1, 1, 2, 2, 2, 2]);
}

#[test]
fn unavailable_segmentation_falls_back_to_individual_ordered_sends() {
    let peer = sender();
    let local = "127.0.0.1:0".parse().unwrap();
    let address = peer.local_addr().unwrap();
    let mut delivery = Delivery::bind(local, local, address, address).unwrap();
    delivery.push_video(1, vec![vec![1; 4], vec![2; 4], vec![3; 4]]);
    let mut batch_attempts = 0;

    let sent = delivery.tick_with_batch_sender(64, |_, _, _| {
        batch_attempts += 1;
        Err(BatchError::Unsupported)
    });

    assert_eq!(batch_attempts, 1);
    assert_eq!(sent, 3);
    assert_eq!(delivery.video_batch_support, VideoBatchSupport::Unavailable);
    for expected in [1, 2, 3] {
        let mut bytes = [0; 8];
        let length = peer.recv(&mut bytes).unwrap();
        assert_eq!(&bytes[..length], &[expected; 4]);
    }
    assert_eq!(delivery.queued(), 0);
}

#[test]
fn transient_batch_send_failure_retains_the_entire_confirmed_ordered_group() {
    let mut delivery = delivery();
    delivery.push_video(1, vec![vec![1; 4], vec![2; 4], vec![3; 4]]);
    let queued_bytes = delivery.queued_bytes();

    let sent = delivery.tick_with_batch_sender(64, |_, _, _| Err(BatchError::Retry));

    assert_eq!(sent, 0);
    assert_eq!(delivery.queued(), 3);
    assert_eq!(delivery.queued_bytes(), queued_bytes);
    assert_eq!(delivery.sent_datagrams, 0);
    assert_eq!(delivery.failure, None);
    assert_eq!(delivery.take_timing()["timing_us"]["send_retries"], 1);
}

#[test]
fn batch_dequeues_only_after_full_byte_count_is_confirmed() {
    let mut delivery = delivery();
    delivery.push_video(1, vec![vec![1; 4], vec![2; 4], vec![3; 4]]);
    let mut submitted = Vec::new();

    let sent = delivery.tick_with_batch_sender(64, |_, payload, segment_size| {
        assert_eq!(segment_size, 4);
        submitted.extend_from_slice(payload);
        Ok(payload.len())
    });

    assert_eq!(sent, 3);
    assert_eq!(
        submitted,
        [1; 4]
            .into_iter()
            .chain([2; 4])
            .chain([3; 4])
            .collect::<Vec<_>>()
    );
    assert_eq!(delivery.queued(), 0);
    assert_eq!(delivery.sent_datagrams, 3);
}

#[test]
fn flush_sends_a_frame_larger_than_the_tick_budget_in_one_call() {
    let mut delivery = delivery();
    delivery.push_video(1, vec![vec![7; 1024]; 300]);
    let mut batches = 0;

    let sent = delivery.flush_with_batch_sender(|_, payload, _| {
        batches += 1;
        Ok(payload.len())
    });

    assert_eq!(sent, 300);
    assert_eq!(delivery.queued(), 0);
    assert!(batches > 1, "each provider call stays bounded");
}

#[test]
fn tick_keeps_its_budget_for_datagrams_a_flush_could_not_send() {
    let mut delivery = delivery();
    delivery.push_video(1, vec![vec![7; 1024]; 300]);

    let sent = delivery.tick_with_batch_sender(64, |_, payload, _| Ok(payload.len()));

    assert_eq!(sent, 64);
    assert_eq!(delivery.queued(), 236);
}

#[test]
fn flush_keeps_the_refused_suffix_queued_in_order() {
    let mut delivery = delivery();
    delivery.push_video(1, vec![vec![1; 4], vec![2; 4], vec![3; 4]]);

    let sent = delivery.flush_with_batch_sender(|_, _, segment_size| Ok(segment_size));

    assert_eq!(sent, 1);
    assert_eq!(delivery.queued(), 2);
    assert_eq!(delivery.failure, None);
    let sent = delivery.tick_with_batch_sender(64, |_, payload, _| {
        assert_eq!(payload, [vec![2; 4], vec![3; 4]].concat());
        Ok(payload.len())
    });
    assert_eq!(sent, 2);
    assert_eq!(delivery.queued(), 0);
}

#[test]
fn flush_retry_leaves_the_frame_queued_without_failing_the_stream() {
    let mut delivery = delivery();
    delivery.push_video(1, vec![vec![1; 4], vec![2; 4]]);

    let sent = delivery.flush_with_batch_sender(|_, _, _| Err(BatchError::Retry));

    assert_eq!(sent, 0);
    assert_eq!(delivery.queued(), 2);
    assert_eq!(delivery.failure, None);
}

#[test]
fn flush_before_peer_discovery_keeps_datagrams_and_never_touches_the_socket() {
    let local = "127.0.0.1:0".parse().unwrap();
    let mut delivery = Delivery::discover(
        local,
        local,
        "127.0.0.1".parse().unwrap(),
        DiscoveryTokens {
            video: VIDEO_TOKEN,
            audio: AUDIO_TOKEN,
        },
        Duration::from_secs(5),
    )
    .unwrap();
    delivery.push_video(1, vec![vec![1; 4]]);

    let sent = delivery.flush_with_batch_sender(|_, _, _| panic!("no peer to send to"));

    assert_eq!(sent, 0);
    assert_eq!(delivery.queued(), 1);
}

#[test]
fn partial_batch_send_result_fails_without_counting_any_datagrams_as_sent() {
    let mut delivery = delivery();
    delivery.push_video(1, vec![vec![1; 4], vec![2; 4], vec![3; 4]]);
    let queued_bytes = delivery.queued_bytes();

    let sent =
        delivery.tick_with_batch_sender(64, |_, payload, _| Ok(payload.len().saturating_sub(1)));

    assert_eq!(sent, 0);
    assert_eq!(delivery.failure, Some("udp_send_failed"));
    assert_eq!(delivery.sent_datagrams, 0);
    assert_eq!(delivery.discarded_datagrams, 3);
    assert_eq!(delivery.queued(), 0);
    assert_eq!(delivery.bytes, 0);
    assert!(queued_bytes > 0);
}

#[test]
fn video_batches_preserve_frame_boundaries() {
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

#[test]
fn provider_partial_batch_acceptance_retries_only_unsent_suffix() {
    let mut delivery = delivery();
    delivery.push_video(1, vec![vec![1; 4], vec![2; 4], vec![3; 4]]);
    let sent = delivery.tick_with_batch_sender(64, |_, payload, segment_size| {
        assert_eq!(payload, [vec![1; 4], vec![2; 4], vec![3; 4]].concat());
        Ok(segment_size)
    });
    assert_eq!(sent, 1);
    assert_eq!(delivery.sent_datagrams, 1);
    assert_eq!(delivery.queued(), 2);
    assert_eq!(delivery.queued_bytes(), 8);
    assert_eq!(delivery.failure, None);
    assert_eq!(
        delivery.take_timing()["timing_us"]["partial_batch_sends"],
        1
    );
    let sent = delivery.tick_with_batch_sender(64, |_, payload, _| {
        assert_eq!(payload, [vec![2; 4], vec![3; 4]].concat());
        Ok(payload.len())
    });
    assert_eq!(sent, 2);
    assert_eq!(delivery.sent_datagrams, 3);
    assert_eq!(delivery.queued(), 0);
}

#[test]
fn provider_with_one_datagram_batch_limit_preserves_ordered_delivery() {
    use amaterasu_module_sdk::abi::{STATUS_INVALID_ARGUMENT, STATUS_OK, Status};
    use amaterasu_module_sdk::network::abi;
    use std::ffi::c_void;
    unsafe extern "C" fn capabilities(_: *mut c_void, output: *mut abi::Capabilities) -> Status {
        if output.is_null() {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: SDK supplies writable capability storage. This adapter offers
        // a valid provider contract with a smaller optional batch limit.
        unsafe {
            *output = abi::Capabilities {
                size: size_of::<abi::Capabilities>() as u32,
                version: 1,
                features: abi::UDP | abi::BATCH,
                max_io_bytes: 65_535,
                max_handles: 1024,
                max_batch: 1,
            }
        };
        STATUS_OK
    }
    let factory = amaterasu_network_sockets::Factory::new();
    // SAFETY: factory and provider code outlive all native calls and the owned
    // binding. The adapter copies only C function pointers/context, never Rust
    // allocator objects. Its table remains boxed until after Network release.
    let original = unsafe {
        amaterasu_network_sockets::bind_standalone(&factory, "media.batch-test").unwrap()
    };
    let mut adapter = Box::new(unsafe { std::ptr::read(original) });
    adapter.capabilities = capabilities;
    let network = unsafe { Network::from_native_table(&*adapter).unwrap() };
    let peer = sender();
    let address = peer.local_addr().unwrap();
    let local = "127.0.0.1:0".parse().unwrap();
    let mut delivery =
        Delivery::bind_with_network(&network, local, local, address, address).unwrap();
    delivery.push_video(1, vec![vec![1; 4], vec![2; 4], vec![3; 4]]);
    let mut buffer = [0; 16];
    assert_eq!(delivery.tick_with_limit(64), 3);
    for expected in [1, 2, 3] {
        let length = peer.recv(&mut buffer).unwrap();
        assert_eq!(&buffer[..length], &[expected; 4]);
        assert_eq!(delivery.failure, None);
    }
    assert_eq!(delivery.sent_datagrams, 3);
    assert_eq!(delivery.queued(), 0);
    drop(delivery);
    // Final Network release calls the original provider's release callback;
    // it owns the original binding. The adapter's separate table box then drops.
    drop(network);
    drop(adapter);
    drop(factory);
}
