use super::*;
use std::ffi::c_void;

fn stream() -> Stream {
    let request = Open {
        peer_ip: "127.0.0.1".parse().unwrap(),
        peer_port: None,
        local_bind: None,
        connect_data: 7,
        key_hex: "35".repeat(16),
        operation_id: None,
    };
    Stream::open("owner", &request, &[0x35; 16], 0).unwrap()
}

fn client() -> ControlSealer {
    ControlSealer::new(
        &[0x35; 16],
        ControlProfile::SunshineV2,
        Direction::ClientToHost,
    )
    .unwrap()
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
    receiver.peek_from(&mut [0; 8000]).unwrap();
    receiver.set_nonblocking(true).unwrap();
    stream.tick(4999);
    assert_eq!(stream.failure, None);
    assert!(stream.rejected_datagrams > 0);
    stream.tick(5000);
    assert!(stream.failure.is_some());
    assert!(stream.socket.is_none());
    assert!(UdpSocket::bind(address).is_ok());
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
