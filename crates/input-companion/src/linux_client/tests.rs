use super::*;
use serde_json::{Value, json};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    mpsc,
};
use std::time::Duration;

fn socket_test_directory() -> PathBuf {
    static NEXT_ID: AtomicUsize = AtomicUsize::new(0);
    let path = std::env::temp_dir().join(format!(
        "amaterasu-input-reply-{}-{}",
        std::process::id(),
        NEXT_ID.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir(&path).unwrap();
    path
}

fn read_kind(reader: &mut BufReader<UnixStream>, timeout: Duration) -> Option<String> {
    reader.get_ref().set_read_timeout(Some(timeout)).unwrap();
    let mut frame = Vec::new();
    match reader.read_until(b'\n', &mut frame) {
        Ok(length) => {
            assert!(length != 0 && frame.last() == Some(&b'\n'));
            let value: Value = serde_json::from_slice(&frame).unwrap();
            Some(value["kind"].as_str().unwrap().to_owned())
        }
        Err(error)
            if frame.is_empty()
                && matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
        {
            None
        }
        Err(error) => panic!("unexpected fixture read failure: {error}"),
    }
}

fn write_reply(reader: &mut BufReader<UnixStream>, reply: Value) {
    serde_json::to_writer(reader.get_mut(), &reply).unwrap();
    reader.get_mut().write_all(b"\n").unwrap();
}

fn acknowledge_start(reader: &mut BufReader<UnixStream>) {
    assert_eq!(
        read_kind(reader, Duration::from_secs(2)).as_deref(),
        Some("worker_start")
    );
    write_reply(
        reader,
        json!({
            "kind": "granted", "generation": 1,
            "token": "0123456789abcdef0123456789abcdef", "gamepad": false,
        }),
    );
}

fn short_timeout_client(stream: UnixStream) -> LinuxInputClient {
    stream.set_nonblocking(true).unwrap();
    let transport = UnixWorkerTransport {
        stream: BufReader::new(stream),
        pending: Vec::new(),
    };
    LinuxInputClient {
        connection: WorkerClient::connect(transport, Duration::from_millis(200)).unwrap(),
    }
}

#[test]
fn late_event_reply_is_not_close_confirmation() {
    let directory = socket_test_directory();
    let path = directory.join("reply-order.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let (release_event_reply, event_reply_released) = mpsc::channel();
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream);
        acknowledge_start(&mut reader);
        assert_eq!(
            read_kind(&mut reader, Duration::from_secs(2)).as_deref(),
            Some("worker_event")
        );
        event_reply_released.recv().unwrap();
        write_reply(&mut reader, json!({"kind": "ok"}));
        assert_eq!(
            read_kind(&mut reader, Duration::from_secs(2)).as_deref(),
            Some("worker_close")
        );
        write_reply(&mut reader, json!({"kind": "denied"}));
    });
    let mut client = LinuxInputClient::connect(&path).unwrap();
    assert_eq!(
        client.send(InputEvent::Relative { dx: 1, dy: -1 }),
        Err(InputError::Device)
    );
    release_event_reply.send(()).unwrap();
    let close = client.close();
    server.join().unwrap();
    drop(client);
    std::fs::remove_dir_all(directory).unwrap();
    assert_eq!(close, Err(InputError::Device));
}

#[test]
fn fragmented_event_reply_survives_timeout_before_close() {
    let (server_stream, client_stream) = UnixStream::pair().unwrap();
    let (continue_reply, reply_resumed) = mpsc::channel();
    let server = std::thread::spawn(move || {
        let mut reader = BufReader::new(server_stream);
        acknowledge_start(&mut reader);
        assert_eq!(
            read_kind(&mut reader, Duration::from_secs(2)).as_deref(),
            Some("worker_event")
        );
        reader.get_mut().write_all(b"{\"kind\":\"o").unwrap();
        reply_resumed.recv().unwrap();
        reader.get_mut().write_all(b"k\"}\n").unwrap();
        assert_eq!(
            read_kind(&mut reader, Duration::from_secs(2)).as_deref(),
            Some("worker_close")
        );
        write_reply(&mut reader, json!({"kind": "ok"}));
    });
    let mut client = short_timeout_client(client_stream);
    assert_eq!(
        client.send(InputEvent::Relative { dx: 1, dy: -1 }),
        Err(InputError::Device)
    );
    continue_reply.send(()).unwrap();
    assert_eq!(client.close(), Ok(()));
    server.join().unwrap();
}

#[test]
fn timed_out_unix_close_does_not_write_a_second_close() {
    let (server_stream, client_stream) = UnixStream::pair().unwrap();
    let (continue_reply, reply_resumed) = mpsc::channel();
    let (finished, client_finished) = mpsc::channel();
    let server = std::thread::spawn(move || {
        let mut reader = BufReader::new(server_stream);
        acknowledge_start(&mut reader);
        assert_eq!(
            read_kind(&mut reader, Duration::from_secs(2)).as_deref(),
            Some("worker_close")
        );
        reply_resumed.recv().unwrap();
        write_reply(&mut reader, json!({"kind": "ok"}));
        client_finished.recv().unwrap();
        assert_eq!(read_kind(&mut reader, Duration::from_millis(20)), None);
    });
    let mut client = short_timeout_client(client_stream);
    assert_eq!(client.close(), Err(InputError::Device));
    continue_reply.send(()).unwrap();
    assert_eq!(client.close(), Ok(()));
    finished.send(()).unwrap();
    server.join().unwrap();
}

#[test]
fn expired_unix_handshake_does_not_open_a_connection() {
    let directory = socket_test_directory();
    let path = directory.join("expired.sock");
    let listener = UnixListener::bind(&path).unwrap();
    listener.set_nonblocking(true).unwrap();
    assert!(matches!(
        LinuxInputClient::connect_with_deadline(&path, Instant::now()),
        Err(InputError::Device)
    ));
    assert!(
        matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
    );
    drop(listener);
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn unix_write_backpressure_cannot_renew_absolute_deadline() {
    let (_server, mut client) = UnixStream::pair().unwrap();
    client.set_nonblocking(true).unwrap();
    let bytes = [1u8; 16 * 1024];
    loop {
        match client.write(&bytes) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(error) => panic!("unexpected socket write failure: {error}"),
        }
    }
    let mut transport = UnixWorkerTransport {
        stream: BufReader::new(client),
        pending: Vec::new(),
    };
    let start = Instant::now();
    assert_eq!(
        transport.write_frame(
            b"{\"kind\":\"worker_close\"}",
            start + Duration::from_millis(20)
        ),
        Err(InputError::Device)
    );
    assert!(start.elapsed() < Duration::from_secs(1));
}
