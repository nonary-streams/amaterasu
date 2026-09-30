use super::*;
use std::{
    io,
    net::TcpListener,
    process::Command,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

#[test]
fn unresponsive_bus_probe_child() {
    if std::env::var_os("AMATERASU_PIPEWIRE_DEADLINE_CHILD").is_none() {
        return;
    }
    let started = Instant::now();
    let backend = NativeBackend::new();
    assert!(!backend.availability().available);
    assert!(backend.availability().native_supported);
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "session bus handshake was not bounded"
    );
    // Start a worker against the same silent bus, then cancel authentication.
    // No connection can reach CreateSession or invoke a picker in this test.
    let selection = backend.select(SourceType::Monitor, 5).unwrap();
    thread::sleep(Duration::from_millis(100));
    let cancel_started = Instant::now();
    drop(selection);
    assert!(cancel_started.elapsed() < Duration::from_secs(1));
}

#[test]
fn session_bus_handshake_is_bounded_when_loopback_peer_accepts_but_never_authenticates() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let (stop, stopped) = mpsc::channel();
    let server = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut connections = Vec::new();
        loop {
            // Hold every accepted socket open until the child has exited or
            // exceeded its deadline. An unbounded handshake cannot accidentally
            // pass because this fake peer closes first.
            if stopped.try_recv().is_ok() || Instant::now() >= deadline {
                return;
            }
            match listener.accept() {
                Ok((connection, _)) => {
                    connections.push(connection);
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("loopback fake bus failed: {error}"),
            }
        }
    });
    // Isolate environment changes in a child test process; other unit tests must
    // keep their current-user session bus and must never invoke a picker here.
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "backend::platform::tests::unresponsive_bus_probe_child",
            "--nocapture",
        ])
        .env("AMATERASU_PIPEWIRE_DEADLINE_CHILD", "1")
        .env(
            "DBUS_SESSION_BUS_ADDRESS",
            format!("tcp:host=127.0.0.1,port={}", address.port()),
        )
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(4);
    let result = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break Some(status);
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            break None;
        }
        thread::sleep(Duration::from_millis(10));
    };
    stop.send(()).unwrap();
    server.join().unwrap();
    assert!(
        result.is_some_and(|status| status.success()),
        "private session bus probe exceeded deadline"
    );
}

#[test]
fn memfd_extent_is_checked_without_mapping_or_reading_pixels() {
    use std::{fs::File, os::fd::AsRawFd};
    unsafe extern "C" {
        fn am_pw_memfd_extent_valid(fd: i64, map_offset: u32, max_size: u32) -> i32;
    }
    struct TemporaryFile(std::path::PathBuf);
    impl Drop for TemporaryFile {
        fn drop(&mut self) {
            std::fs::remove_file(&self.0).unwrap();
        }
    }
    let temporary = TemporaryFile(std::env::temp_dir().join(format!(
        "amaterasu-pipewire-extent-{}-{}",
        std::process::id(),
        crate::model::new_id().unwrap()
    )));
    let file = File::create_new(&temporary.0).unwrap();
    file.set_len(8192).unwrap();
    let fd = i64::from(file.as_raw_fd());
    // SAFETY: the helper only fstats this live borrowed descriptor; it never
    // maps or reads file contents and never closes the descriptor.
    unsafe {
        assert_eq!(am_pw_memfd_extent_valid(fd, 4096, 4096), 1);
        assert_eq!(am_pw_memfd_extent_valid(fd, 4096, 4097), 0);
        assert_eq!(am_pw_memfd_extent_valid(fd, u32::MAX, u32::MAX), 0);
        assert_eq!(am_pw_memfd_extent_valid(fd, 0, 0), 0);
        assert_eq!(am_pw_memfd_extent_valid(-1, 0, 1), 0);
        assert_eq!(am_pw_memfd_extent_valid(i64::from(i32::MAX) + 1, 0, 1), 0);
        file.set_len(4095).unwrap();
        assert_eq!(am_pw_memfd_extent_valid(fd, 0, 4096), 0);
    }
    // A descriptor previously checked by the helper remains owned and usable.
    file.set_len(8192).unwrap();
}

#[test]
fn adapter_callback_copies_pixels_into_the_stable_mailbox_allocation() {
    let mailbox = Arc::new(Mailbox::default());
    let mut bytes = [42u8; 16];
    let plane = Plane {
        data: bytes.as_ptr(),
        memory_type: 1,
        flags: 1,
        max_size: 16,
        chunk_offset: 0,
        chunk_size: 16,
        stride: 8,
        chunk_flags: 0,
        plane_count: 1,
        fd: -1,
        map_offset: 0,
    };
    // SAFETY: this test retains both the Arc allocation and initialized plane
    // bytes throughout the exact private callback boundary used by the worker.
    assert_eq!(
        unsafe {
            frame_callback(
                Arc::as_ptr(&mailbox).cast_mut().cast(),
                &plane,
                2,
                2,
                1,
                1000,
            )
        },
        1
    );
    bytes.fill(99);
    let delivery = mailbox.claim().unwrap().unwrap();
    assert_eq!(delivery.frame.bytes, [42u8; 16]);
}
