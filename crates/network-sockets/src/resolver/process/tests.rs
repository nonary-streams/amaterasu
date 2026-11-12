use super::*;
use crate::resolver::helperprotocol::{MAX_RESPONSE_BYTES, Request, Response};
use std::time::{Duration, Instant};

#[cfg(all(unix, not(target_os = "linux")))]
use super::platform as private_image;
#[cfg(target_os = "linux")]
use super::private_unix_test as private_image;

#[cfg(unix)]
#[test]
fn private_unix_images_keep_exact_identity_and_user_only_modes() {
    private_image::assert_private_image_guards(HELPER_BYTES);
}

#[cfg(unix)]
#[test]
fn private_unix_images_execute_only_the_materialized_helper_bytes() {
    assert_eq!(
        Response::decode(&private_image::run_private_image(HELPER_BYTES, &request())).unwrap(),
        Response::Addresses(vec!["127.0.0.1:47989".parse().unwrap()])
    );
}

#[cfg(unix)]
#[test]
fn private_unix_fifo_ends_are_atomically_noninheritable_and_keep_eof_local() {
    private_image::assert_atomic_fifo_lifetime(
        include_bytes!(env!("AMATERASU_NETWORK_RESOLVER_FIXTURE_PATH")),
        &request(),
    );
}

#[cfg(unix)]
#[test]
fn private_unix_images_reject_untrusted_roots_without_changing_them() {
    private_image::assert_untrusted_roots_are_preserved(HELPER_BYTES);
}

#[cfg(unix)]
#[test]
fn private_unix_image_cache_is_bounded_and_active_images_remain_owned() {
    private_image::assert_private_image_capacity(HELPER_BYTES);
}

#[cfg(unix)]
#[test]
fn private_unix_default_test_images_use_independent_roots_until_the_last_arc_drops() {
    private_image::assert_default_test_images_have_isolated_roots(HELPER_BYTES);
}

#[cfg(unix)]
#[test]
fn private_unix_default_test_root_cleanup_preserves_unknown_entries() {
    private_image::assert_default_test_cleanup_preserves_unknown_entries(HELPER_BYTES);
}

#[cfg(unix)]
#[test]
fn private_unix_cache_reclaims_only_verified_unlocked_images() {
    private_image::assert_stale_image_cleanup_preserves_unknown_entries(HELPER_BYTES);
}

fn request() -> Vec<u8> {
    Request {
        host: "127.0.0.1".into(),
        port: 47989,
    }
    .encode()
    .unwrap()
}

#[cfg(target_os = "linux")]
#[test]
fn embedded_executable_bytes_cannot_be_substituted_or_truncated() {
    let executable = HelperExecutable::new(None).unwrap();
    platform::assert_sealed(&executable.0, HELPER_BYTES);
}

#[cfg(windows)]
#[test]
fn verified_helper_handles_block_byte_and_path_substitution() {
    let executable = HelperExecutable::new(None).unwrap();
    platform::assert_windows_substitution_guards(&executable.0);
}

#[cfg(windows)]
#[test]
fn helper_cache_is_bounded_and_preserves_active_or_unrelated_files() {
    platform::assert_windows_cache_bound(HELPER_BYTES);
}

#[cfg(windows)]
#[test]
fn an_interrupted_new_helper_write_does_not_poison_the_canonical_path() {
    platform::assert_windows_failed_write_is_retryable(HELPER_BYTES);
}

#[test]
fn empty_and_oversized_requests_are_rejected_before_starting_a_child() {
    let executable = HelperExecutable::new(None).unwrap();
    assert!(matches!(
        executable.spawn(&[]),
        Err(error) if error.kind() == io::ErrorKind::InvalidInput
    ));
    assert!(matches!(
        executable.spawn(&vec![0; MAX_REQUEST_BYTES + 1]),
        Err(error) if error.kind() == io::ErrorKind::InvalidInput
    ));
}

#[test]
fn numeric_resolution_uses_embedded_bytes_and_keeps_the_image_alive_after_owner_drop() {
    let executable = HelperExecutable::new(None).unwrap();
    let mut child = executable.spawn(&request()).unwrap();
    drop(executable);
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut response = Vec::new();
    let mut closed = false;
    let mut exited = None;
    while !closed || exited.is_none() {
        let mut buffer = [0; 1024];
        match child.try_read(&mut buffer) {
            Ok(0) => closed = true,
            Ok(length) => response.extend_from_slice(&buffer[..length]),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(error) => panic!("helper output failed: {error}"),
        }
        assert!(response.len() <= MAX_RESPONSE_BYTES);
        exited = child.try_wait().unwrap();
        assert!(Instant::now() < deadline, "numeric helper failed to retire");
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(exited.unwrap().success());
    assert_eq!(
        Response::decode(&response).unwrap(),
        Response::Addresses(vec!["127.0.0.1:47989".parse().unwrap()])
    );
    child.kill().unwrap();
    assert_eq!(child.try_wait().unwrap(), exited);
}

fn fixture(mode: &str) -> HelperExecutable {
    HelperExecutable::for_test_fixture(
        include_bytes!(env!("AMATERASU_NETWORK_RESOLVER_FIXTURE_PATH")),
        &[mode],
    )
    .unwrap()
}

fn wait_for_exit(child: &mut ResolverChild) -> ExitStatus {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        assert!(
            Instant::now() < deadline,
            "terminated fixture failed to retire"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn stalled_child_read_and_exit_observation_stay_nonblocking_until_exact_termination() {
    let executable = fixture("block");
    let mut child = executable.spawn(&request()).unwrap();
    let observation_deadline = Instant::now() + Duration::from_millis(30);
    while Instant::now() < observation_deadline {
        let mut output = [0; 32];
        let before = Instant::now();
        assert!(
            matches!(child.try_read(&mut output), Err(error) if error.kind() == io::ErrorKind::WouldBlock)
        );
        assert!(child.try_wait().unwrap().is_none());
        assert!(before.elapsed() < Duration::from_millis(100));
        std::thread::sleep(Duration::from_millis(1));
    }
    child.kill().unwrap();
    assert!(!wait_for_exit(&mut child).success());
}

#[test]
fn a_complete_address_response_does_not_count_as_child_retirement() {
    let executable = fixture("addresses_then_block");
    let mut child = executable.spawn(&request()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    let expected = Response::Addresses(vec!["127.0.0.1:47989".parse().unwrap()])
        .encode()
        .unwrap();
    let mut response = Vec::new();
    while response.len() < expected.len() {
        let mut buffer = [0; 128];
        match child.try_read(&mut buffer) {
            Ok(0) => panic!("fixture exited before its held response could be inspected"),
            Ok(length) => response.extend_from_slice(&buffer[..length]),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(error) => panic!("fixture output failed: {error}"),
        }
        assert!(child.try_wait().unwrap().is_none());
        assert!(Instant::now() < deadline, "fixture response did not arrive");
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(response, expected);
    assert!(child.try_wait().unwrap().is_none());
    child.kill().unwrap();
    assert!(!wait_for_exit(&mut child).success());
}
