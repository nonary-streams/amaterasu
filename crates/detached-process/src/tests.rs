use super::*;
use std::path::PathBuf;
use std::sync::Barrier;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

struct Fixture {
    directory: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let directory = std::env::temp_dir().join(format!(
            "amaterasu-detached-reaper-{name}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        std::fs::create_dir(&directory).unwrap();
        Self { directory }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", "tests::detached_fixture", "--ignored"])
            .env("AMATERASU_REAPER_FIXTURE", &self.directory)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        command
    }

    fn release(&self) {
        std::fs::write(self.directory.join("release"), b"release").unwrap();
    }

    fn wait_for(&self, name: &str) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !self.directory.join(name).exists() {
            assert!(Instant::now() < deadline, "fixture did not write {name}");
            thread::sleep(Duration::from_millis(5));
        }
    }

    fn cleanup(&self) {
        std::fs::remove_dir_all(&self.directory).unwrap();
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::write(self.directory.join("release"), b"release");
    }
}

#[test]
#[ignore = "subprocess entry point for detached child ownership tests"]
fn detached_fixture() {
    let directory = PathBuf::from(std::env::var_os("AMATERASU_REAPER_FIXTURE").unwrap());
    std::fs::write(directory.join("started"), b"started").unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !directory.join("release").exists() {
        assert!(Instant::now() < deadline, "fixture release timed out");
        thread::sleep(Duration::from_millis(5));
    }
    if std::env::var_os("AMATERASU_REAPER_WRITE_OUTPUT").is_some() {
        use std::io::Write;
        std::io::stdout().write_all(b"late-child-output\n").unwrap();
    }
    std::fs::write(directory.join("finished"), b"finished").unwrap();
}

#[test]
#[ignore = "subprocess entry point for output lease tests"]
fn output_fixture() {
    use std::io::Write;
    std::io::stdout().write_all(b"stdout-marker\n").unwrap();
    std::io::stderr().write_all(b"stderr-marker\n").unwrap();
}

fn output_command() -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command.args([
        "--exact",
        "tests::output_fixture",
        "--ignored",
        "--nocapture",
    ]);
    command
}

#[cfg(unix)]
#[test]
fn output_lease_pins_original_inode_and_capacity_until_the_last_clone() {
    let fixture = Fixture::new("sink-rotation");
    let path = fixture.directory.join("output");
    let original = fixture.directory.join("output.rotated");
    let rejected = fixture.directory.join("must-not-open");
    let registry = Registry::with_limits(1, 1).unwrap();
    let sink = registry.open_output(&path).unwrap();
    let retained = sink.clone();
    drop(sink);
    assert!(registry.open_output(&rejected).is_err());
    assert!(!rejected.exists(), "a rejected sink must not open the path");

    std::fs::rename(&path, &original).unwrap();
    std::fs::write(&path, b"replacement\n").unwrap();
    let mut command = output_command();
    retained.attach(&mut command, true, true).unwrap();
    // An accepted command has its own duplicates, so closing the last lease
    // cannot redirect it or free the underlying file before spawn.
    drop(retained);
    let next = registry.open_output(&rejected).unwrap();
    let child = registry.spawn(&mut command).unwrap();
    assert!(wait_for_exit(&child).success());
    let old_output = std::fs::read_to_string(&original).unwrap();
    assert!(old_output.contains("stdout-marker"), "{old_output}");
    assert!(old_output.contains("stderr-marker"), "{old_output}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "replacement\n");
    drop(child);
    drop(next);
    fixture.cleanup();
}

#[test]
fn invalid_and_failed_output_opens_do_not_consume_sink_capacity() {
    let fixture = Fixture::new("sink-validation");
    let registry = Registry::with_limits(1, 1).unwrap();
    assert!(
        registry
            .open_output(PathBuf::from("relative").as_path())
            .is_err()
    );
    assert!(registry.open_output(&fixture.directory).is_err());
    assert!(
        registry
            .open_output(&fixture.directory.join("bad\nname"))
            .is_err()
    );
    let sink = registry
        .open_output(&fixture.directory.join("valid"))
        .unwrap();
    drop(sink);
    fixture.cleanup();
}

#[test]
fn accepted_child_keeps_output_after_every_observer_and_sink_closes() {
    let fixture = Fixture::new("sink-detached");
    let path = fixture.directory.join("output");
    let registry = Registry::with_limits(1, 1).unwrap();
    let sink = registry.open_output(&path).unwrap();
    let mut command = fixture.command();
    command.env("AMATERASU_REAPER_WRITE_OUTPUT", "1");
    sink.attach(&mut command, true, false).unwrap();
    let child = registry.spawn(&mut command).unwrap();
    fixture.wait_for("started");
    drop(child);
    drop(sink);
    drop(command);
    drop(registry);
    fixture.release();
    fixture.wait_for("finished");
    assert!(
        std::fs::read_to_string(path)
            .unwrap()
            .contains("late-child-output")
    );
    fixture.cleanup();
}

#[cfg(unix)]
#[test]
fn fifo_output_is_rejected_without_waiting_for_a_reader() {
    use std::os::unix::ffi::OsStrExt;
    let fixture = Fixture::new("sink-fifo");
    let path = fixture.directory.join("fifo");
    let encoded = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: encoded is a live NUL-terminated unique test path.
    assert_eq!(unsafe { libc::mkfifo(encoded.as_ptr(), 0o600) }, 0);
    let registry = Registry::with_limits(1, 1).unwrap();
    let start = Instant::now();
    assert!(registry.open_output(&path).is_err());
    assert!(start.elapsed() < Duration::from_secs(1));
    let sink = registry
        .open_output(&fixture.directory.join("valid"))
        .unwrap();
    drop(sink);
    fixture.cleanup();
}

fn wait_for_exit(process: &Process) -> ExitStatus {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = process.status().unwrap() {
            return status;
        }
        assert!(Instant::now() < deadline, "child was not reaped");
        thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn capacity_is_reserved_before_concurrent_spawns_and_until_observer_release() {
    let fixture = Fixture::new("capacity");
    let registry = Registry::new(1).unwrap();
    let barrier = Arc::new(Barrier::new(8));
    let mut workers = Vec::new();
    for _ in 0..8 {
        let registry = registry.clone();
        let barrier = Arc::clone(&barrier);
        let mut command = fixture.command();
        workers.push(thread::spawn(move || {
            barrier.wait();
            registry.spawn(&mut command)
        }));
    }
    let results: Vec<_> = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    let process = results.into_iter().find_map(Result::ok).unwrap();
    fixture.wait_for("started");
    assert!(process.status().unwrap().is_none());
    fixture.release();
    assert!(wait_for_exit(&process).success());
    // Completion stays queryable and bounded until the observer drops it.
    assert!(registry.spawn(&mut fixture.command()).is_err());
    drop(process);
    let replacement = registry.spawn(&mut fixture.command()).unwrap();
    assert!(wait_for_exit(&replacement).success());
    drop(replacement);
    fixture.cleanup();
}

#[test]
fn failed_spawn_returns_its_reservation() {
    let fixture = Fixture::new("failed-spawn");
    let registry = Registry::new(1).unwrap();
    assert!(
        registry
            .spawn(&mut Command::new(fixture.directory.join("missing")))
            .is_err()
    );
    let child = registry.spawn(&mut fixture.command()).unwrap();
    fixture.release();
    assert!(wait_for_exit(&child).success());
    drop(child);
    fixture.cleanup();
}

#[cfg(unix)]
#[test]
fn dropping_every_observer_keeps_the_child_alive_and_eventually_reaps_it() {
    let fixture = Fixture::new("reload");
    let registry = Registry::new(1).unwrap();
    let child = registry.spawn(&mut fixture.command()).unwrap();
    let pid = child.id();
    fixture.wait_for("started");
    drop(child);
    drop(registry);
    fixture.release();
    fixture.wait_for("finished");

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        // SAFETY: this does not reap the fixture child; the reaper has exclusive
        // wait ownership, and the zeroed siginfo storage is writable.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                pid,
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if result == -1 && io::Error::last_os_error().raw_os_error() == Some(libc::ECHILD) {
            break;
        }
        assert_eq!(result, 0, "unexpected child wait error");
        assert!(Instant::now() < deadline, "detached child was never reaped");
        thread::sleep(Duration::from_millis(5));
    }
    fixture.cleanup();
}

#[cfg(unix)]
#[test]
fn worker_never_reaps_an_unregistered_child() {
    struct OwnedChild(Child);
    impl Drop for OwnedChild {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let registered_fixture = Fixture::new("registered");
    let unrelated_fixture = Fixture::new("unregistered");
    let registry = Registry::new(1).unwrap();
    let registered = registry.spawn(&mut registered_fixture.command()).unwrap();
    let mut unrelated = OwnedChild(unrelated_fixture.command().spawn().unwrap());
    unrelated_fixture.wait_for("started");
    registered_fixture.release();
    unrelated_fixture.release();
    assert!(wait_for_exit(&registered).success());
    unrelated_fixture.wait_for("finished");

    // Only the test owns this unrelated child. The registered reaper must not
    // use wildcard waits that could steal its exit status.
    assert!(unrelated.0.wait().unwrap().success());
    drop(registered);
    registered_fixture.cleanup();
    unrelated_fixture.cleanup();
}

#[cfg(unix)]
#[test]
fn lost_wait_ownership_never_reports_retirement_or_discards_the_child() {
    let fixture = Fixture::new("wait-error");
    fixture.release();
    let child = fixture.command().spawn().unwrap();
    let mut entry = Entry::Spawned {
        child: Some(child),
        exit: None,
        wait_error: None,
        observed: false,
    };
    assert!(poll_entry(&mut entry, |_| Err(
        io::Error::from_raw_os_error(libc::ECHILD)
    )));
    assert!(matches!(
        entry,
        Entry::Spawned {
            child: Some(_),
            wait_error: Some(_),
            ..
        }
    ));
    let deadline = Instant::now() + Duration::from_secs(5);
    while poll_entry(&mut entry, Child::try_wait) {
        assert!(Instant::now() < deadline, "fixture did not exit");
        thread::sleep(Duration::from_millis(5));
    }
    assert!(matches!(
        entry,
        Entry::Spawned {
            child: None,
            exit: Some(_),
            wait_error: None,
            ..
        }
    ));
    fixture.cleanup();
}
