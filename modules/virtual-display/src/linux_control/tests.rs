use super::*;
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    os::unix::{fs::DirBuilderExt, net::UnixListener},
    thread,
};

fn broker(
    messages: Vec<(&'static str, &'static str)>,
) -> (LinuxControlClient, thread::JoinHandle<()>, PathBuf) {
    let directory = std::env::temp_dir().join(format!(
        "amaterasu-libvirtualdisplay-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&directory)
        .unwrap();
    let socket = directory.join("control.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let server = thread::spawn(move || {
        for (expected, response) in messages {
            let (mut connection, _) = listener.accept().unwrap();
            connection
                .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                .unwrap();
            let mut request = String::new();
            BufReader::new(connection.try_clone().unwrap())
                .read_line(&mut request)
                .unwrap();
            assert_eq!(request, expected);
            connection.write_all(response.as_bytes()).unwrap();
        }
    });
    (LinuxControlClient::new(socket).unwrap(), server, directory)
}

#[test]
fn linked_library_queries_and_mutates_through_its_original_broker_client() {
    let (client, server, directory) = broker(vec![
        ("status Virtual-2\n", "STATUS disconnected Virtual-2\n"),
        ("connect Virtual-2\n", "OK connected Virtual-2\n"),
        ("disconnect Virtual-2\n", "OK disconnected Virtual-2\n"),
    ]);
    assert!(!client.query_connector("Virtual-2").unwrap());
    assert!(client.set_connector("Virtual-2", true).unwrap());
    assert!(!client.set_connector("Virtual-2", false).unwrap());
    server.join().unwrap();
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn another_uids_connector_cannot_be_adopted_by_status_shortcut() {
    let (client, server, directory) = broker(vec![(
        "connect Virtual-1\n",
        "ERROR connector owned by another uid\n",
    )]);
    let error = client.set_connector("Virtual-1", true).unwrap_err();
    assert_eq!(error.status, 4);
    assert_eq!(error.detail, "ERROR connector owned by another uid");
    server.join().unwrap();
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn requested_mode_uses_authenticated_broker_and_exact_reply() {
    use std::os::unix::fs::PermissionsExt;
    let (client, server, directory) = broker(vec![(
        "mode Virtual-2 2560 1440 120000\n",
        "OK mode Virtual-2 2560 1440 120000\n",
    )]);
    fs::set_permissions(client.socket_path(), fs::Permissions::from_mode(0o660)).unwrap();
    // SAFETY: geteuid has no arguments or memory-safety preconditions.
    let uid = unsafe { libc::geteuid() };
    client
        .request_mode_from_peer("Virtual-2", 2560, 1440, 120000, uid)
        .unwrap();
    server.join().unwrap();
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn rejects_invalid_outputs_before_connecting() {
    let client =
        LinuxControlClient::new(PathBuf::from("/nonexistent/amaterasu-display.sock")).unwrap();
    for output in [
        "Virtual-0",
        "Virtual-01",
        "HDMI-A-1",
        "Virtual-1\nconnect Virtual-2",
        "Virtual-1\0",
    ] {
        assert_eq!(client.query_connector(output).unwrap_err().status, 1);
    }
    assert!(LinuxControlClient::new(PathBuf::from("relative.sock")).is_err());
}

#[test]
#[ignore = "queries the installed root-owned libvirtualdisplay broker; never changes a connector"]
fn native_broker_reports_the_existing_virtual_desktop() {
    let connected = LinuxControlClient::default()
        .query_connector("Virtual-1")
        .unwrap();
    assert!(connected);
}
