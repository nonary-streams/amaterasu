//! Best-effort association of a newly launched Steam game process.
//! Steam's client remains outside the managed process group; only game PIDs
//! attributed by install-tree evidence can be signaled.
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SteamGameLiveness {
    Unknown,
    Searching,
    #[cfg_attr(not(any(target_os = "linux", windows)), allow(dead_code))]
    Associated,
    Stopping,
}

const STOP_GRACE: Duration = Duration::from_secs(2);
const STOP_CONFIRMATION_TIMEOUT: Duration = Duration::from_secs(10);

/// Retirement requires a successful snapshot proving no attributed game remains.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SteamStopProgress {
    Pending,
    Complete,
    Failed(&'static str),
}

#[cfg(test)]
std::thread_local! {
    static FAIL_NEXT_PROCESS_SCAN: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
pub(crate) fn fail_next_process_scan() {
    FAIL_NEXT_PROCESS_SCAN.set(true);
}

pub(crate) struct SteamProcessTracker {
    #[cfg(target_os = "linux")]
    inner: linux::Tracker,
    #[cfg(windows)]
    inner: windows::Tracker,
}

impl SteamProcessTracker {
    pub(crate) fn begin(install_directory: &str, steam_id: Option<&str>) -> Option<Self> {
        #[cfg(target_os = "linux")]
        {
            linux::Tracker::begin(install_directory, steam_id).map(|inner| Self { inner })
        }
        #[cfg(windows)]
        {
            let _ = steam_id;
            windows::Tracker::begin(install_directory).map(|inner| Self { inner })
        }
        #[cfg(not(any(target_os = "linux", windows)))]
        {
            let _ = (install_directory, steam_id);
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
    pub(crate) fn request_stop(&mut self, now: Instant) -> SteamStopProgress {
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
            SteamStopProgress::Complete
        }
    }

    /// Advance bounded TERM/KILL without discarding unconfirmed process ownership.
    pub(crate) fn advance_stop(&mut self, now: Instant) -> SteamStopProgress {
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
            SteamStopProgress::Complete
        }
    }

    pub(crate) fn stop_budget() -> Duration {
        STOP_GRACE + STOP_CONFIRMATION_TIMEOUT
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
    mod scanner;
    use super::{
        Instant, STOP_CONFIRMATION_TIMEOUT, STOP_GRACE, SteamGameLiveness, SteamStopProgress,
    };
    use scanner::ProcessScanner;
    use std::collections::{BTreeMap, BTreeSet};
    use std::ffi::c_void;
    use std::fs;
    use std::io::{self, Read};
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    const MAX_PROCESS_SCAN: usize = 131_072;
    const GAME_ASSOCIATION_WINDOW: Duration = Duration::from_secs(15);

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
        steam_app_id: Option<String>,
    }

    #[derive(Default)]
    struct ProcessSnapshot {
        processes: Vec<Process>,
        indeterminate_pids: BTreeMap<u32, Option<Identity>>,
    }

    impl From<Vec<Process>> for ProcessSnapshot {
        fn from(processes: Vec<Process>) -> Self {
            Self {
                processes,
                indeterminate_pids: BTreeMap::new(),
            }
        }
    }

    pub(super) struct Tracker {
        install_root: PathBuf,
        steam_id: Option<String>,
        user_id: u32,
        baseline: BTreeMap<u32, Option<u64>>,
        tracked: BTreeMap<u32, Identity>,
        started_at: Instant,
        next_scan_at: Instant,
        scanner: ProcessScanner,
        scan_failed: bool,
        associated: bool,
        stopping_at: Option<Instant>,
    }

    impl Tracker {
        pub(super) fn begin(install_directory: &str, steam_id: Option<&str>) -> Option<Self> {
            let install_root = Path::new(install_directory).canonicalize().ok()?;
            if !install_root.is_dir() || install_root == Path::new("/") {
                return None;
            }
            let user_id = effective_user_id();
            let snapshot = scan_processes(&install_root)?;
            let baseline = snapshot
                .processes
                .into_iter()
                .filter(|process| process.user_id == user_id)
                .map(|process| (process.identity.pid, Some(process.identity.start_ticks)))
                .chain(
                    snapshot
                        .indeterminate_pids
                        .into_iter()
                        .map(|(pid, identity)| {
                            (pid, identity.map(|identity| identity.start_ticks))
                        }),
                )
                .collect();
            Some(Self {
                scanner: ProcessScanner::start(install_root.clone())?,
                install_root,
                steam_id: steam_id.map(str::to_owned),
                user_id,
                baseline,
                tracked: BTreeMap::new(),
                started_at: Instant::now(),
                next_scan_at: Instant::now(),
                scan_failed: false,
                associated: false,
                stopping_at: None,
            })
        }

        pub(super) fn observe(&mut self, now: Instant) -> SteamGameLiveness {
            if let Some(snapshot) = self.scanner.poll() {
                self.scan_failed = snapshot.is_none();
                if let Some(processes) = snapshot {
                    self.refresh_snapshot(&processes);
                }
            }
            if now >= self.next_scan_at {
                self.scan_failed |= !self.scanner.request();
                self.next_scan_at = now + Duration::from_millis(250);
            }
            if self.scan_failed {
                return if self.associated {
                    SteamGameLiveness::Associated
                } else {
                    SteamGameLiveness::Unknown
                };
            }
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

        pub(super) fn request_stop(&mut self, now: Instant) -> SteamStopProgress {
            let install_root = self.install_root.clone();
            self.request_stop_with(
                now,
                || scan_processes(&install_root),
                |identity, signal, user_id| {
                    let _ = signal_identity(identity, signal, user_id);
                },
            )
        }

        fn request_stop_with(
            &mut self,
            now: Instant,
            mut scan: impl FnMut() -> Option<ProcessSnapshot>,
            mut signal: impl FnMut(Identity, i32, u32),
        ) -> SteamStopProgress {
            // Retain the stop obligation even when its first snapshot fails, so
            // an explicit cleanup retry returns to this tracker.
            self.stopping_at = Some(now + STOP_GRACE);
            let Some(processes) = scan() else {
                return SteamStopProgress::Failed("steam_process_scan_failed");
            };
            self.refresh_snapshot(&processes);
            if self.tracked.is_empty() {
                self.stopping_at = None;
                return SteamStopProgress::Complete;
            }
            for identity in self.tracked_descendant_first(&processes.processes) {
                signal(identity, libc::SIGTERM, self.user_id);
            }
            SteamStopProgress::Pending
        }

        pub(super) fn advance_stop(&mut self, now: Instant) -> SteamStopProgress {
            let install_root = self.install_root.clone();
            self.advance_stop_with(
                now,
                || scan_processes(&install_root),
                |identity, signal, user_id| {
                    let _ = signal_identity(identity, signal, user_id);
                },
            )
        }

        fn advance_stop_with(
            &mut self,
            now: Instant,
            mut scan: impl FnMut() -> Option<ProcessSnapshot>,
            mut signal: impl FnMut(Identity, i32, u32),
        ) -> SteamStopProgress {
            let Some(processes) = scan() else {
                // An unreadable process table proves neither signal authority
                // nor retirement. Preserve exact identities for a later retry.
                return if self
                    .stopping_at
                    .is_some_and(|deadline| now >= deadline + STOP_CONFIRMATION_TIMEOUT)
                {
                    SteamStopProgress::Failed("steam_process_scan_failed")
                } else {
                    SteamStopProgress::Pending
                };
            };
            self.refresh_snapshot(&processes);
            if self.tracked.is_empty() {
                self.stopping_at = None;
                return SteamStopProgress::Complete;
            }
            let Some(deadline) = self.stopping_at else {
                return SteamStopProgress::Pending;
            };
            if now >= deadline + STOP_CONFIRMATION_TIMEOUT {
                let error = if self.tracked.keys().any(|pid| {
                    processes.indeterminate_pids.contains_key(pid)
                        || processes.processes.iter().any(|process| {
                            process.identity.pid == *pid && process.executable.is_none()
                        })
                }) {
                    "steam_process_scan_failed"
                } else {
                    "steam_process_stop_timed_out"
                };
                return SteamStopProgress::Failed(error);
            }
            if now >= deadline {
                for identity in self.tracked_descendant_first(&processes.processes) {
                    signal(identity, libc::SIGKILL, self.user_id);
                }
                if let Some(processes) = scan() {
                    self.refresh_snapshot(&processes);
                    if self.tracked.is_empty() {
                        self.stopping_at = None;
                        return SteamStopProgress::Complete;
                    }
                }
            }
            SteamStopProgress::Pending
        }

        #[cfg(test)]
        fn refresh_tracking(&mut self, processes: &[Process]) {
            self.refresh_snapshot(&processes.to_vec().into());
        }

        fn refresh_snapshot(&mut self, snapshot: &ProcessSnapshot) {
            let processes = &snapshot.processes;
            let by_pid = processes
                .iter()
                .map(|process| (process.identity.pid, process))
                .collect::<BTreeMap<_, _>>();
            self.tracked.retain(|pid, identity| {
                snapshot
                    .indeterminate_pids
                    .get(pid)
                    .is_some_and(|observed| observed.is_none_or(|observed| observed == *identity))
                    || by_pid.get(pid).is_some_and(|process| {
                        process.identity == *identity
                            && process.user_id == self.user_id
                            && (process.executable.is_none() || !protected_runtime(process))
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
                        && has_install_tree_evidence(
                            process,
                            install_root,
                            self.steam_id.as_deref(),
                            None,
                        )
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
                        && self
                            .tracked
                            .get(&process.parent_id)
                            .is_some_and(|identity| {
                                by_pid.get(&process.parent_id).is_some_and(|parent| {
                                    signal_target_is_current(*identity, parent, self.user_id)
                                })
                            })
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
            let mut tracked = self
                .tracked
                .values()
                .copied()
                .filter(|identity| {
                    by_pid.get(&identity.pid).is_some_and(|process| {
                        signal_target_is_current(*identity, process, self.user_id)
                    })
                })
                .collect::<Vec<_>>();
            tracked.sort_by_key(|identity| std::cmp::Reverse(depth(*identity)));
            tracked
        }
    }

    fn is_new_process(baseline: &BTreeMap<u32, Option<u64>>, identity: Identity) -> bool {
        match baseline.get(&identity.pid) {
            None => true,
            Some(Some(start_ticks)) => *start_ticks != identity.start_ticks,
            Some(None) => false,
        }
    }

    fn scan_processes(install_root: &Path) -> Option<ProcessSnapshot> {
        #[cfg(test)]
        if super::FAIL_NEXT_PROCESS_SCAN.replace(false) {
            return None;
        }
        scan_processes_in(install_root, Path::new("/proc"), effective_user_id())
    }

    fn scan_processes_in(
        install_root: &Path,
        proc_root: &Path,
        user_id: u32,
    ) -> Option<ProcessSnapshot> {
        let directory = fs::read_dir(proc_root).ok()?;
        let mut snapshot = ProcessSnapshot::default();
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
            let Some((parent_id, start_ticks, exited)) = read_identity(&process_directory, pid)
            else {
                snapshot.indeterminate_pids.insert(pid, None);
                continue;
            };
            // Zombies and dead tasks have conclusively exited even while an
            // external parent still owns their wait/reap operation.
            if exited {
                continue;
            }
            let Some(process_user) = read_user_id(&process_directory) else {
                snapshot
                    .indeterminate_pids
                    .insert(pid, Some(Identity { pid, start_ticks }));
                continue;
            };
            if process_user != user_id {
                continue;
            }
            let executable = fs::read_link(process_directory.join("exe")).ok();
            let working_directory = fs::read_link(process_directory.join("cwd")).ok();
            let steam_app_id = (working_directory.as_deref() == Some(install_root))
                .then(|| read_steam_app_id(&process_directory))
                .flatten();
            snapshot.processes.push(Process {
                identity: Identity { pid, start_ticks },
                user_id: process_user,
                parent_id,
                executable,
                working_directory,
                steam_app_id,
            });
        }
        Some(snapshot)
    }

    fn read_steam_app_id(directory: &Path) -> Option<String> {
        const MAX_ENVIRONMENT_BYTES: u64 = 128 * 1024;
        let mut bytes = Vec::new();
        fs::File::open(directory.join("environ"))
            .ok()?
            .take(MAX_ENVIRONMENT_BYTES + 1)
            .read_to_end(&mut bytes)
            .ok()?;
        if bytes.len() as u64 > MAX_ENVIRONMENT_BYTES {
            return None;
        }
        let mut ids = bytes
            .split(|byte| *byte == 0)
            .filter_map(|entry| entry.strip_prefix(b"SteamAppId="));
        let id = std::str::from_utf8(ids.next()?).ok()?;
        if ids.next().is_some() || id.is_empty() || !id.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        Some(id.to_owned())
    }

    fn read_identity(directory: &Path, pid: u32) -> Option<(u32, u64, bool)> {
        let stat = fs::read_to_string(directory.join("stat")).ok()?;
        let command_end = stat.rfind(')')?;
        let mut fields = stat[command_end + 1..].split_whitespace();
        let exited = matches!(fields.next()?, "Z" | "X" | "x");
        let parent_id = fields.next()?.parse().ok()?;
        // `starttime` is stat field 22; fields here begin at field 3.
        let start_ticks = fields.nth(17)?.parse().ok()?;
        (pid != 0).then_some((parent_id, start_ticks, exited))
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
        steam_id: Option<&str>,
        command_line_path: Option<&Path>,
    ) -> bool {
        // Arguments are user-controlled text and never establish root ownership;
        // descendants are attributed only from a proven executable/cwd candidate.
        let _untrusted_command_line_path = command_line_path;
        let inside =
            |candidate: &Path| candidate != install_root && candidate.starts_with(install_root);
        process.executable.as_deref().is_some_and(inside)
            || process.working_directory.as_deref().is_some_and(inside)
            || (process.working_directory.as_deref() == Some(install_root)
                && steam_id.is_some()
                && process.steam_app_id.as_deref() == steam_id)
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
        let (parent_id, start_ticks, exited) = read_identity(directory, pid)?;
        if exited {
            return None;
        }
        let user_id = read_user_id(directory)?;
        Some(Process {
            identity: Identity { pid, start_ticks },
            user_id,
            parent_id,
            executable: fs::read_link(directory.join("exe")).ok(),
            working_directory: fs::read_link(directory.join("cwd")).ok(),
            steam_app_id: None,
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
                steam_app_id: None,
            }
        }

        fn tracker() -> Tracker {
            Tracker {
                install_root: PathBuf::from("/games/title"),
                steam_id: Some("42".into()),
                user_id: 1000,
                baseline: BTreeMap::from([(10, Some(100)), (11, Some(200))]),
                tracked: BTreeMap::new(),
                started_at: Instant::now(),
                next_scan_at: Instant::now(),
                scanner: ProcessScanner::start(PathBuf::from("/games/title")).unwrap(),
                scan_failed: false,
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
                Some("42"),
                Some(Path::new("/games/title/game")),
            ));
            tracker.refresh_tracking(&[process]);
            assert!(tracker.tracked.is_empty());
        }

        #[test]
        fn install_root_cwd_alone_does_not_attribute_a_launcher() {
            let mut process = synthetic_process(12, 1, "/usr/bin/launcher", 1000);
            process.working_directory = Some(PathBuf::from("/games/title"));
            assert!(!has_install_tree_evidence(
                &process,
                Path::new("/games/title"),
                Some("42"),
                None,
            ));
            process.working_directory = Some(PathBuf::from("/games/title/game"));
            assert!(has_install_tree_evidence(
                &process,
                Path::new("/games/title"),
                Some("42"),
                None,
            ));
        }

        #[test]
        fn root_cwd_requires_the_matching_steam_app_id() {
            let mut tracker = tracker();
            let mut game = synthetic_process(12, 1, "/proton/wine64-preloader", 1000);
            game.working_directory = Some(PathBuf::from("/games/title"));
            game.steam_app_id = Some("43".into());
            tracker.refresh_tracking(std::slice::from_ref(&game));
            assert!(tracker.tracked.is_empty());

            game.steam_app_id = Some("42".into());
            tracker.refresh_tracking(&[game]);
            assert!(tracker.tracked.contains_key(&12));
        }

        #[test]
        fn only_new_install_tree_processes_and_their_descendants_are_associated() {
            let mut tracker = tracker();
            let baseline_process = Process {
                identity: Identity {
                    pid: 10,
                    start_ticks: 100,
                },
                ..synthetic_process(10, 1, "/games/title/game", 1000)
            };
            let processes = [
                baseline_process,
                synthetic_process(12, 1, "/games/title/bin/game", 1000),
                synthetic_process(13, 12, "/usr/bin/child", 1000),
                synthetic_process(14, 12, "/games/other/game", 1000),
                synthetic_process(15, 1, "/games/title/other-user", 1001),
            ];
            tracker.refresh_tracking(&processes);
            assert_eq!(
                tracker.tracked.keys().copied().collect::<Vec<_>>(),
                [12, 13, 14]
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
        fn stop_keeps_live_identities_after_the_confirmation_deadline() {
            let mut tracker = tracker();
            let game = synthetic_process(12, 1, "/games/title/game", 1000);
            tracker.refresh_tracking(std::slice::from_ref(&game));
            let deadline = Instant::now();
            tracker.stopping_at = Some(deadline);
            let pending = tracker.advance_stop_with(
                deadline + STOP_CONFIRMATION_TIMEOUT,
                || Some(vec![game.clone()].into()),
                |_, _, _| panic!("an expired stop attempt must not signal again"),
            );
            assert_eq!(
                pending,
                SteamStopProgress::Failed("steam_process_stop_timed_out")
            );
            assert_eq!(tracker.tracked.get(&12), Some(&game.identity));
            assert!(tracker.is_stopping());
        }

        #[test]
        fn stop_cannot_confirm_retirement_without_a_snapshot() {
            let mut tracker = tracker();
            let game = synthetic_process(12, 1, "/games/title/game", 1000);
            tracker.refresh_tracking(std::slice::from_ref(&game));
            let deadline = Instant::now();
            tracker.stopping_at = Some(deadline);
            let pending = tracker.advance_stop_with(
                deadline + STOP_CONFIRMATION_TIMEOUT,
                || None,
                |_, _, _| panic!("unreadable snapshots must never authorize signals"),
            );
            assert_eq!(
                pending,
                SteamStopProgress::Failed("steam_process_scan_failed")
            );
            assert_eq!(tracker.tracked.get(&12), Some(&game.identity));
            assert!(tracker.is_stopping());
        }

        #[test]
        fn stop_scan_failure_retains_a_retryable_attempt() {
            let mut tracker = tracker();
            let game = synthetic_process(12, 1, "/games/title/game", 1000);
            tracker.refresh_tracking(std::slice::from_ref(&game));
            let pending = tracker.request_stop_with(
                Instant::now(),
                || None,
                |_, _, _| panic!("unreadable snapshots must never authorize signals"),
            );
            assert_eq!(
                pending,
                SteamStopProgress::Failed("steam_process_scan_failed")
            );
            assert_eq!(tracker.tracked.get(&12), Some(&game.identity));
            assert!(tracker.is_stopping());
        }

        #[test]
        fn stop_preserves_unreadable_owned_processes_without_signaling_them() {
            for readable_identity in [false, true] {
                let mut tracker = tracker();
                let game = synthetic_process(12, 1, "/games/title/game", 1000);
                tracker.refresh_tracking(std::slice::from_ref(&game));
                let deadline = Instant::now();
                tracker.stopping_at = Some(deadline);
                let snapshot = || ProcessSnapshot {
                    processes: vec![synthetic_process(13, 12, "/usr/bin/unverified-child", 1000)],
                    indeterminate_pids: BTreeMap::from([(
                        12,
                        readable_identity.then_some(game.identity),
                    )]),
                };
                assert_eq!(
                    tracker.advance_stop_with(
                        deadline,
                        || Some(snapshot()),
                        |_, _, _| panic!(
                            "unreadable ownership cannot authorize a signal or a descendant"
                        ),
                    ),
                    SteamStopProgress::Pending,
                );
                assert_eq!(tracker.tracked, BTreeMap::from([(12, game.identity)]));
                assert_eq!(
                    tracker.advance_stop_with(
                        deadline + STOP_CONFIRMATION_TIMEOUT,
                        || Some(snapshot()),
                        |_, _, _| panic!("the expired attempt must not signal"),
                    ),
                    SteamStopProgress::Failed("steam_process_scan_failed"),
                );
                assert_eq!(tracker.tracked, BTreeMap::from([(12, game.identity)]));
                assert!(tracker.is_stopping());
                assert_eq!(
                    tracker.request_stop_with(
                        deadline + STOP_CONFIRMATION_TIMEOUT,
                        || Some(ProcessSnapshot::default()),
                        |_, _, _| panic!("confirmed absence must not signal"),
                    ),
                    SteamStopProgress::Complete,
                );
            }
        }

        #[test]
        fn stop_preserves_an_owned_process_with_an_unreadable_executable() {
            let mut tracker = tracker();
            let mut game = synthetic_process(12, 1, "/games/title/game", 1000);
            tracker.refresh_tracking(std::slice::from_ref(&game));
            game.executable = None;
            let now = Instant::now();
            assert_eq!(
                tracker.request_stop_with(
                    now,
                    || Some(vec![game.clone()].into()),
                    |_, _, _| panic!("an unreadable executable must not authorize signals"),
                ),
                SteamStopProgress::Pending,
            );
            assert_eq!(tracker.tracked.get(&12), Some(&game.identity));
        }

        #[test]
        fn a_readable_reused_identity_retires_the_old_process_despite_unreadable_user() {
            let mut tracker = tracker();
            tracker.refresh_tracking(&[synthetic_process(12, 1, "/games/title/game", 1000)]);
            assert_eq!(
                tracker.request_stop_with(
                    Instant::now(),
                    || Some(ProcessSnapshot {
                        processes: Vec::new(),
                        indeterminate_pids: BTreeMap::from([(
                            12,
                            Some(Identity {
                                pid: 12,
                                start_ticks: 999
                            }),
                        )]),
                    }),
                    |_, _, _| panic!("a reused PID must not be signaled"),
                ),
                SteamStopProgress::Complete,
            );
            assert!(tracker.tracked.is_empty());
        }

        #[test]
        fn exited_proc_states_retire_the_game_without_user_or_executable_metadata() {
            let root = std::env::temp_dir().join(format!(
                "amaterasu-steam-proc-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
            ));
            let directory = root.join("12");
            fs::create_dir_all(&directory).unwrap();
            for state in ["R", "Z", "X", "x"] {
                let mut fields = vec!["0"; 20];
                fields[0] = state;
                fields[1] = "1";
                fields[19] = "12";
                fs::write(
                    directory.join("stat"),
                    format!("12 (game) {}", fields.join(" ")),
                )
                .unwrap();
                let snapshot = scan_processes_in(Path::new("/games/title"), &root, 1000).unwrap();
                let mut tracker = tracker();
                let game = synthetic_process(12, 1, "/games/title/game", 1000);
                tracker.refresh_tracking(std::slice::from_ref(&game));
                tracker.refresh_snapshot(&snapshot);
                if state == "R" {
                    assert_eq!(tracker.tracked.get(&12), Some(&game.identity));
                    assert_eq!(
                        snapshot.indeterminate_pids.get(&12),
                        Some(&Some(game.identity))
                    );
                } else {
                    assert!(tracker.tracked.is_empty(), "{state} must count as exited");
                    assert!(snapshot.indeterminate_pids.is_empty());
                }
            }
            fs::remove_dir_all(&root).unwrap();
        }

        #[test]
        fn an_unreadable_baseline_pid_does_not_later_become_a_new_game() {
            let mut tracker = tracker();
            tracker.baseline.insert(12, None);
            tracker.refresh_tracking(&[synthetic_process(12, 1, "/games/title/game", 1000)]);
            assert!(tracker.tracked.is_empty());
        }

        #[test]
        fn stop_waits_for_an_observed_exit_after_kill() {
            let mut tracker = tracker();
            let game = synthetic_process(12, 1, "/games/title/game", 1000);
            let child = synthetic_process(13, 12, "/usr/bin/game-child", 1000);
            let processes = vec![game.clone(), child.clone()];
            let now = Instant::now();
            let mut signals = Vec::new();
            assert_eq!(
                tracker.request_stop_with(
                    now,
                    || Some(processes.clone().into()),
                    |identity, signal, user_id| signals.push((identity.pid, signal, user_id)),
                ),
                SteamStopProgress::Pending,
            );
            assert_eq!(
                signals,
                [(13, libc::SIGTERM, 1000), (12, libc::SIGTERM, 1000)]
            );

            signals.clear();
            let mut snapshots = [Some(processes.into()), None].into_iter();
            assert_eq!(
                tracker.advance_stop_with(
                    now + STOP_GRACE,
                    || snapshots.next().unwrap(),
                    |identity, signal, user_id| signals.push((identity.pid, signal, user_id)),
                ),
                SteamStopProgress::Pending,
            );
            assert_eq!(
                signals,
                [(13, libc::SIGKILL, 1000), (12, libc::SIGKILL, 1000)]
            );
            assert_eq!(tracker.tracked.get(&12), Some(&game.identity));
            assert_eq!(tracker.tracked.get(&13), Some(&child.identity));
            assert!(tracker.is_stopping());

            assert_eq!(
                tracker.advance_stop_with(
                    now + STOP_GRACE + Duration::from_millis(1),
                    || Some(ProcessSnapshot::default()),
                    |_, _, _| panic!("an empty snapshot must not authorize signals"),
                ),
                SteamStopProgress::Complete,
            );
            assert!(!tracker.is_stopping());
        }

        #[test]
        fn stop_retry_revalidates_retained_identities_before_signaling() {
            let mut tracker = tracker();
            let game = synthetic_process(12, 1, "/games/title/game", 1000);
            tracker.refresh_tracking(std::slice::from_ref(&game));
            let now = Instant::now();
            tracker.stopping_at = Some(now);
            assert_eq!(
                tracker.advance_stop_with(
                    now + STOP_CONFIRMATION_TIMEOUT,
                    || Some(vec![game.clone()].into()),
                    |_, _, _| panic!("the expired attempt must not signal"),
                ),
                SteamStopProgress::Failed("steam_process_stop_timed_out"),
            );
            let reused = Process {
                identity: Identity {
                    pid: 12,
                    start_ticks: 999,
                },
                ..synthetic_process(12, 1, "/usr/bin/unrelated", 1000)
            };
            assert_eq!(
                tracker.request_stop_with(
                    now + STOP_CONFIRMATION_TIMEOUT,
                    || Some(vec![reused.clone()].into()),
                    |_, _, _| panic!("a reused PID must not receive a retry signal"),
                ),
                SteamStopProgress::Complete,
            );
            assert!(tracker.tracked.is_empty());
            assert!(!tracker.is_stopping());
        }

        #[test]
        fn missing_or_root_install_directory_disables_process_tracking() {
            assert!(Tracker::begin("/path/that/does/not/exist", Some("42")).is_none());
            assert!(Tracker::begin("/", Some("42")).is_none());
        }
    }
}
