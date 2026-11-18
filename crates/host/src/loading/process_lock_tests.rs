use super::{NativeProcessState, ProcessRootLock};
use amaterasu_package::RootLock;
use std::path::PathBuf;
use std::sync::Arc;

fn test_root(label: &str) -> PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "amaterasu-process-lock-{label}-{}-{nonce}",
        std::process::id()
    ));
    std::fs::create_dir(&root).unwrap();
    root
}

#[test]
fn healthy_process_releases_its_root_lock_normally() {
    let root = test_root("healthy");
    let lock = ProcessRootLock {
        lock: Some(RootLock::acquire(&root).unwrap()),
        process: Arc::new(NativeProcessState::default()),
    };
    assert!(RootLock::acquire(&root).is_err());
    drop(lock);
    // A parallel test can be between fork and exec on macOS. Its temporary
    // copy of the close-on-exec descriptor cannot keep admission indefinitely.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
    let reacquired = loop {
        match RootLock::acquire(&root) {
            Ok(lock) => break lock,
            Err(error) => {
                assert!(std::time::Instant::now() < deadline, "{error}");
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }
    };
    drop(reacquired);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn quarantined_root_lock_child_fixture() {
    let Some(root) = std::env::var_os("AMATERASU_QUARANTINE_LOCK_TEST_ROOT") else {
        return;
    };
    let root = PathBuf::from(root);
    let process = Arc::new(NativeProcessState::default());
    let lock = ProcessRootLock {
        lock: Some(RootLock::acquire(&root).unwrap()),
        process: process.clone(),
    };
    process.quarantine();
    drop(lock);
    assert!(
        RootLock::acquire(&root).is_err(),
        "error-return stack unwinding must not release native admission"
    );
}

#[test]
fn quarantined_process_keeps_admission_until_actual_process_exit() {
    let root = test_root("quarantined");
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "loading::process_lock_tests::quarantined_root_lock_child_fixture",
            "--nocapture",
        ])
        .env("AMATERASU_QUARANTINE_LOCK_TEST_ROOT", &root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "child lock fixture failed: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    // The child retained its lock wrapper. Successful admission now proves that
    // the actual OS process exit released the lock, independently of graph Drop.
    drop(RootLock::acquire(&root).unwrap());
    std::fs::remove_dir_all(root).unwrap();
}
