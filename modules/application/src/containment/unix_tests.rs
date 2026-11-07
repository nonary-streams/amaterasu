use super::*;
use std::process::Stdio;
use std::time::{Duration, Instant};

/// Every test owns only this controlled subprocess, including panic cleanup.
struct ExitedRoot(ContainedChild);

impl ExitedRoot {
    fn spawn() -> Self {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", "tests::controlled_child", "--ignored"])
            .env("AMATERASU_TEST_CHILD", "exit")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let child = ContainedChild::spawn(&mut command, false)
            .unwrap_or_else(|error| panic!("controlled root spawn failed: {}", error.error));
        let mut root = Self(child);
        let deadline = Instant::now() + Duration::from_secs(10);
        while !root.0.exited().unwrap() {
            assert!(Instant::now() < deadline, "controlled root did not exit");
            std::thread::sleep(Duration::from_millis(5));
        }
        root
    }
}

impl Drop for ExitedRoot {
    fn drop(&mut self) {
        // No descendants are spawned. Contain and reap exactly the test-owned
        // root on panic; its unreaped PID remains reserved until this wait.
        let _ = self.0.child.kill();
        let _ = self.0.child.wait();
    }
}

#[test]
fn root_reap_waits_for_group_retirement_after_force_acceptance() {
    let mut root = ExitedRoot::spawn();
    root.0.force_stop().unwrap();

    // SIGKILL acceptance does not synchronously retire descendants. Model that
    // native observation while the actual exited leader remains wait-owned.
    assert!(
        root.0
            .try_reap_with_group_observation(|_| Ok(true))
            .unwrap()
            .is_none(),
        "a live descendant must keep the root PID and process group reserved"
    );
    assert!(root.0.exited().unwrap(), "the root must remain unreaped");

    assert!(
        root.0
            .try_reap_with_group_observation(|_| Ok(false))
            .unwrap()
            .is_some(),
        "complete group retirement permits releasing the root"
    );
}

#[test]
fn group_observation_error_keeps_the_root_reserved_for_retry() {
    let mut root = ExitedRoot::spawn();
    root.0.force_stop().unwrap();

    let error = root
        .0
        .try_reap_with_group_observation(|_| {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "injected incomplete group inspection",
            ))
        })
        .expect_err("an incomplete scan cannot establish group retirement");
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    assert!(root.0.exited().unwrap(), "the root must remain unreaped");
    assert!(
        root.0
            .try_reap_with_group_observation(|_| Ok(false))
            .unwrap()
            .is_some()
    );
}

#[test]
fn root_reap_requires_the_final_containment_attempt() {
    let mut root = ExitedRoot::spawn();
    assert!(
        root.0
            .try_reap_with_group_observation(|_| panic!("group inspected before containment"))
            .unwrap()
            .is_none()
    );
    assert!(root.0.exited().unwrap());
}

#[test]
fn lost_wait_ownership_does_not_use_a_recycled_group_observation() {
    let mut root = ExitedRoot::spawn();
    root.0.force_stop().unwrap();
    assert!(root.0.child.try_wait().unwrap().is_some());

    let error = root
        .0
        .try_reap_with_group_observation(|_| {
            panic!("a group number cannot be inspected after its root reservation is lost")
        })
        .expect_err("external reaping must invalidate containment ownership");
    assert_eq!(error.raw_os_error(), Some(libc::ECHILD));
}

#[test]
#[cfg(target_os = "linux")]
fn malformed_process_stat_cannot_prove_group_retirement() {
    for malformed in [
        "",
        "123 (missing terminator",
        "123 (command)",
        "123 (command) S",
        "123 (command) S 1",
        "123 (command) S invalid-parent 42",
        "123 (command) S 1 invalid-group",
    ] {
        assert!(
            LinuxProcessStat::parse(malformed)
                .expect_err("an unreadable identity cannot establish group absence")
                .kind()
                == io::ErrorKind::InvalidData
        );
    }
}

#[test]
#[cfg(target_os = "linux")]
fn group_retirement_distinguishes_live_members_from_exited_and_other_groups() {
    let scan = |stat| {
        scan_group(
            "9 (self) S 1 7",
            "NStgid:\t9\n",
            &[(42, "42 (leader) Z 1 42"), (123, stat)],
        )
        .unwrap()
    };
    assert!(scan("123 (command with ) spaces) S 1 42"));
    assert!(!scan("123 (command) Z 1 42"));
    assert!(!scan("123 (command) X 1 42"));
    assert!(!scan("123 (command) x 1 42"));
    assert!(!scan("123 (command) S 1 99"));
}

#[test]
#[cfg(target_os = "linux")]
fn zombie_group_leader_with_live_sibling_is_not_retired() {
    let processes = [
        (42, "42 (held leader) Z 1 42"),
        (43, "43 (descendant) Z 1 42"),
    ];
    let scan = |sibling_live: bool| {
        linux_group_has_live_members_from_stats(
            42,
            9,
            "9 (self) S 1 7",
            "NStgid:\t9\n",
            processes
                .iter()
                .map(|(pid, stat)| Ok(Some((*pid, (*stat).to_owned())))),
            |pid| Ok(pid != 43 || !sibling_live),
        )
    };
    assert!(scan(true).unwrap());
    assert!(!scan(false).unwrap());
}

#[test]
#[cfg(target_os = "linux")]
fn failed_pidfd_observation_cannot_prove_retirement() {
    let entries = [
        Ok(Some((42, "42 (held leader) Z 1 42".to_owned()))),
        Ok(Some((43, "43 (descendant) Z 1 42".to_owned()))),
    ];
    let error = linux_group_has_live_members_from_stats(
        42,
        9,
        "9 (self) S 1 7",
        "NStgid:\t9\n",
        entries,
        |pid| {
            if pid == 42 {
                Ok(true)
            } else {
                Err(io::Error::from_raw_os_error(libc::ENOSYS))
            }
        },
    )
    .expect_err("unavailable pidfd observation cannot prove group retirement");
    assert_eq!(error.raw_os_error(), Some(libc::ENOSYS));
}

#[test]
#[cfg(target_os = "linux")]
fn invalid_pidfd_poll_events_cannot_prove_retirement() {
    assert!(linux_pidfd_revents(libc::POLLIN).unwrap());
    assert!(linux_pidfd_revents(libc::POLLHUP).unwrap());
    for event in [
        0,
        libc::POLLERR,
        libc::POLLNVAL,
        libc::POLLIN | libc::POLLERR,
    ] {
        assert!(linux_pidfd_revents(event).is_err());
    }
}

#[cfg(target_os = "linux")]
fn scan_group(self_stat: &str, self_status: &str, processes: &[(u32, &str)]) -> io::Result<bool> {
    linux_group_has_live_members_from_stats(
        42,
        9,
        self_stat,
        self_status,
        processes
            .iter()
            .map(|(process_id, stat)| Ok(Some((*process_id, (*stat).to_owned())))),
        |_| Ok(true),
    )
}

#[test]
#[cfg(target_os = "linux")]
fn retirement_scan_requires_the_same_procfs_pid_namespace() {
    let leader = [(42, "42 (held leader) Z 1 42")];
    for mismatched_status in [
        "Name:\ttest\n",
        "NStgid:\n",
        "NStgid:\tinvalid\n",
        "NStgid:\t0\n",
        "NStgid:\t1000\n",
        "NStgid:\t1000 9\n",
        "NStgid:\t9 9\n",
        "NStgid:\t9\nNStgid:\t9\n",
    ] {
        assert_eq!(
            scan_group("9 (self) S 1 7", mismatched_status, &leader)
                .expect_err("another or unknown procfs PID namespace cannot prove retirement")
                .kind(),
            io::ErrorKind::InvalidData
        );
    }
}

#[test]
#[cfg(target_os = "linux")]
fn retirement_scan_rejects_self_stat_from_another_pid_view() {
    assert_eq!(
        scan_group(
            "1000 (self) S 1 7",
            "NStgid:\t9\n",
            &[(42, "42 (held leader) Z 1 42")],
        )
        .expect_err("self stat must agree with the current process ID")
        .kind(),
        io::ErrorKind::InvalidData
    );
}

#[test]
#[cfg(target_os = "linux")]
fn retirement_scan_requires_the_held_leader_in_its_expected_group() {
    for processes in [
        &[][..],
        &[(43, "43 (unrelated) S 1 99")][..],
        &[(42, "42 (wrong group) Z 1 99")][..],
        &[(42, "1000 (wrong identity) Z 1 42")][..],
    ] {
        assert_eq!(
            scan_group("9 (self) S 1 7", "NStgid:\t9\n", processes)
                .expect_err("a partial or inconsistent group view cannot prove retirement")
                .kind(),
            io::ErrorKind::InvalidData
        );
    }
}

#[test]
#[cfg(target_os = "linux")]
fn complete_local_proc_view_can_distinguish_retired_and_live_groups() {
    assert!(
        !scan_group(
            "9 (self) S 1 7",
            "Name:\ttest\nNStgid:\t9\n",
            &[
                (42, "42 (held leader) Z 1 42"),
                (43, "43 (dead descendant) X 1 42"),
                (44, "44 (unrelated) S 1 99"),
            ],
        )
        .unwrap()
    );
    assert!(
        scan_group(
            "9 (self) S 1 7",
            "NStgid:\t9\n",
            &[
                (42, "42 (held leader) Z 1 42"),
                (43, "43 (live descendant) D 1 42")
            ],
        )
        .unwrap()
    );
}

#[test]
#[cfg(target_os = "linux")]
fn vanished_proc_stat_is_skipped_but_cannot_replace_the_held_leader() {
    let read = |pid, result| linux_process_stat_from_read(pid, result);
    let scan = |entries: Vec<io::Result<Option<(u32, String)>>>| {
        linux_group_has_live_members_from_stats(
            42,
            9,
            "9 (self) S 1 7",
            "NStgid:\t9\n",
            entries,
            |_| Ok(true),
        )
    };
    let leader = || read(42, Ok("42 (held leader) Z 1 42".to_owned()));
    for errno in [libc::ENOENT, libc::ESRCH] {
        assert!(
            !scan(vec![
                leader(),
                read(43, Err(io::Error::from_raw_os_error(errno)))
            ])
            .expect("a process that vanished during stat cannot remain a live group member")
        );
        assert_eq!(
            scan(vec![read(42, Err(io::Error::from_raw_os_error(errno)))])
                .expect_err("a missing owned leader cannot prove group retirement")
                .kind(),
            io::ErrorKind::InvalidData
        );
    }
    assert_eq!(
        scan(vec![
            leader(),
            read(43, Err(io::Error::from_raw_os_error(libc::EACCES)))
        ])
        .expect_err("an unreadable process may be a live group member")
        .kind(),
        io::ErrorKind::PermissionDenied
    );
}

#[test]
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn unsupported_unix_retirement_does_not_release_the_root_reservation() {
    let mut root = ExitedRoot::spawn();
    root.0.force_stop().unwrap();

    let error = root.0.try_reap().expect_err(
        "numeric group probing cannot distinguish a zombie leader from live descendants",
    );
    assert_eq!(error.kind(), io::ErrorKind::Unsupported);
    assert!(root.0.exited().unwrap());
}
