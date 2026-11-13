use super::*;
use std::ffi::c_void;
use std::net::UdpSocket;

fn stream() -> Stream {
    let request = Open {
        peer_ip: "127.0.0.1".parse().unwrap(),
        peer_port: None,
        local_bind: None,
        connect_data: 7,
        key_hex: "35".repeat(16),
        ping_timeout_ms: 10_000,
        operation_id: None,
    };
    Stream::open("owner", &request, &[0x35; 16], 0).unwrap()
}

#[test]
fn actual_control_stream_uses_ipv6_loopback_and_releases_its_exact_port() {
    let request = Open {
        peer_ip: "::1".parse().unwrap(),
        peer_port: None,
        local_bind: Some("[::1]:0".parse().unwrap()),
        connect_data: 7,
        key_hex: "35".repeat(16),
        ping_timeout_ms: 10_000,
        operation_id: None,
    };
    let key = request.validate(false).unwrap();
    let stream = Stream::open("owner", &request, &key, 0).unwrap();
    let local = stream.local;
    assert_eq!(local.ip(), request.peer_ip);
    assert_ne!(local.port(), 0);
    let ipv4_same_port =
        UdpSocket::bind(SocketAddr::new("127.0.0.1".parse().unwrap(), local.port())).unwrap();
    assert!(amaterasu_network_policy::bind_udp(local).is_err());
    drop(stream);
    assert_eq!(
        amaterasu_network_policy::bind_udp(local)
            .unwrap()
            .local_addr()
            .unwrap(),
        local
    );
    drop(ipv4_same_port);
}

fn client() -> ControlSealer {
    ControlSealer::new(
        &[0x35; 16],
        ControlProfile::SunshineV2,
        Direction::ClientToHost,
    )
    .unwrap()
}

fn connected_peer(stream: &mut Stream) {
    use amaterasu_gamestream::enet::*;
    let connect = Datagram {
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
                data: 7,
            }),
        }],
    };
    stream
        .peer
        .receive(&encode(&connect, &Limits::default()).unwrap(), 0)
        .unwrap();
    stream.peer.pop_datagram().unwrap();
    let ack = Command {
        channel_id: CONTROL_CHANNEL,
        reliable_sequence: 1,
        flags: 0,
        body: CommandBody::Acknowledge {
            received_sequence: 1,
            received_sent_time: 0,
        },
    };
    stream.peer.receive(&peer_packet(ack), 1).unwrap();
    assert!(matches!(
        stream.peer.pop_event(),
        Some(Event::Connected { .. })
    ));
}

fn peer_packet(command: amaterasu_gamestream::enet::Command) -> Vec<u8> {
    use amaterasu_gamestream::enet::*;
    encode(
        &Datagram {
            header: Header {
                peer_id: 0,
                session_id: 0,
                sent_time: Some(1),
            },
            commands: vec![command],
        },
        &Limits::default(),
    )
    .unwrap()
}

fn reliable_packet(sequence: u16, data: Vec<u8>) -> Vec<u8> {
    use amaterasu_gamestream::enet::*;
    peer_packet(Command {
        channel_id: 0,
        reliable_sequence: sequence,
        flags: FLAG_ACKNOWLEDGE,
        body: CommandBody::SendReliable { data },
    })
}

#[test]
fn startup_backpressure_keeps_reliable_messages_and_retries_without_disconnect() {
    unsafe extern "C" fn accept(_: *mut c_void, _: *const ControlMessage) -> Status {
        STATUS_OK
    }
    let mut stream = stream();
    connected_peer(&mut stream);
    let mut client = client();
    for i in 0..MAX_MESSAGES {
        stream.accept_control(
            0,
            Delivery::Reliable,
            &client.seal(9, &(i as u32).to_le_bytes()).unwrap(),
            2,
        );
    }
    for sequence in 1..=256 {
        let data = client.seal(9, &(sequence as u32).to_le_bytes()).unwrap();
        stream
            .peer
            .receive(&reliable_packet(sequence, data), 2)
            .unwrap();
        assert!(stream.peer.pop_datagram().is_some());
    }
    stream.collect_messages(2);
    assert_eq!(stream.incoming.len(), MAX_MESSAGES);
    assert!(stream.failure.is_none());
    let retry = reliable_packet(257, client.seal(9, b"retry").unwrap());
    assert_eq!(stream.peer.receive(&retry, 3), Err(PeerError::Busy));
    assert!(stream.peer.pop_datagram().is_none());
    let sink = ControlMessageSink {
        context: std::ptr::null_mut(),
        write: Some(accept),
    };
    assert_eq!(unsafe { stream.receive(sink) }, STATUS_OK);
    stream.collect_messages(3);
    assert_eq!(stream.incoming.len(), MAX_MESSAGES);
    stream.peer.receive(&retry, 4).unwrap();
    assert!(stream.peer.pop_datagram().is_some());
    for _ in 0..MAX_MESSAGES {
        assert_eq!(unsafe { stream.receive(sink) }, STATUS_OK);
    }
    stream.collect_messages(4);
    assert_eq!(stream.incoming.len(), 256);
    assert_eq!(stream.incoming.back().unwrap().bytes, b"retry");
    assert!(stream.failure.is_none());
}

#[test]
fn byte_limit_backpressure_resumes_without_consuming_the_peer_event() {
    unsafe extern "C" fn accept(_: *mut c_void, _: *const ControlMessage) -> Status {
        STATUS_OK
    }
    let mut stream = stream();
    connected_peer(&mut stream);
    let mut client = client();
    let large = vec![7; MAX_CONTROL_PAYLOAD_BYTES];
    for _ in 0..16 {
        stream.accept_control(0, Delivery::Reliable, &client.seal(9, &large).unwrap(), 2);
    }
    assert!(stream.incoming_bytes > MAX_MESSAGE_BYTES - MAX_CONTROL_PAYLOAD_BYTES);
    let packet = reliable_packet(1, client.seal(9, b"after-large").unwrap());
    stream.peer.receive(&packet, 2).unwrap();
    stream.collect_messages(2);
    assert_eq!(stream.incoming.len(), 16);
    assert!(stream.failure.is_none());
    let sink = ControlMessageSink {
        context: std::ptr::null_mut(),
        write: Some(accept),
    };
    assert_eq!(unsafe { stream.receive(sink) }, STATUS_OK);
    stream.collect_messages(3);
    assert_eq!(stream.incoming.back().unwrap().bytes, b"after-large");
    assert!(stream.failure.is_none());
}

#[test]
fn timing_preserves_rejected_messages_and_resets_only_observations() {
    unsafe extern "C" fn busy(_: *mut c_void, _: *const ControlMessage) -> Status {
        MEDIA_BUSY
    }
    unsafe extern "C" fn accept(_: *mut c_void, _: *const ControlMessage) -> Status {
        STATUS_OK
    }
    let mut stream = stream();
    let encrypted = client().seal(9, b"private-input").unwrap();
    stream.accept_control(2, Delivery::Reliable, &encrypted, 1);
    stream.incoming.front_mut().unwrap().queued_at =
        Instant::now() - std::time::Duration::from_millis(25);
    let sink = ControlMessageSink {
        context: std::ptr::null_mut(),
        write: Some(busy),
    };
    assert_eq!(unsafe { stream.receive(sink) }, MEDIA_BUSY);
    let pending = stream.take_timing(1);
    assert_eq!(pending["timing_us"]["queue_wait_us"]["calls"], 0);
    assert_eq!(pending["timing_us"]["consumer_retries"], 1);
    assert!(pending["oldest_queued_us"].as_u64().unwrap() >= 25_000);
    assert!(!pending.to_string().contains("private-input"));
    assert_eq!(
        unsafe {
            stream.receive(ControlMessageSink {
                write: Some(accept),
                ..sink
            })
        },
        STATUS_OK
    );
    let accepted = stream.take_timing(1);
    assert_eq!(accepted["timing_us"]["queue_wait_us"]["calls"], 1);
    assert!(
        accepted["timing_us"]["queue_wait_us"]["max_us"]
            .as_u64()
            .unwrap()
            >= 25_000
    );
    assert!(stream.incoming.is_empty());
    assert_eq!(
        stream.take_timing(1)["timing_us"]["queue_wait_us"]["calls"],
        0
    );
}

#[test]
fn rejected_ciphertext_and_replays_never_refresh_authentication_or_deliver_messages() {
    let mut stream = stream();
    let mut client = client();
    let valid = client.seal(9, b"input").unwrap();
    let mut tampered = valid.clone();
    tampered[8] ^= 1;
    stream.accept_control(2, Delivery::Reliable, &tampered, 50);
    assert_eq!(stream.authenticated_at, None);
    assert!(stream.incoming.is_empty());
    stream.accept_control(2, Delivery::Reliable, &valid, 100);
    stream.accept_control(3, Delivery::Unreliable, &valid, 200);
    stream.accept_control(2, Delivery::Reliable, &tampered, 300);
    assert_eq!(stream.authenticated_at, Some(100));
    assert_eq!(stream.incoming.len(), 1);
    assert_eq!(stream.incoming[0].bytes, b"input");
    assert_eq!(stream.rejected_messages, 3);
}

#[test]
fn count_and_byte_overflow_fail_visibly_without_retaining_a_socket_or_plaintext() {
    for payload_size in [0, 65511] {
        let mut stream = stream();
        let address = stream.local;
        let mut client = client();
        let payload = vec![7; payload_size];
        for sequence in 0..=MAX_MESSAGES {
            stream.accept_control(
                2,
                Delivery::Reliable,
                &client.seal(9, &payload).unwrap(),
                sequence as u64,
            );
            if stream.failure.is_some() {
                break;
            }
        }
        assert_eq!(stream.failure, Some("consumer_queue_full"));
        assert!(stream.incoming.is_empty());
        assert_eq!(stream.incoming_bytes, 0);
        assert!(UdpSocket::bind(address).is_ok());
    }
}

#[test]
fn oversized_or_unauthenticated_udp_does_not_extend_the_admission_deadline() {
    let mut stream = stream();
    let address = stream.local;
    let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
    sender.send_to(&vec![0; 8000], address).unwrap();
    let receiver = stream.socket.as_ref().unwrap();
    receiver.set_nonblocking(false).unwrap();
    receiver
        .set_read_timeout(Some(std::time::Duration::from_secs(1)))
        .unwrap();
    receiver.peek(&mut [0; 8000]).unwrap();
    receiver.set_nonblocking(true).unwrap();
    stream.tick(9999);
    assert_eq!(stream.failure, None);
    assert!(stream.rejected_datagrams > 0);
    stream.tick(10_000);
    assert_eq!(stream.failure, Some("authentication_timeout"));
    assert!(stream.socket.is_none());
    assert!(UdpSocket::bind(address).is_ok());
}

fn stream_with_ping_timeout(ping_timeout_ms: i64) -> Stream {
    let request = Open {
        peer_ip: "127.0.0.1".parse().unwrap(),
        peer_port: None,
        local_bind: None,
        connect_data: 7,
        key_hex: "35".repeat(16),
        ping_timeout_ms,
        operation_id: None,
    };
    Stream::open("owner", &request, &[0x35; 16], 0).unwrap()
}

#[test]
fn configured_ping_timeout_applies_to_authentication_and_authenticated_idle() {
    let mut awaiting_authentication = stream_with_ping_timeout(250);
    awaiting_authentication.tick(249);
    assert_eq!(awaiting_authentication.failure, None);
    awaiting_authentication.tick(250);
    assert_eq!(
        awaiting_authentication.failure,
        Some("authentication_timeout")
    );

    let mut authenticated = stream_with_ping_timeout(250);
    authenticated.accept_control(
        2,
        Delivery::Reliable,
        &client().seal(3, b"keyboard").unwrap(),
        400,
    );
    authenticated.check_timeout(649);
    assert_eq!(authenticated.failure, None);
    authenticated.check_timeout(650);
    assert_eq!(authenticated.failure, Some("authenticated_peer_idle"));
}

#[test]
fn moonlight_encrypted_periodic_ping_refreshes_authenticated_idle_deadline() {
    let mut stream = stream_with_ping_timeout(250);
    let mut client = client();

    for now in [100, 200, 300, 400] {
        // Moonlight's v7 control stream sends encrypted type 0x0200 every
        // 100 ms. These are authenticated activity even when they carry no input.
        let ping = client.seal(0x0200, &[4, 0, 0, 0, 0, 0, 0, 0]).unwrap();
        stream.accept_control(0, Delivery::Reliable, &ping, now);
        stream.check_timeout(now + 249);
        assert_eq!(stream.failure, None);
    }
    assert_eq!(stream.authenticated_at, Some(400));
    assert_eq!(stream.received_messages, 4);
}

#[test]
fn terminal_failure_logs_only_the_first_fixed_internal_code() {
    let mut stream = stream();
    stream.fail("peer_disconnected");
    stream.fail("udp_receive_failed");

    assert_eq!(stream.failure, Some("peer_disconnected"));
    assert_eq!(stream.metadata(7)["failure"], "peer_disconnected");
    assert_eq!(
        peer_poll_failure(PeerError::ClockRegression),
        "peer_poll_clock_regression"
    );
    assert_eq!(
        peer_enqueue_failure(PeerError::Busy),
        "peer_enqueue_busy_after_admission"
    );
}

#[test]
fn zero_ping_timeout_expires_immediately_and_default_is_ten_seconds() {
    let mut immediate = stream_with_ping_timeout(0);
    immediate.tick(0);
    assert_eq!(immediate.failure, Some("authentication_timeout"));
    let default_stream = stream();
    assert_eq!(default_stream.ping_timeout_ms, 10_000);
    assert_eq!(default_stream.metadata(7)["ping_timeout_ms"], 10_000);
}

#[derive(Default)]
struct Sink {
    calls: usize,
    reject: bool,
    bytes: Vec<u8>,
    sequence: u64,
}

unsafe extern "C" fn receive(context: *mut c_void, message: *const ControlMessage) -> Status {
    // SAFETY: test supplies initialized stack context; stream supplies borrowed
    // header and initialized queue bytes for this synchronous callback only.
    let sink = unsafe { &mut *context.cast::<Sink>() };
    let message = unsafe { &*message };
    sink.calls += 1;
    sink.bytes = unsafe { message.bytes().unwrap() }.to_vec();
    sink.sequence = message.sequence;
    if sink.reject { MEDIA_BUSY } else { STATUS_OK }
}

#[test]
fn rejected_native_sink_retains_identical_authenticated_payload() {
    let mut stream = stream();
    stream.accept_control(
        2,
        Delivery::Reliable,
        &client().seal(3, b"keyboard").unwrap(),
        0,
    );
    let mut sink = Sink {
        reject: true,
        ..Sink::default()
    };
    let callback = ControlMessageSink {
        context: (&mut sink as *mut Sink).cast(),
        write: Some(receive),
    };
    // SAFETY: live stack sink is borrowed synchronously for each call.
    assert_eq!(unsafe { stream.receive(callback) }, MEDIA_BUSY);
    assert_eq!(stream.incoming_bytes, 8);
    assert_eq!(sink.calls, 1);
    assert_eq!(sink.bytes, b"keyboard");
    sink.reject = false;
    assert_eq!(unsafe { stream.receive(callback) }, STATUS_OK);
    assert_eq!(sink.calls, 2);
    assert_eq!(sink.bytes, b"keyboard");
    assert_eq!(sink.sequence, 0);
    assert_eq!(stream.incoming_bytes, 0);
    assert_eq!(unsafe { stream.receive(callback) }, MEDIA_PENDING);
    assert_eq!(sink.calls, 2);
}
