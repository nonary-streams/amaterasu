//! Bounded retries for focusing the game reported by Playnite's trusted extension.
use std::time::{Duration, Instant};

const FOCUS_RETRY_INTERVAL: Duration = Duration::from_secs(1);
const MAX_FOCUS_PATH_BYTES: usize = 4_096;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FocusPolicy {
    pub attempts: u32,
    pub timeout_secs: u32,
    pub exit_on_first: bool,
}

impl Default for FocusPolicy {
    fn default() -> Self {
        Self {
            attempts: 3,
            timeout_secs: 15,
            exit_on_first: false,
        }
    }
}

impl FocusPolicy {
    pub const fn disabled() -> Self {
        Self {
            attempts: 0,
            timeout_secs: 0,
            exit_on_first: false,
        }
    }

    fn enabled(self) -> bool {
        self.attempts > 0 && self.timeout_secs > 0
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FocusTarget {
    pub install_directory: Option<String>,
    pub executable: Option<String>,
}

impl FocusTarget {
    pub fn from_status_paths(install_directory: Option<&str>, executable: Option<&str>) -> Self {
        let executable = match executable {
            Some(path) => match bounded_absolute_path(path, false) {
                Some(path) => Some(path),
                None => return Self::default(),
            },
            None => None,
        };
        Self {
            install_directory: install_directory.and_then(|path| bounded_absolute_path(path, true)),
            executable,
        }
    }

    fn merge_nonempty(&mut self, update: Self) {
        if update.install_directory.is_some() {
            self.install_directory = update.install_directory;
        }
        if update.executable.is_some() {
            self.executable = update.executable;
        }
    }
}

fn bounded_absolute_path(value: &str, allow_directory: bool) -> Option<String> {
    let path = value.trim();
    if path.is_empty()
        || path.len() > MAX_FOCUS_PATH_BYTES
        || path.chars().any(char::is_control)
        || path.contains('"')
    {
        return None;
    }
    let normalized = path.replace('/', "\\");
    let bytes = normalized.as_bytes();
    let drive_path =
        bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && bytes[2] == b'\\';
    let unc_path = normalized.starts_with(r"\\")
        && normalized[2..]
            .split('\\')
            .filter(|component| !component.is_empty())
            .count()
            >= 2;
    if !drive_path && !unc_path {
        return None;
    }
    if allow_directory && drive_path && normalized.trim_end_matches('\\').len() <= 2 {
        return None;
    }
    Some(normalized)
}

#[derive(Default)]
pub struct FocusRetry {
    policy: FocusPolicyState,
    target: FocusTarget,
    started_at: Option<Instant>,
    last_attempt_at: Option<Instant>,
    successes_remaining: u32,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct FocusPolicyState {
    policy: FocusPolicy,
    active: bool,
}

impl FocusRetry {
    pub fn start(&mut self, policy: FocusPolicy, target: FocusTarget, now: Instant) {
        self.cancel();
        if !policy.enabled() {
            return;
        }
        self.policy = FocusPolicyState {
            policy,
            active: true,
        };
        self.successes_remaining = if policy.exit_on_first {
            1
        } else {
            policy.attempts
        };
        self.target = target;
        self.started_at = Some(now);
    }

    pub fn observe_target(&mut self, update: FocusTarget) {
        if self.policy.active {
            self.target.merge_nonempty(update);
            // Keep the fixed deadline and one-attempt-per-second limit even if
            // the extension updates target metadata repeatedly.
        }
    }

    pub fn cancel(&mut self) {
        self.policy = FocusPolicyState::default();
        self.target = FocusTarget::default();
        self.started_at = None;
        self.last_attempt_at = None;
        self.successes_remaining = 0;
    }

    pub fn tick_with(
        &mut self,
        now: Instant,
        mut attempt_focus: impl FnMut(&FocusTarget) -> bool,
    ) -> bool {
        if !self.policy.active {
            return false;
        }
        let Some(started_at) = self.started_at else {
            self.cancel();
            return false;
        };
        if now.saturating_duration_since(started_at)
            >= Duration::from_secs(u64::from(self.policy.policy.timeout_secs))
        {
            self.cancel();
            return false;
        }
        if self
            .last_attempt_at
            .is_some_and(|last| now.saturating_duration_since(last) < FOCUS_RETRY_INTERVAL)
        {
            return false;
        }

        self.last_attempt_at = Some(now);
        if !attempt_focus(&self.target) {
            return false;
        }
        self.successes_remaining = self.successes_remaining.saturating_sub(1);
        if self.policy.policy.exit_on_first || self.successes_remaining == 0 {
            self.cancel();
            true
        } else {
            false
        }
    }

    #[cfg(windows)]
    pub fn attempt_windows_focus(target: &FocusTarget) -> bool {
        windows_focus::attempt(target)
    }

    #[cfg(not(windows))]
    pub fn attempt_windows_focus(_target: &FocusTarget) -> bool {
        false
    }
}

#[cfg(windows)]
mod windows_focus {
    use super::{FocusTarget, matches_image_path};
    use std::collections::BTreeMap;
    use std::thread;
    use std::time::Duration;
    use windows_sys::Win32::Foundation::{CloseHandle, HWND, LPARAM};
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GW_OWNER, GetForegroundWindow, GetWindow, GetWindowThreadProcessId, IsIconic,
        IsWindowVisible, SetForegroundWindow,
    };
    use windows_sys::core::BOOL;

    struct WindowSearch<'a> {
        target: &'a FocusTarget,
        image_paths: BTreeMap<u32, Option<String>>,
        candidate: Option<(HWND, u32)>,
        visited: usize,
    }

    pub fn attempt(target: &FocusTarget) -> bool {
        if target.install_directory.is_none() && target.executable.is_none() {
            return false;
        }
        let mut search = WindowSearch {
            target,
            image_paths: BTreeMap::new(),
            candidate: None,
            visited: 0,
        };
        // SAFETY: EnumWindows invokes the callback synchronously; search remains
        // alive and exclusively borrowed for the complete call.
        unsafe { EnumWindows(Some(find_matching_window), &mut search as *mut _ as LPARAM) };
        let Some((window, process_id)) = search.candidate else {
            return false;
        };
        if foreground_process_id() == Some(process_id) {
            return true;
        }
        // SAFETY: window came from EnumWindows and its process image matched the
        // bounded target reported by the authenticated Playnite extension.
        if unsafe { SetForegroundWindow(window) } == 0 {
            return false;
        }
        thread::sleep(Duration::from_millis(100));
        foreground_process_id() == Some(process_id)
    }

    fn foreground_process_id() -> Option<u32> {
        // SAFETY: GetForegroundWindow returns the current window handle.
        let foreground = unsafe { GetForegroundWindow() };
        if foreground.is_null() {
            return None;
        }
        let mut foreground_pid = 0;
        // SAFETY: foreground is a current window handle returned by Windows.
        if unsafe { GetWindowThreadProcessId(foreground, &mut foreground_pid) } == 0 {
            return None;
        }
        Some(foreground_pid)
    }

    unsafe extern "system" fn find_matching_window(window: HWND, context: LPARAM) -> BOOL {
        // SAFETY: context points to the live WindowSearch passed to synchronous
        // EnumWindows above.
        let search = unsafe { &mut *(context as *mut WindowSearch<'_>) };
        search.visited += 1;
        if search.visited > 512 {
            return 0;
        }
        // SAFETY: window is supplied by EnumWindows for this callback.
        let visible = unsafe { IsWindowVisible(window) } != 0;
        // SAFETY: window is supplied by EnumWindows for this callback.
        let minimized = unsafe { IsIconic(window) } != 0;
        // SAFETY: window is supplied by EnumWindows for this callback.
        let owned = !unsafe { GetWindow(window, GW_OWNER) }.is_null();
        if !visible || minimized || owned {
            return 1;
        }
        let mut process_id = 0;
        // SAFETY: the callback window is valid for the duration of enumeration.
        if unsafe { GetWindowThreadProcessId(window, &mut process_id) } == 0 || process_id == 0 {
            return 1;
        }
        let image_path = search
            .image_paths
            .entry(process_id)
            .or_insert_with(|| query_image_path(process_id));
        if image_path
            .as_deref()
            .is_some_and(|path| matches_image_path(path, search.target))
        {
            search.candidate = Some((window, process_id));
            return 0;
        }
        1
    }

    fn query_image_path(process_id: u32) -> Option<String> {
        // SAFETY: process_id came from an OS-owned window; query access cannot
        // write to or launch the process.
        let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, process_id) };
        if process.is_null() {
            return None;
        }
        let mut path = [0u16; 32_768];
        let mut length = path.len() as u32;
        // SAFETY: process is valid and path is writable for length UTF-16 units.
        let queried =
            unsafe { QueryFullProcessImageNameW(process, 0, path.as_mut_ptr(), &mut length) };
        // SAFETY: this function owns the process handle returned by OpenProcess.
        unsafe { CloseHandle(process) };
        if queried == 0 || length as usize > path.len() {
            return None;
        }
        Some(String::from_utf16_lossy(&path[..length as usize]))
    }
}

fn matches_image_path(image_path: &str, target: &FocusTarget) -> bool {
    let image_path = normalize_windows_path(image_path);
    if let Some(executable) = target.executable.as_deref() {
        return image_path == normalize_windows_path(executable);
    }
    target
        .install_directory
        .as_deref()
        .is_some_and(|directory| {
            let directory = normalize_windows_path(directory);
            image_path
                .strip_prefix(&directory)
                .is_some_and(|suffix| suffix.starts_with('\\'))
        })
}

fn normalize_windows_path(path: &str) -> String {
    path.replace('/', "\\")
        .trim_start_matches(r"\\?\")
        .trim_end_matches('\\')
        .to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::{FocusPolicy, FocusRetry, FocusTarget, matches_image_path};
    use std::time::{Duration, Instant};

    fn target() -> FocusTarget {
        FocusTarget {
            install_directory: Some(r"C:\Games\Sample".into()),
            executable: Some(r"C:\Games\Sample\game.exe".into()),
        }
    }

    #[test]
    fn focus_retries_until_confirmed_attempt_budget_is_spent() {
        let started = Instant::now();
        let mut retry = FocusRetry::default();
        retry.start(FocusPolicy::default(), target(), started);
        let mut attempts = 0;
        for second in 0..3 {
            let complete = retry.tick_with(started + Duration::from_secs(second), |_| {
                attempts += 1;
                true
            });
            assert_eq!(complete, second == 2);
        }
        assert!(!retry.tick_with(started + Duration::from_secs(4), |_| panic!("retry ended")));
        assert_eq!(attempts, 3);
    }

    #[test]
    fn first_success_policy_stops_after_one_confirmed_focus() {
        let started = Instant::now();
        let mut retry = FocusRetry::default();
        retry.start(
            FocusPolicy {
                exit_on_first: true,
                ..FocusPolicy::default()
            },
            target(),
            started,
        );
        assert!(retry.tick_with(started, |_| true));
        assert!(!retry.tick_with(started + Duration::from_secs(1), |_| panic!("retry ended")));
    }

    #[test]
    fn zero_attempts_or_timeout_disables_focus_and_timeout_cancels_retries() {
        let started = Instant::now();
        for policy in [
            FocusPolicy::disabled(),
            FocusPolicy {
                attempts: 3,
                timeout_secs: 0,
                exit_on_first: false,
            },
        ] {
            let mut retry = FocusRetry::default();
            retry.start(policy, target(), started);
            assert!(!retry.tick_with(started, |_| panic!("disabled policy attempted focus")));
        }

        let mut retry = FocusRetry::default();
        retry.start(
            FocusPolicy {
                attempts: 3,
                timeout_secs: 2,
                exit_on_first: false,
            },
            target(),
            started,
        );
        let mut attempts = 0;
        retry.tick_with(started, |_| {
            attempts += 1;
            false
        });
        retry.tick_with(started + Duration::from_secs(1), |_| {
            attempts += 1;
            false
        });
        retry.tick_with(started + Duration::from_secs(2), |_| {
            attempts += 1;
            false
        });
        assert_eq!(attempts, 2);
    }

    #[test]
    fn frequent_status_target_updates_cannot_bypass_the_attempt_interval() {
        let started = Instant::now();
        let mut retry = FocusRetry::default();
        retry.start(FocusPolicy::default(), target(), started);
        let mut attempts = 0;
        retry.tick_with(started, |_| {
            attempts += 1;
            false
        });
        for _ in 0..10 {
            retry.observe_target(FocusTarget::from_status_paths(
                Some(r"C:\Games\Sample"),
                Some(r"C:\Games\Sample\game.exe"),
            ));
        }
        retry.tick_with(started + Duration::from_millis(500), |_| {
            attempts += 1;
            false
        });
        retry.tick_with(started + Duration::from_secs(1), |_| {
            attempts += 1;
            false
        });
        assert_eq!(attempts, 2);
    }

    #[test]
    fn target_updates_do_not_extend_deadline_or_reset_success_budget() {
        let started = Instant::now();
        let mut retry = FocusRetry::default();
        retry.start(
            FocusPolicy {
                attempts: 2,
                timeout_secs: 2,
                exit_on_first: false,
            },
            FocusTarget::default(),
            started,
        );
        assert!(!retry.tick_with(started, |_| false));
        assert!(!retry.tick_with(started + Duration::from_millis(1_500), |_| false));
        retry.observe_target(FocusTarget::from_status_paths(
            Some(r"C:\Games\Sample"),
            Some(r"C:\Games\Sample\game.exe"),
        ));
        retry.tick_with(started + Duration::from_secs(2), |_| {
            panic!("deadline elapsed")
        });
    }

    #[test]
    fn status_paths_are_bounded_absolute_and_match_only_target_game_processes() {
        let target = FocusTarget::from_status_paths(
            Some(r"C:/Games/Sample/"),
            Some(r"C:/Games/Sample/game.exe"),
        );
        assert_eq!(
            target.install_directory.as_deref(),
            Some(r"C:\Games\Sample\")
        );
        assert!(target.executable.is_some());
        assert!(
            FocusTarget::from_status_paths(Some(r"C:\"), None)
                .install_directory
                .is_none()
        );
        let invalid_path = format!("C:\\Games\\{}Bad", char::from(10));
        assert!(
            FocusTarget::from_status_paths(Some(&invalid_path), None)
                .install_directory
                .is_none()
        );
        assert!(matches_image_path(r"C:\Games\Sample\game.exe", &target));
        assert!(!matches_image_path(r"C:\Games\Sample\helper.exe", &target));
        assert!(!matches_image_path(
            r"C:\Games\SampleOther\game.exe",
            &target
        ));
        assert!(!matches_image_path(r"D:\Other\game.exe", &target));

        let directory_only = FocusTarget::from_status_paths(Some(r"C:\Games\Sample"), None);
        assert!(matches_image_path(
            r"C:\Games\Sample\subdir\game.exe",
            &directory_only
        ));
        assert!(!matches_image_path(
            r"C:\Games\SampleOther\game.exe",
            &directory_only
        ));
        assert_eq!(
            FocusTarget::from_status_paths(Some(r"C:\Games\Sample"), Some(r"..\game.exe")),
            FocusTarget::default(),
            "an invalid supplied executable must not broaden matching to the install directory"
        );
    }
}
