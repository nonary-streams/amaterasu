//! Best-effort association of a newly launched Steam game process.
//! Steam's client remains outside the managed process group; only game PIDs
//! attributed by install-tree evidence can be signaled.
use std::time::Instant;

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SteamGameLiveness {
    Unknown,
    Searching,
    #[cfg_attr(not(any(target_os = "linux", windows)), allow(dead_code))]
    Associated,
    Stopping,
}

pub(crate) struct SteamProcessTracker {
    #[cfg(target_os = "linux")]
    inner: linux::Tracker,
    #[cfg(windows)]
    inner: windows::Tracker,
}

impl SteamProcessTracker {
    pub(crate) fn begin(install_directory: &str) -> Option<Self> {
        #[cfg(target_os = "linux")]
        {
            linux::Tracker::begin(install_directory).map(|inner| Self { inner })
        }
        #[cfg(windows)]
        {
            windows::Tracker::begin(install_directory).map(|inner| Self { inner })
        }
        #[cfg(not(any(target_os = "linux", windows)))]
        {
            let _ = install_directory;
            None
        }
    }

    pub(crate) fn observe(&mut self, now: Instant) -> SteamGameLiveness {
        #[cfg(target_os = "linux")]
        {
            self.inner.observe(now)
        }
        #[cfg(windows)]
        {
            self.inner.observe(now)
        }
        #[cfg(not(any(target_os = "linux", windows)))]
        {
            let _ = now;
            SteamGameLiveness::Unknown
        }
    }

    pub(crate) fn was_associated(&self) -> bool {
        #[cfg(target_os = "linux")]
        {
            self.inner.was_associated()
        }
        #[cfg(windows)]
        {
            self.inner.was_associated()
        }
        #[cfg(not(any(target_os = "linux", windows)))]
        {
            false
        }
    }

    /// Begin stopping the currently associated game tree. The Steam client and
    /// its runtime processes are excluded and never receive signals.
    pub(crate) fn request_stop(&mut self, now: Instant) -> bool {
        #[cfg(target_os = "linux")]
        {
            self.inner.request_stop(now)
        }
        #[cfg(windows)]
        {
            self.inner.request_stop(now)
        }
        #[cfg(not(any(target_os = "linux", windows)))]
        {
            let _ = now;
            false
        }
    }

    /// Returns true while attributed game processes remain after bounded TERM/KILL.
    pub(crate) fn advance_stop(&mut self, now: Instant) -> bool {
        #[cfg(target_os = "linux")]
        {
            self.inner.advance_stop(now)
        }
        #[cfg(windows)]
        {
            self.inner.advance_stop(now)
        }
        #[cfg(not(any(target_os = "linux", windows)))]
        {
            let _ = now;
            false
        }
    }

    pub(crate) fn is_stopping(&self) -> bool {
        #[cfg(target_os = "linux")]
        {
            self.inner.is_stopping()
        }
        #[cfg(windows)]
        {
            self.inner.is_stopping()
        }
        #[cfg(not(any(target_os = "linux", windows)))]
        {
            false
        }
    }
}

#[cfg(windows)]
#[path = "steam_tracking/windows.rs"]
mod windows;

#[cfg(target_os = "linux")]
mod linux {
    use super::{Instant, SteamGameLiveness};
    use std::collections::{BTreeMap, BTreeSet};
    use std::ffi::c_void;
    use std::fs;
    use std::io;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    const MAX_PROCESS_SCAN: usize = 131_072;
    const GAME_ASSOCIATION_WINDOW: Duration = Duration::from_secs(15);
    const STOP_GRACE: Duration = Duration::from_secs(2);
    const STOP_SCAN_FAILURE_GRACE: Duration = Duration::from_secs(10);

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    struct Identity {
        pid: u32,
        start_ticks: u64,
    }

    #[derive(Clone, Debug)]
    struct Process {
        identity: Identity,
        user_id: u32,
        parent_id: u32,
        executable: Option<PathBuf>,
        working_directory: Option<PathBuf>,
    }

    pub(super) struct Tracker {
        install_root: PathBuf,
        user_id: u32,
        baseline: BTreeMap<u32, u64>,
        tracked: BTreeMap<u32, Identity>,
        started_at: Instant,
        associated: bool,
        stopping_at: Option<Instant>,
    }

    impl Tracker {
        pub(super) fn begin(install_directory: &str) -> Option<Self> {
            let install_root = Path::new(install_directory).canonicalize().ok()?;
            if !install_root.is_dir() || install_root == Path::new("/") {
                return None;
            }
            let user_id = effective_user_id();
            let baseline = scan_processes()?
                .into_iter()
                .filter(|process| process.user_id == user_id)
                .map(|process| (process.identity.pid, process.identity.start_ticks))
                .collect();
            Some(Self {
                install_root,
                user_id,
                baseline,
                tracked: BTreeMap::new(),
                started_at: Instant::now(),
                associated: false,
                stopping_at: None,
            })
        }

        pub(super) fn observe(&mut self, now: Instant) -> SteamGameLiveness {
            let Some(processes) = scan_processes() else {
                return if self.associated {
                    SteamGameLiveness::Associated
                } else {
                    SteamGameLiveness::Unknown
                };
            };
            self.refresh_tracking(&processes);
            if self.stopping_at.is_some() {
                SteamGameLiveness::Stopping
            } else if !self.tracked.is_empty() {
                self.associated = true;
                SteamGameLiveness::Associated
            } else if self.associated
                || now.saturating_duration_since(self.started_at) > GAME_ASSOCIATION_WINDOW
            {
                SteamGameLiveness::Unknown
            } else {
                SteamGameLiveness::Searching
            }
        }

        pub(super) fn was_associated(&self) -> bool {
            self.associated
        }

        pub(super) fn is_stopping(&self) -> bool {
            self.stopping_at.is_some()
        }

        pub(super) fn request_stop(&mut self, now: Instant) -> bool {
            let Some(processes) = scan_processes() else {
                return false;
            };
            self.refresh_tracking(&processes);
            if self.tracked.is_empty() {
                return false;
            }
            for identity in self.tracked_descendant_first(&processes) {
                let _ = signal_identity(identity, libc::SIGTERM, self.user_id);
            }
            self.stopping_at = Some(now + STOP_GRACE);
            true
        }

        pub(super) fn advance_stop(&mut self, now: Instant) -> bool {
            let Some(processes) = scan_processes() else {
                // An unreadable process table cannot justify another signal.
                let finished = self
                    .stopping_at
                    .is_some_and(|deadline| now >= deadline + STOP_SCAN_FAILURE_GRACE);
                if finished {
                    self.stopping_at = None;
                    self.tracked.clear();
                }
                return !finished;
            };
            self.refresh_tracking(&processes);
            if self.tracked.is_empty() {
                self.stopping_at = None;
                return false;
            }
            let Some(deadline) = self.stopping_at else {
                return true;
            };
            if now >= deadline + STOP_SCAN_FAILURE_GRACE {
                self.tracked.clear();
                self.stopping_at = None;
                return false;
            }
            if now >= deadline {
                for identity in self.tracked_descendant_first(&processes) {
                    let _ = signal_identity(identity, libc::SIGKILL, self.user_id);
                }
                self.refresh_tracking_from_scan();
            }
            !self.tracked.is_empty()
        }

        fn refresh_tracking_from_scan(&mut self) {
            if let Some(processes) = scan_processes() {
                self.refresh_tracking(&processes);
            }
        }

        fn refresh_tracking(&mut self, processes: &[Process]) {
            let by_pid = processes
                .iter()
                .map(|process| (process.identity.pid, process))
                .collect::<BTreeMap<_, _>>();
            self.tracked.retain(|pid, identity| {
                by_pid.get(pid).is_some_and(|process| {
                    process.identity == *identity
                        && process.user_id == self.user_id
                        && !protected_runtime(process)
                })
            });

            let user_id = self.user_id;
            let baseline = &self.baseline;
            let install_root = &self.install_root;
            let new_install_tree_processes = processes
                .iter()
                .filter(|process| {
                    process.user_id == user_id
                        && is_new_process(baseline, process.identity)
                        && !protected_runtime(process)
                        && has_install_tree_evidence(process, install_root, None)
                })
                .map(|process| process.identity)
                .collect::<Vec<_>>();
            for identity in new_install_tree_processes {
                self.tracked.insert(identity.pid, identity);
            }

            loop {
                let mut additions = Vec::new();
                for process in processes {
                    if process.user_id == self.user_id
                        && is_new_process(&self.baseline, process.identity)
                        && !protected_runtime(process)
                        && !self.tracked.contains_key(&process.identity.pid)
                        && self.tracked.contains_key(&process.parent_id)
                    {
                        additions.push(process.identity);
                    }
                }
                if additions.is_empty() {
                    break;
                }
                for identity in additions {
                    self.tracked.insert(identity.pid, identity);
                }
            }
            if !self.tracked.is_empty() {
                self.associated = true;
            }
        }

        fn tracked_descendant_first(&self, processes: &[Process]) -> Vec<Identity> {
            let by_pid = processes
                .iter()
                .map(|process| (process.identity.pid, process))
                .collect::<BTreeMap<_, _>>();
            let depth = |identity: Identity| {
                let mut current = identity.pid;
                let mut seen = BTreeSet::new();
                let mut depth = 0;
                while seen.insert(current) {
                    let Some(process) = by_pid.get(&current) else {
                        break;
                    };
                    if !self.tracked.contains_key(&process.parent_id) {
                        break;
                    }
                    current = process.parent_id;
                    depth += 1;
                }
                depth
            };
            let mut tracked = self.tracked.values().copied().collect::<Vec<_>>();
            tracked.sort_by_key(|identity| std::cmp::Reverse(depth(*identity)));
            tracked
        }
    }

    fn is_new_process(baseline: &BTreeMap<u32, u64>, identity: Identity) -> bool {
        baseline
            .get(&identity.pid)
            .is_none_or(|start_ticks| *start_ticks != identity.start_ticks)
    }

    fn scan_processes() -> Option<Vec<Process>> {
        let directory = fs::read_dir("/proc").ok()?;
        let user_id = effective_user_id();
        let mut processes = Vec::new();
        for (index, entry) in directory.enumerate() {
            if index >= MAX_PROCESS_SCAN {
                return None;
            }
            let entry = entry.ok()?;
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|name| name.parse::<u32>().ok())
            else {
                continue;
            };
            let process_directory = entry.path();
            let Some((parent_id, start_ticks)) = read_identity(&process_directory, pid) else {
                continue;
            };
            let Some(process_user) = read_user_id(&process_directory) else {
                continue;
            };
            if process_user != user_id {
                continue;
            }
            let executable = fs::read_link(process_directory.join("exe")).ok();
            let working_directory = fs::read_link(process_directory.join("cwd")).ok();
            processes.push(Process {
                identity: Identity { pid, start_ticks },
                user_id: process_user,
                parent_id,
                executable,
                working_directory,
            });
        }
        Some(processes)
    }

    fn read_identity(directory: &Path, pid: u32) -> Option<(u32, u64)> {
        let stat = fs::read_to_string(directory.join("stat")).ok()?;
        let command_end = stat.rfind(')')?;
        let mut fields = stat[command_end + 1..].split_whitespace();
        let _state = fields.next()?;
        let parent_id = fields.next()?.parse().ok()?;
        // `starttime` is stat field 22; fields here begin at field 3.
        let start_ticks = fields.nth(17)?.parse().ok()?;
        (pid != 0).then_some((parent_id, start_ticks))
    }

    fn read_user_id(directory: &Path) -> Option<u32> {
        let status = fs::read_to_string(directory.join("status")).ok()?;
        status.lines().find_map(|line| {
            line.strip_prefix("Uid:")?
                .split_whitespace()
                .next()?
                .parse()
                .ok()
        })
    }

    fn has_install_tree_evidence(
        process: &Process,
        install_root: &Path,
        command_line_path: Option<&Path>,
    ) -> bool {
        // Arguments are user-controlled text and never establish root ownership;
        // descendants are attributed only from a proven executable/cwd candidate.
        let _untrusted_command_line_path = command_line_path;
        let inside =
            |candidate: &Path| candidate != install_root && candidate.starts_with(install_root);
        process.executable.as_deref().is_some_and(inside)
            || process.working_directory.as_deref().is_some_and(inside)
    }

    fn protected_runtime(process: &Process) -> bool {
        let Some(executable) = process.executable.as_deref() else {
            return true;
        };
        let Some(name) = executable.file_name().and_then(|name| name.to_str()) else {
            return true;
        };
        let name = name.to_ascii_lowercase();
        matches!(
            name.as_str(),
            "steam"
                | "steam.sh"
                | "steamwebhelper"
                | "steamservice"
                | "steam-runtime-launcher-service"
                | "pressure-vessel-wrap"
                | "pressure-vessel-adverb"
                | "pv-bwrap"
                | "pv-adverb"
                | "bwrap"
                | "reaper"
                | "proton"
                | "wine"
                | "wine64"
                | "wineserver"
        ) || name.starts_with("steam-runtime-")
            || name.starts_with("pressure-vessel-")
            || name.starts_with("pv-")
    }

    fn signal_identity(identity: Identity, signal: i32, user_id: u32) -> io::Result<bool> {
        if effective_user_id() != user_id {
            return Ok(false);
        }
        // SAFETY: pidfd_open returns an owned descriptor for this PID identity.
        // Signals below use that descriptor, never a numeric PID after validation.
        let raw_pidfd = unsafe { libc::syscall(libc::SYS_pidfd_open, identity.pid, 0) as i32 };
        if raw_pidfd < 0 {
            return Ok(false);
        }
        // SAFETY: successful pidfd_open returned a fresh descriptor owned here.
        let pidfd = unsafe { OwnedFd::from_raw_fd(raw_pidfd) };
        let proc_directory = Path::new("/proc").join(identity.pid.to_string());
        let Some(current) = read_process(&proc_directory, identity.pid) else {
            return Ok(false);
        };
        if !signal_target_is_current(identity, &current, user_id) {
            return Ok(false);
        }
        // SAFETY: pidfd refers to the already validated process identity; null siginfo
        // requests the kernel's default signal delivery through pidfd_send_signal.
        let result = unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                pidfd.as_raw_fd(),
                signal,
                std::ptr::null::<c_void>(),
                0,
            ) as i32
        };
        if result == 0 {
            Ok(true)
        } else {
            let error = io::Error::last_os_error();
            if matches!(error.raw_os_error(), Some(libc::ESRCH) | Some(libc::EPERM)) {
                Ok(false)
            } else {
                Err(error)
            }
        }
    }

    fn effective_user_id() -> u32 {
        // SAFETY: geteuid has no pointer arguments or preconditions.
        unsafe { libc::geteuid() }
    }

    fn read_process(directory: &Path, pid: u32) -> Option<Process> {
        let (parent_id, start_ticks) = read_identity(directory, pid)?;
        let user_id = read_user_id(directory)?;
        Some(Process {
            identity: Identity { pid, start_ticks },
            user_id,
            parent_id,
            executable: fs::read_link(directory.join("exe")).ok(),
            working_directory: fs::read_link(directory.join("cwd")).ok(),
        })
    }

    fn signal_target_is_current(expected: Identity, process: &Process, user_id: u32) -> bool {
        process.identity == expected && process.user_id == user_id && !protected_runtime(process)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn synthetic_process(pid: u32, parent_id: u32, path: &str, user_id: u32) -> Process {
            Process {
                identity: Identity {
                    pid,
                    start_ticks: u64::from(pid),
                },
                user_id,
                parent_id,
                executable: Some(PathBuf::from(path)),
                working_directory: None,
            }
        }

        fn tracker() -> Tracker {
            Tracker {
                install_root: PathBuf::from("/games/title"),
                user_id: 1000,
                baseline: BTreeMap::from([(10, 100), (11, 200)]),
                tracked: BTreeMap::new(),
                started_at: Instant::now(),
                associated: false,
                stopping_at: None,
            }
        }

        #[test]
        fn unrelated_same_user_processes_are_not_associated() {
            let mut tracker = tracker();
            let processes = [synthetic_process(12, 1, "/usr/bin/unrelated", 1000)];
            tracker.refresh_tracking(&processes);
            assert!(tracker.tracked.is_empty());
        }

        #[test]
        fn an_in_tree_command_line_argument_does_not_associate_an_outside_process() {
            let mut tracker = tracker();
            let process = synthetic_process(12, 1, "/usr/bin/unrelated", 1000);
            assert!(!has_install_tree_evidence(
                &process,
                &tracker.install_root,
                Some(Path::new("/games/title/game")),
            ));
            tracker.refresh_tracking(&[process]);
            assert!(tracker.tracked.is_empty());
        }

        #[test]
        fn only_new_install_tree_processes_and_their_descendants_are_associated() {
            let mut tracker = tracker();
            let processes = [
                synthetic_process(10, 1, "/games/title/game", 1000),
                synthetic_process(12, 1, "/games/title/bin/game", 1000),
                synthetic_process(13, 12, "/usr/bin/child", 1000),
                synthetic_process(14, 12, "/games/other/game", 1000),
                synthetic_process(15, 1, "/games/title/other-user", 1001),
            ];
            tracker.refresh_tracking(&processes);
            assert_eq!(
                tracker.tracked.keys().copied().collect::<Vec<_>>(),
                [12, 13]
            );
        }

        #[test]
        fn pid_reuse_does_not_preserve_a_baseline_process_identity() {
            let mut tracker = tracker();
            let reused = Process {
                identity: Identity {
                    pid: 10,
                    start_ticks: 999,
                },
                ..synthetic_process(10, 1, "/games/title/reused", 1000)
            };
            tracker.refresh_tracking(&[reused]);
            assert_eq!(tracker.tracked.get(&10).unwrap().start_ticks, 999);
        }

        #[test]
        fn a_reused_tracked_pid_outside_the_install_tree_is_removed_before_stop() {
            let mut tracker = tracker();
            tracker.refresh_tracking(&[synthetic_process(12, 1, "/games/title/game", 1000)]);
            assert!(tracker.tracked.contains_key(&12));

            tracker.refresh_tracking(&[Process {
                identity: Identity {
                    pid: 12,
                    start_ticks: 999,
                },
                ..synthetic_process(12, 1, "/usr/bin/unrelated", 1000)
            }]);
            assert!(!tracker.tracked.contains_key(&12));
        }

        #[test]
        fn steam_and_runtime_processes_are_never_selected_for_signaling() {
            let mut tracker = tracker();
            let processes = [
                synthetic_process(12, 1, "/games/title/game", 1000),
                synthetic_process(13, 12, "/usr/bin/steamwebhelper", 1000),
                synthetic_process(14, 12, "/usr/bin/pressure-vessel-adverb", 1000),
            ];
            tracker.refresh_tracking(&processes);
            assert_eq!(tracker.tracked.keys().copied().collect::<Vec<_>>(), [12]);
        }

        #[test]
        fn stop_revalidation_rejects_pid_reuse_and_steam_runtime_execs() {
            let expected = Identity {
                pid: 12,
                start_ticks: 11,
            };
            let reused = synthetic_process(12, 1, "/games/title/game", 1000);
            assert!(!signal_target_is_current(expected, &reused, 1000));

            let mut runtime = synthetic_process(12, 1, "/games/title/game", 1000);
            runtime.executable = Some(PathBuf::from("/usr/bin/steamwebhelper"));
            assert!(!signal_target_is_current(expected, &runtime, 1000));

            let wrong_user = synthetic_process(12, 1, "/games/title/game", 1001);
            assert!(!signal_target_is_current(expected, &wrong_user, 1000));
        }

        #[test]
        fn missing_or_root_install_directory_disables_process_tracking() {
            assert!(Tracker::begin("/path/that/does/not/exist").is_none());
            assert!(Tracker::begin("/").is_none());
        }
    }
}
