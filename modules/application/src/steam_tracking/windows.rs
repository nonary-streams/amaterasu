#[cfg(test)]
use super::TEST_STOP_GRACE;
use super::{STOP_CONFIRMATION_TIMEOUT, SteamGameLiveness, SteamStopProgress};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_NO_MORE_FILES, FILETIME, GetLastError, HANDLE, HWND, INVALID_HANDLE_VALUE,
    LPARAM, WAIT_TIMEOUT,
};
use windows_sys::Win32::Globalization::{CSTR_EQUAL, CompareStringOrdinal};
use windows_sys::Win32::Security::{
    GetLengthSid, GetTokenInformation, TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetProcessTimes, OpenProcess, OpenProcessToken,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
    QueryFullProcessImageNameW, TerminateProcess, WaitForSingleObject,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowThreadProcessId, PostMessageW, WM_CLOSE,
};

const MAX_PROCESS_COUNT: usize = 131_072;
const MAX_TOKEN_INFORMATION_BYTES: u32 = 64 * 1024;
const PROCESS_IMAGE_BUFFER_UNITS: usize = 32_768;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Identity {
    pid: u32,
    creation_time: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Process {
    identity: Option<Identity>,
    parent_id: u32,
    user_sid: Option<Vec<u8>>,
    image: Option<PathBuf>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct TrackedProcess {
    identity: Identity,
    parent_id: u32,
    user_sid: Vec<u8>,
    image: PathBuf,
}

pub(super) struct Tracker {
    install_root: PathBuf,
    user_sid: Vec<u8>,
    baseline: BTreeMap<u32, Option<u64>>,
    tracked: BTreeMap<u32, TrackedProcess>,
    started_at: Instant,
    associated: bool,
    stopping_at: Option<Instant>,
    stop_grace: Duration,
    shutdown_deadline: Option<Instant>,
}

impl Tracker {
    #[cfg(test)]
    pub(super) fn expire_association_window_for_test(&mut self, now: Instant) {
        self.started_at = now
            .checked_sub(super::GAME_ASSOCIATION_WINDOW + Duration::from_secs(1))
            .unwrap_or(now);
    }

    pub(super) fn begin(install_directory: &str) -> Option<Self> {
        let install_root = canonical_install_directory(install_directory)?;
        let processes = scan_processes()?;
        let user_sid = current_process_user_sid()?;
        let baseline = processes
            .iter()
            .map(|(pid, process)| {
                (
                    *pid,
                    process.identity.map(|identity| identity.creation_time),
                )
            })
            .collect();

        Some(Self {
            install_root,
            user_sid,
            baseline,
            tracked: BTreeMap::new(),
            started_at: Instant::now(),
            associated: false,
            stopping_at: None,
            stop_grace: Duration::ZERO,
            shutdown_deadline: None,
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
            || now.saturating_duration_since(self.started_at) >= super::GAME_ASSOCIATION_WINDOW
        {
            SteamGameLiveness::Unknown
        } else {
            SteamGameLiveness::Searching
        }
    }

    pub(super) fn was_associated(&self) -> bool {
        self.associated
    }

    pub(super) fn prepare_shutdown(&mut self, deadline: Instant) {
        let deadline = self
            .shutdown_deadline
            .map_or(deadline, |current| current.min(deadline));
        self.shutdown_deadline = Some(deadline);
        if let Some(stopping_at) = &mut self.stopping_at {
            *stopping_at = (*stopping_at).min(
                deadline
                    .checked_sub(STOP_CONFIRMATION_TIMEOUT)
                    .unwrap_or(deadline),
            );
        }
    }

    pub(super) fn is_stopping(&self) -> bool {
        self.stopping_at.is_some()
    }

    pub(super) fn request_stop(&mut self, now: Instant, grace: Duration) -> SteamStopProgress {
        self.request_stop_with(now, grace, scan_processes, request_graceful_close)
    }

    fn request_stop_with(
        &mut self,
        now: Instant,
        grace: Duration,
        mut scan: impl FnMut() -> Option<BTreeMap<u32, Process>>,
        mut close: impl FnMut(&TrackedProcess, &[u8]),
    ) -> SteamStopProgress {
        self.stop_grace = grace;
        let requested = match self.shutdown_deadline {
            Some(deadline) => (now + grace).min(
                deadline
                    .checked_sub(STOP_CONFIRMATION_TIMEOUT)
                    .unwrap_or(deadline),
            ),
            None => now + grace,
        };
        self.stopping_at = Some(if self.shutdown_deadline.is_some() {
            self.stopping_at
                .map_or(requested, |current| current.min(requested))
        } else {
            requested
        });
        let Some(processes) = scan() else {
            return SteamStopProgress::Failed("steam_process_scan_failed");
        };
        self.refresh_tracking(&processes);
        if self.tracked.is_empty() {
            if !self.associated && now < self.started_at + super::GAME_ASSOCIATION_WINDOW {
                // Steam can accept the launch request before creating the game.
                // Keep the stop obligation through the bounded association window.
                return SteamStopProgress::Pending;
            }
            self.stopping_at = None;
            return SteamStopProgress::Complete;
        }
        for process in self.tracked_descendant_first(&processes) {
            close(&process, &self.user_sid);
        }
        SteamStopProgress::Pending
    }

    pub(super) fn force_shutdown(&mut self) {
        self.force_shutdown_with(scan_processes, terminate_validated_process);
    }

    fn force_shutdown_with(
        &mut self,
        mut scan: impl FnMut() -> Option<BTreeMap<u32, Process>>,
        mut terminate: impl FnMut(&TrackedProcess, &[u8]),
    ) {
        let Some(snapshot) = scan() else {
            return;
        };
        // Retained exact identities are the only destructor authority.
        self.tracked.retain(|pid, tracked| {
            snapshot.get(pid).is_some_and(|process| {
                process_may_still_be_tracked(tracked, process, &self.user_sid)
            })
        });
        for process in self.tracked_descendant_first(&snapshot) {
            terminate(&process, &self.user_sid);
        }
    }

    pub(super) fn advance_stop(&mut self, now: Instant) -> SteamStopProgress {
        self.advance_stop_with_actions(
            now,
            scan_processes,
            request_graceful_close,
            terminate_validated_process,
        )
    }

    fn advance_stop_with(
        &mut self,
        now: Instant,
        scan: impl FnMut() -> Option<BTreeMap<u32, Process>>,
        mut terminate: impl FnMut(&TrackedProcess, &[u8]),
    ) -> SteamStopProgress {
        self.advance_stop_with_actions(
            now,
            scan,
            |_, _| {},
            |process, user_sid| terminate(process, user_sid),
        )
    }

    fn advance_stop_with_actions(
        &mut self,
        now: Instant,
        mut scan: impl FnMut() -> Option<BTreeMap<u32, Process>>,
        mut close: impl FnMut(&TrackedProcess, &[u8]),
        mut terminate: impl FnMut(&TrackedProcess, &[u8]),
    ) -> SteamStopProgress {
        let Some(processes) = scan() else {
            return if self
                .stopping_at
                .is_some_and(|deadline| now >= deadline + STOP_CONFIRMATION_TIMEOUT)
            {
                SteamStopProgress::Failed("steam_process_scan_failed")
            } else {
                SteamStopProgress::Pending
            };
        };
        let was_associated = self.associated;
        self.refresh_tracking(&processes);
        if self.tracked.is_empty() {
            if !self.associated && now < self.started_at + super::GAME_ASSOCIATION_WINDOW {
                return SteamStopProgress::Pending;
            }
            self.stopping_at = None;
            return SteamStopProgress::Complete;
        }
        if !was_associated {
            // Start the configured grace when the first late game is discovered,
            // then retain the existing graceful-close/terminate confirmation path.
            self.stopping_at = Some(self.stop_deadline(now));
            for process in self.tracked_descendant_first(&processes) {
                close(&process, &self.user_sid);
            }
        }
        let Some(deadline) = self.stopping_at else {
            return SteamStopProgress::Pending;
        };
        if now >= deadline + STOP_CONFIRMATION_TIMEOUT {
            let error = if self.tracked.iter().any(|(pid, tracked)| {
                processes.get(pid).is_some_and(|process| {
                    !process_matches_tracked(tracked, process, &self.user_sid)
                })
            }) {
                "steam_process_scan_failed"
            } else {
                "steam_process_stop_timed_out"
            };
            return SteamStopProgress::Failed(error);
        }
        if now >= deadline {
            for process in self.tracked_descendant_first(&processes) {
                terminate(&process, &self.user_sid);
            }
            if let Some(processes) = scan() {
                self.refresh_tracking(&processes);
                if self.tracked.is_empty() {
                    self.stopping_at = None;
                    return SteamStopProgress::Complete;
                }
            }
        }
        SteamStopProgress::Pending
    }

    fn stop_deadline(&self, now: Instant) -> Instant {
        let requested = now + self.stop_grace;
        self.shutdown_deadline.map_or(requested, |deadline| {
            requested.min(
                deadline
                    .checked_sub(STOP_CONFIRMATION_TIMEOUT)
                    .unwrap_or(deadline),
            )
        })
    }

    fn refresh_tracking(&mut self, processes: &BTreeMap<u32, Process>) {
        self.tracked.retain(|pid, tracked| {
            processes.get(pid).is_some_and(|process| {
                process_may_still_be_tracked(tracked, process, &self.user_sid)
            })
        });

        for process in processes.values() {
            let Some(identity) = process.identity else {
                continue;
            };
            let Some(user_sid) = process.user_sid.as_ref() else {
                continue;
            };
            let Some(image) = process.image.as_ref() else {
                continue;
            };
            if user_sid != &self.user_sid
                || !is_new_process(&self.baseline, identity)
                || protected_runtime(image)
                || !path_is_within(image, &self.install_root)
            {
                continue;
            }
            self.tracked.insert(
                identity.pid,
                TrackedProcess {
                    identity,
                    parent_id: process.parent_id,
                    user_sid: user_sid.clone(),
                    image: image.clone(),
                },
            );
        }

        loop {
            let additions = processes
                .values()
                .filter_map(|process| {
                    let identity = process.identity?;
                    let user_sid = process.user_sid.as_ref()?;
                    let image = process.image.as_ref()?;
                    (user_sid == &self.user_sid
                        && is_new_process(&self.baseline, identity)
                        && !protected_runtime(image)
                        && !self.tracked.contains_key(&identity.pid)
                        && self
                            .tracked
                            .get(&process.parent_id)
                            .is_some_and(|tracked_parent| {
                                processes.get(&process.parent_id).is_some_and(|parent| {
                                    process_matches_tracked(tracked_parent, parent, &self.user_sid)
                                })
                            }))
                    .then(|| TrackedProcess {
                        identity,
                        parent_id: process.parent_id,
                        user_sid: user_sid.clone(),
                        image: image.clone(),
                    })
                })
                .collect::<Vec<_>>();
            if additions.is_empty() {
                break;
            }
            for process in additions {
                self.tracked.insert(process.identity.pid, process);
            }
        }
        if !self.tracked.is_empty() {
            self.associated = true;
        }
    }

    fn tracked_descendant_first(&self, snapshot: &BTreeMap<u32, Process>) -> Vec<TrackedProcess> {
        let depth = |process: &TrackedProcess| {
            let mut parent_id = process.parent_id;
            let mut visited = BTreeSet::new();
            let mut depth = 0usize;
            while visited.insert(parent_id) {
                let Some(parent) = self.tracked.get(&parent_id) else {
                    break;
                };
                parent_id = parent.parent_id;
                depth += 1;
            }
            depth
        };
        let mut processes = self
            .tracked
            .values()
            .filter(|tracked| {
                snapshot.get(&tracked.identity.pid).is_some_and(|process| {
                    process_matches_tracked(tracked, process, &self.user_sid)
                })
            })
            .cloned()
            .collect::<Vec<_>>();
        processes.sort_by_key(|process| {
            (
                std::cmp::Reverse(depth(process)),
                std::cmp::Reverse(process.identity.pid),
            )
        });
        processes
    }
}

fn is_new_process(baseline: &BTreeMap<u32, Option<u64>>, identity: Identity) -> bool {
    match baseline.get(&identity.pid) {
        None => true,
        Some(Some(previous_creation_time)) => *previous_creation_time != identity.creation_time,
        Some(None) => false,
    }
}

fn canonical_install_directory(value: &str) -> Option<PathBuf> {
    let install_root = Path::new(value).canonicalize().ok()?;
    if !install_root.is_dir()
        || !install_root
            .components()
            .any(|component| matches!(component, Component::Normal(_)))
    {
        return None;
    }
    Some(install_root)
}

fn path_is_within(path: &Path, root: &Path) -> bool {
    let mut path_components = path.components();
    let mut compared_root_components = 0usize;
    for root_component in root.components() {
        let Some(path_component) = path_components.next() else {
            return false;
        };
        if !windows_component_equal(path_component.as_os_str(), root_component.as_os_str()) {
            return false;
        }
        compared_root_components += 1;
    }
    compared_root_components > 0 && path_components.next().is_some()
}

fn paths_equal(left: &Path, right: &Path) -> bool {
    let mut left_components = left.components();
    let mut right_components = right.components();
    loop {
        match (left_components.next(), right_components.next()) {
            (Some(left), Some(right))
                if windows_component_equal(left.as_os_str(), right.as_os_str()) => {}
            (None, None) => return true,
            _ => return false,
        }
    }
}

fn windows_component_equal(left: &OsStr, right: &OsStr) -> bool {
    let left = left.encode_wide().collect::<Vec<_>>();
    let right = right.encode_wide().collect::<Vec<_>>();
    let (Ok(left_len), Ok(right_len)) = (i32::try_from(left.len()), i32::try_from(right.len()))
    else {
        return false;
    };
    // SAFETY: both UTF-16 buffers remain alive for the call and lengths are their
    // exact element counts; CompareStringOrdinal reads, but does not retain them.
    unsafe {
        CompareStringOrdinal(left.as_ptr(), left_len, right.as_ptr(), right_len, 1) == CSTR_EQUAL
    }
}

fn protected_runtime(image: &Path) -> bool {
    let Some(name) = image.file_name().and_then(OsStr::to_str) else {
        return true;
    };
    let name = name.to_ascii_lowercase();
    matches!(
        name.as_str(),
        "steam.exe"
            | "steamwebhelper.exe"
            | "steamservice.exe"
            | "steamservice_tmp.exe"
            | "steamerrorreporter.exe"
            | "steamerrorreporter64.exe"
            | "steammonitor.exe"
            | "gameoverlayui.exe"
            | "gameoverlayui64.exe"
    ) || name.starts_with("steam-runtime-")
}

/// Missing metadata cannot prove that a previously owned process retired.
fn process_may_still_be_tracked(
    tracked: &TrackedProcess,
    process: &Process,
    current_user_sid: &[u8],
) -> bool {
    tracked.user_sid == current_user_sid
        && !protected_runtime(&tracked.image)
        && process
            .identity
            .is_none_or(|identity| identity == tracked.identity)
        && process
            .user_sid
            .as_deref()
            .is_none_or(|sid| sid == tracked.user_sid.as_slice())
        && process
            .image
            .as_deref()
            .is_none_or(|image| paths_equal(image, &tracked.image))
}

fn process_matches_tracked(
    tracked: &TrackedProcess,
    process: &Process,
    current_user_sid: &[u8],
) -> bool {
    process.identity == Some(tracked.identity)
        && process.user_sid.as_deref() == Some(tracked.user_sid.as_slice())
        && tracked.user_sid == current_user_sid
        && process
            .image
            .as_deref()
            .is_some_and(|image| paths_equal(image, &tracked.image))
        && !protected_runtime(&tracked.image)
}

fn scan_processes() -> Option<BTreeMap<u32, Process>> {
    #[cfg(test)]
    if super::FAIL_NEXT_PROCESS_SCAN.replace(false) {
        return None;
    }
    let snapshot = OwnedHandle::new(unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) })?;
    let mut entry = PROCESSENTRY32W {
        dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    // SAFETY: `snapshot` is a live Toolhelp process snapshot and `entry` is a
    // writable structure initialized with the required size.
    if unsafe { Process32FirstW(snapshot.raw(), &mut entry) } == 0 {
        return None;
    }

    let mut processes = BTreeMap::new();
    loop {
        if processes.len() >= MAX_PROCESS_COUNT {
            return None;
        }
        let pid = entry.th32ProcessID;
        processes.insert(pid, read_process(pid, entry.th32ParentProcessID));

        // SAFETY: `snapshot` remains live and `entry` remains a valid writable
        // process-entry buffer for each synchronous enumeration call.
        if unsafe { Process32NextW(snapshot.raw(), &mut entry) } == 0 {
            // A partial or failed enumeration is not safe for baseline or stop
            // decisions; callers fail closed instead of using partial results.
            if unsafe { GetLastError() } != ERROR_NO_MORE_FILES {
                return None;
            }
            break;
        }
    }
    Some(processes)
}

fn read_process(pid: u32, parent_id: u32) -> Process {
    let Some(handle) = open_process(pid, PROCESS_QUERY_LIMITED_INFORMATION) else {
        return Process {
            identity: None,
            parent_id,
            user_sid: None,
            image: None,
        };
    };
    read_process_from_handle(handle.raw(), pid, parent_id)
}

fn read_process_from_handle(handle: HANDLE, pid: u32, parent_id: u32) -> Process {
    Process {
        identity: process_creation_time(handle)
            .map(|creation_time| Identity { pid, creation_time }),
        parent_id,
        user_sid: process_user_sid(handle),
        image: process_image(handle),
    }
}

fn process_creation_time(handle: HANDLE) -> Option<u64> {
    let mut creation = FILETIME::default();
    let mut exit = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    // SAFETY: the handle is live and each FILETIME pointer refers to writable
    // storage of the exact type required by GetProcessTimes.
    if unsafe { GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user) } == 0 {
        return None;
    }
    let value = (u64::from(creation.dwHighDateTime) << 32) | u64::from(creation.dwLowDateTime);
    (value != 0).then_some(value)
}

fn process_image(handle: HANDLE) -> Option<PathBuf> {
    let mut image = vec![0u16; PROCESS_IMAGE_BUFFER_UNITS];
    let mut image_len = u32::try_from(image.len()).ok()?;
    // SAFETY: `image` is writable storage sized by `image_len`; the handle is
    // live and QueryFullProcessImageNameW will not retain the buffer.
    if unsafe { QueryFullProcessImageNameW(handle, 0, image.as_mut_ptr(), &mut image_len) } == 0
        || image_len == 0
        || image_len as usize >= image.len()
    {
        return None;
    }
    let path = PathBuf::from(OsString::from_wide(&image[..image_len as usize]));
    path.canonicalize().ok()
}

fn process_user_sid(handle: HANDLE) -> Option<Vec<u8>> {
    let mut token = std::ptr::null_mut();
    // SAFETY: `handle` is a live process handle and `token` is writable output
    // storage; a successful call transfers one token handle to this function.
    if unsafe { OpenProcessToken(handle, TOKEN_QUERY, &mut token) } == 0 {
        return None;
    }
    let token = OwnedHandle::new(token)?;
    let mut required = 0u32;
    // SAFETY: the null buffer and zero length are the documented size-query
    // form of GetTokenInformation; `required` is writable output storage.
    unsafe {
        GetTokenInformation(
            token.raw(),
            TokenUser,
            std::ptr::null_mut(),
            0,
            &mut required,
        );
    }
    if required == 0 || required > MAX_TOKEN_INFORMATION_BYTES {
        return None;
    }
    let word_count = (required as usize).div_ceil(std::mem::size_of::<usize>());
    let mut storage = vec![0usize; word_count];
    let mut returned = 0u32;
    // SAFETY: Vec<usize> provides aligned storage at least `required` bytes
    // long. The token handle and output-length pointer remain live throughout.
    if unsafe {
        GetTokenInformation(
            token.raw(),
            TokenUser,
            storage.as_mut_ptr().cast(),
            required,
            &mut returned,
        )
    } == 0
        || returned < required
        || returned as usize > storage.len() * std::mem::size_of::<usize>()
    {
        return None;
    }

    // SAFETY: a successful TokenUser query initializes a TOKEN_USER at the
    // start of the aligned buffer; its SID pointer is valid only while storage
    // remains alive, so the SID bytes are copied before returning.
    let token_user = unsafe { &*(storage.as_ptr().cast::<TOKEN_USER>()) };
    let sid = token_user.User.Sid;
    if sid.is_null() {
        return None;
    }
    // SAFETY: `sid` came from the successful TokenUser result above and the
    // buffer remains live; GetLengthSid reads the SID header within that buffer.
    let sid_len = unsafe { GetLengthSid(sid) } as usize;
    let buffer_start = storage.as_ptr() as usize;
    let buffer_end = buffer_start.checked_add(storage.len() * std::mem::size_of::<usize>())?;
    let sid_start = sid as usize;
    if sid_len == 0
        || sid_start < buffer_start
        || sid_start.checked_add(sid_len)? > buffer_end
        || sid_start.checked_add(sid_len)? > buffer_start.checked_add(returned as usize)?
    {
        return None;
    }
    // SAFETY: the SID range was checked to lie entirely inside the live result
    // buffer; copy it so no pointer into the temporary token buffer escapes.
    Some(unsafe { std::slice::from_raw_parts(sid.cast::<u8>(), sid_len) }.to_vec())
}

fn current_process_user_sid() -> Option<Vec<u8>> {
    // SAFETY: GetCurrentProcess returns a stable pseudo-handle owned by Windows;
    // process_user_sid opens and closes only a separate token handle.
    process_user_sid(unsafe { GetCurrentProcess() })
}

struct OwnedHandle(HANDLE);

impl OwnedHandle {
    fn new(handle: HANDLE) -> Option<Self> {
        (!handle.is_null() && handle != INVALID_HANDLE_VALUE).then_some(Self(handle))
    }

    fn raw(&self) -> HANDLE {
        self.0
    }
}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: this wrapper is created only for a newly-owned handle and
        // closes it exactly once when the wrapper is dropped.
        unsafe { CloseHandle(self.0) };
    }
}

fn open_process(pid: u32, access: u32) -> Option<OwnedHandle> {
    // SAFETY: OpenProcess accepts the numeric process ID from the bounded
    // Toolhelp snapshot; success returns a handle owned by this wrapper.
    OwnedHandle::new(unsafe { OpenProcess(access, 0, pid) })
}

fn canonical_target_matches(tracked: &TrackedProcess, user_sid: &[u8], handle: HANDLE) -> bool {
    let current = read_process_from_handle(handle, tracked.identity.pid, tracked.parent_id);
    process_matches_tracked(tracked, &current, user_sid)
}

fn validated_process_handle(
    tracked: &TrackedProcess,
    user_sid: &[u8],
    access: u32,
) -> Option<OwnedHandle> {
    let handle = open_process(tracked.identity.pid, access)?;
    if canonical_target_matches(tracked, user_sid, handle.raw()) {
        Some(handle)
    } else {
        None
    }
}

fn request_graceful_close(tracked: &TrackedProcess, user_sid: &[u8]) {
    let access = PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE;
    let Some(handle) = validated_process_handle(tracked, user_sid, access) else {
        return;
    };
    // SAFETY: the process handle was validated against creation time, user SID,
    // and image path and remains open throughout synchronous window enumeration.
    if unsafe { WaitForSingleObject(handle.raw(), 0) } != WAIT_TIMEOUT {
        return;
    }
    let mut context = CloseWindowContext {
        pid: tracked.identity.pid,
        process_handle: handle.raw(),
    };
    // SAFETY: EnumWindows invokes the callback synchronously; `context` remains
    // alive and uniquely borrowed for the whole call. The validated process
    // handle remains owned until the enumeration has completed.
    unsafe {
        EnumWindows(
            Some(post_close_to_owned_process),
            (&mut context as *mut CloseWindowContext) as LPARAM,
        );
    }
}

struct CloseWindowContext {
    pid: u32,
    process_handle: HANDLE,
}

unsafe extern "system" fn post_close_to_owned_process(window: HWND, parameter: LPARAM) -> i32 {
    // SAFETY: the LPARAM is the unique pointer passed to synchronous EnumWindows
    // in request_graceful_close and remains valid until EnumWindows returns.
    let context = unsafe { &mut *(parameter as *mut CloseWindowContext) };
    let mut owner_pid = 0u32;
    // SAFETY: EnumWindows supplies a live HWND and owner_pid is writable output.
    unsafe { GetWindowThreadProcessId(window, &mut owner_pid) };
    if owner_pid != context.pid {
        return 1;
    }
    // The validated process handle pins the original process object. Confirm it
    // is still live and the window still belongs to that PID immediately before
    // posting, so a recycled PID cannot make a foreign window a stop target.
    // SAFETY: handle remains owned by the caller until the callback returns.
    if unsafe { WaitForSingleObject(context.process_handle, 0) } != WAIT_TIMEOUT {
        return 1;
    }
    let mut current_owner_pid = 0u32;
    // SAFETY: `window` is rechecked immediately before PostMessageW and the
    // output PID is a valid writable pointer.
    unsafe { GetWindowThreadProcessId(window, &mut current_owner_pid) };
    if current_owner_pid == context.pid {
        // SAFETY: the same live HWND is posted to only after the owner and
        // validated process object have both been checked above.
        unsafe { PostMessageW(window, WM_CLOSE, 0, 0) };
    }
    1
}

fn terminate_validated_process(tracked: &TrackedProcess, user_sid: &[u8]) {
    let access = PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE | PROCESS_TERMINATE;
    let Some(handle) = validated_process_handle(tracked, user_sid, access) else {
        return;
    };
    // SAFETY: this exact open process object passed creation-time, same-user,
    // and unchanged-image checks; termination uses its handle, never a PID.
    if unsafe { WaitForSingleObject(handle.raw(), 0) } == WAIT_TIMEOUT {
        // SAFETY: the validated handle has PROCESS_TERMINATE rights and remains
        // owned for the duration of the call.
        unsafe { TerminateProcess(handle.raw(), 1) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CURRENT_SID: &[u8] = &[1, 5, 21, 10];
    const OTHER_SID: &[u8] = &[1, 5, 21, 20];

    fn process(pid: u32, parent_id: u32, image: Option<&str>, sid: Option<&[u8]>) -> Process {
        Process {
            identity: Some(Identity {
                pid,
                creation_time: u64::from(pid) * 10,
            }),
            parent_id,
            user_sid: sid.map(<[u8]>::to_vec),
            image: image.map(PathBuf::from),
        }
    }

    fn tracker() -> Tracker {
        Tracker {
            install_root: PathBuf::from(r"C:\games\title"),
            user_sid: CURRENT_SID.to_vec(),
            baseline: BTreeMap::from([(10, Some(100)), (11, Some(110)), (12, None)]),
            tracked: BTreeMap::new(),
            started_at: Instant::now(),
            associated: false,
            stopping_at: None,
            stop_grace: Duration::ZERO,
            shutdown_deadline: None,
        }
    }

    fn owned_game() -> BTreeMap<u32, Process> {
        BTreeMap::from([(
            20,
            process(20, 1, Some(r"C:\games\title\game.exe"), Some(CURRENT_SID)),
        )])
    }

    #[test]
    fn final_shutdown_does_not_associate_a_later_independent_launch() {
        let mut tracker = tracker();
        tracker.refresh_tracking(&owned_game());
        assert_eq!(
            tracker.request_stop_with(
                Instant::now(),
                Duration::ZERO,
                || Some(BTreeMap::new()),
                |_, _| {}
            ),
            SteamStopProgress::Complete
        );
        let later = BTreeMap::from([(
            99,
            process(99, 1, Some(r"C:\games\title\game.exe"), Some(CURRENT_SID)),
        )]);
        tracker.force_shutdown_with(
            || Some(later.clone()),
            |_, _| panic!("a later independent launch is not owned"),
        );
        assert!(tracker.tracked.is_empty());
        assert!(!tracker.is_stopping());
    }

    #[test]
    fn final_shutdown_revalidates_only_retained_identities_without_renewing_time() {
        let mut tracker = tracker();
        let mut processes = owned_game();
        tracker.refresh_tracking(&processes);
        let tracked = tracker.tracked.clone();
        let deadline = Instant::now() - Duration::from_secs(20);
        tracker.stopping_at = Some(deadline);
        processes.insert(
            21,
            process(21, 20, Some(r"C:\tools\new-child.exe"), Some(CURRENT_SID)),
        );
        processes.insert(
            99,
            process(99, 1, Some(r"C:\games\title\game.exe"), Some(CURRENT_SID)),
        );
        let mut signaled = Vec::new();
        tracker.force_shutdown_with(
            || Some(processes.clone()),
            |process, _| signaled.push(process.identity),
        );
        assert_eq!(signaled, [tracked.get(&20).unwrap().identity]);
        assert_eq!(tracker.tracked, tracked);
        assert_eq!(tracker.stopping_at, Some(deadline));
    }

    #[test]
    fn configured_steam_grace_controls_force_and_confirmation_deadlines() {
        for seconds in [0, 1, 30, 300] {
            let mut tracker = tracker();
            let processes = owned_game();
            let now = Instant::now();
            let grace = Duration::from_secs(seconds);
            assert_eq!(
                tracker.request_stop_with(now, grace, || Some(processes.clone()), |_, _| {}),
                SteamStopProgress::Pending,
            );
            assert_eq!(tracker.stopping_at, Some(now + grace));
            let mut terminated = Vec::new();
            if !grace.is_zero() {
                assert_eq!(
                    tracker.advance_stop_with(
                        now + grace - Duration::from_millis(1),
                        || Some(processes.clone()),
                        |process, _| terminated.push(process.identity),
                    ),
                    SteamStopProgress::Pending,
                );
                assert!(terminated.is_empty());
            }
            assert_eq!(
                tracker.advance_stop_with(
                    now + grace,
                    || Some(processes.clone()),
                    |process, _| terminated.push(process.identity),
                ),
                SteamStopProgress::Pending,
            );
            assert_eq!(terminated.len(), 1);
            assert_eq!(
                tracker.advance_stop_with(
                    now + grace + STOP_CONFIRMATION_TIMEOUT,
                    || Some(processes.clone()),
                    |_, _| panic!("the expired attempt must not signal"),
                ),
                SteamStopProgress::Failed("steam_process_stop_timed_out"),
            );
            let retry_at = now + grace + STOP_CONFIRMATION_TIMEOUT;
            tracker.request_stop_with(retry_at, grace, || Some(processes.clone()), |_, _| {});
            assert_eq!(tracker.stopping_at, Some(retry_at + grace));
        }
    }

    #[test]
    fn cancellation_waits_for_and_stops_a_late_steam_game() {
        let mut tracker = tracker();
        let started_at = Instant::now();
        tracker.started_at = started_at;
        let cancelled_at = started_at + Duration::from_secs(1);
        assert_eq!(
            tracker.request_stop_with(
                cancelled_at,
                TEST_STOP_GRACE,
                || Some(BTreeMap::new()),
                |_, _| panic!("there is no process to close"),
            ),
            SteamStopProgress::Pending,
            "an empty snapshot during launch association cannot prove retirement",
        );
        assert!(tracker.is_stopping());

        let game = owned_game();
        let game_identity = game[&20].identity.unwrap();
        let discovered_at = cancelled_at + Duration::from_secs(1);
        let mut closed = Vec::new();
        let mut terminated = Vec::new();
        assert_eq!(
            tracker.advance_stop_with_actions(
                discovered_at,
                || Some(game.clone()),
                |process, _| closed.push(process.identity),
                |process, _| terminated.push(process.identity),
            ),
            SteamStopProgress::Pending,
        );
        assert_eq!(closed, [game_identity]);
        assert!(terminated.is_empty());
        assert_eq!(tracker.stopping_at, Some(discovered_at + TEST_STOP_GRACE));

        let force_at = discovered_at + TEST_STOP_GRACE;
        assert_eq!(
            tracker.advance_stop_with_actions(
                force_at - Duration::from_millis(1),
                || Some(game.clone()),
                |_, _| panic!("the late game must be closed only once"),
                |process, _| terminated.push(process.identity),
            ),
            SteamStopProgress::Pending,
        );
        assert!(terminated.is_empty());
        assert_eq!(
            tracker.advance_stop_with_actions(
                force_at,
                || Some(game.clone()),
                |_, _| panic!("the late game must be closed only once"),
                |process, _| terminated.push(process.identity),
            ),
            SteamStopProgress::Pending,
        );
        assert_eq!(terminated, [game_identity]);
        assert_eq!(
            tracker.advance_stop_with_actions(
                force_at + Duration::from_millis(1),
                || Some(BTreeMap::new()),
                |_, _| panic!("confirmed absence must not be closed"),
                |_, _| panic!("confirmed absence must not be terminated"),
            ),
            SteamStopProgress::Complete,
        );
        assert!(!tracker.is_stopping());
    }

    #[test]
    fn empty_stop_wait_is_bounded_by_the_association_window() {
        let mut tracker = tracker();
        let started_at = Instant::now();
        tracker.started_at = started_at;
        let cancelled_at = started_at + Duration::from_secs(1);
        assert_eq!(
            tracker.request_stop_with(
                cancelled_at,
                Duration::ZERO,
                || Some(BTreeMap::new()),
                |_, _| panic!("there is no process to close"),
            ),
            SteamStopProgress::Pending,
        );
        assert_eq!(
            tracker.advance_stop_with_actions(
                started_at + super::super::GAME_ASSOCIATION_WINDOW,
                || Some(BTreeMap::new()),
                |_, _| panic!("there is no process to close"),
                |_, _| panic!("there is no process to terminate"),
            ),
            SteamStopProgress::Complete,
        );
        assert!(!tracker.is_stopping());
    }

    #[test]
    fn stop_keeps_live_identities_after_the_confirmation_deadline() {
        let mut tracker = tracker();
        let processes = owned_game();
        tracker.refresh_tracking(&processes);
        let tracked = tracker.tracked.clone();
        let deadline = Instant::now();
        tracker.stopping_at = Some(deadline);
        assert_eq!(
            tracker.advance_stop_with(
                deadline + STOP_CONFIRMATION_TIMEOUT,
                || Some(processes.clone()),
                |_, _| panic!("the expired attempt must not signal"),
            ),
            SteamStopProgress::Failed("steam_process_stop_timed_out"),
        );
        assert_eq!(tracker.tracked, tracked);
        assert!(tracker.is_stopping());
    }

    #[test]
    fn stop_cannot_confirm_retirement_without_a_snapshot() {
        let mut tracker = tracker();
        tracker.refresh_tracking(&owned_game());
        let tracked = tracker.tracked.clone();
        let deadline = Instant::now();
        tracker.stopping_at = Some(deadline);
        assert_eq!(
            tracker.advance_stop_with(
                deadline + STOP_CONFIRMATION_TIMEOUT,
                || None,
                |_, _| panic!("unreadable snapshots must never authorize signals"),
            ),
            SteamStopProgress::Failed("steam_process_scan_failed"),
        );
        assert_eq!(tracker.tracked, tracked);
        assert!(tracker.is_stopping());
    }

    #[test]
    fn stop_scan_failure_retains_a_retryable_attempt() {
        let mut tracker = tracker();
        tracker.refresh_tracking(&owned_game());
        let tracked = tracker.tracked.clone();
        assert_eq!(
            tracker.request_stop_with(
                Instant::now(),
                TEST_STOP_GRACE,
                || None,
                |_, _| panic!("unreadable snapshots must never authorize signals"),
            ),
            SteamStopProgress::Failed("steam_process_scan_failed"),
        );
        assert_eq!(tracker.tracked, tracked);
        assert!(tracker.is_stopping());
    }

    #[test]
    fn stop_preserves_unreadable_owned_processes_without_signaling_them() {
        for missing_field in ["identity", "user", "image", "all"] {
            let mut tracker = tracker();
            let mut snapshot = owned_game();
            tracker.refresh_tracking(&snapshot);
            let tracked = tracker.tracked.clone();
            let game = snapshot.get_mut(&20).unwrap();
            if matches!(missing_field, "identity" | "all") {
                game.identity = None;
            }
            if matches!(missing_field, "user" | "all") {
                game.user_sid = None;
            }
            if matches!(missing_field, "image" | "all") {
                game.image = None;
            }
            snapshot.insert(
                21,
                process(
                    21,
                    20,
                    Some(r"C:\tools\unverified-child.exe"),
                    Some(CURRENT_SID),
                ),
            );
            let now = Instant::now();
            assert_eq!(
                tracker.request_stop_with(
                    now,
                    TEST_STOP_GRACE,
                    || Some(snapshot.clone()),
                    |_, _| panic!("unreadable ownership cannot authorize a signal or a descendant"),
                ),
                SteamStopProgress::Pending,
            );
            assert_eq!(tracker.tracked, tracked);
            assert_eq!(
                tracker.advance_stop_with(
                    now + TEST_STOP_GRACE + STOP_CONFIRMATION_TIMEOUT,
                    || Some(snapshot.clone()),
                    |_, _| panic!("the expired attempt must not signal"),
                ),
                SteamStopProgress::Failed("steam_process_scan_failed"),
            );
            assert_eq!(tracker.tracked, tracked);
            assert!(tracker.is_stopping());
            assert_eq!(
                tracker.request_stop_with(
                    now + TEST_STOP_GRACE + STOP_CONFIRMATION_TIMEOUT,
                    TEST_STOP_GRACE,
                    || Some(BTreeMap::new()),
                    |_, _| panic!("confirmed absence must not signal"),
                ),
                SteamStopProgress::Complete,
            );
        }
    }

    #[test]
    fn a_readable_reused_identity_retires_the_old_process_despite_unreadable_user() {
        let mut tracker = tracker();
        tracker.refresh_tracking(&owned_game());
        let mut reused = process(20, 1, None, None);
        reused.identity.as_mut().unwrap().creation_time += 1;
        assert_eq!(
            tracker.request_stop_with(
                Instant::now(),
                TEST_STOP_GRACE,
                || Some(BTreeMap::from([(20, reused.clone())])),
                |_, _| panic!("a reused PID must not be signaled"),
            ),
            SteamStopProgress::Complete,
        );
        assert!(tracker.tracked.is_empty());
    }

    #[test]
    fn stop_waits_for_an_observed_exit_after_termination() {
        let mut tracker = tracker();
        let now = Instant::now();
        let processes = owned_game();
        assert_eq!(
            tracker.request_stop_with(now, TEST_STOP_GRACE, || Some(processes.clone()), |_, _| {}),
            SteamStopProgress::Pending,
        );
        let tracked = tracker.tracked.clone();
        let mut snapshots = [Some(processes), None].into_iter();
        let mut terminated = Vec::new();
        assert_eq!(
            tracker.advance_stop_with(
                now + TEST_STOP_GRACE,
                || snapshots.next().unwrap(),
                |process, _| terminated.push(process.identity),
            ),
            SteamStopProgress::Pending,
        );
        assert_eq!(terminated, [tracked.get(&20).unwrap().identity]);
        assert_eq!(tracker.tracked, tracked);
        assert!(tracker.is_stopping());
        assert_eq!(
            tracker.advance_stop_with(
                now + TEST_STOP_GRACE + Duration::from_millis(1),
                || Some(BTreeMap::new()),
                |_, _| panic!("an empty snapshot must not authorize signals"),
            ),
            SteamStopProgress::Complete,
        );
        assert!(!tracker.is_stopping());
    }

    #[test]
    fn stop_retry_revalidates_retained_identities_before_signaling() {
        let mut tracker = tracker();
        let processes = owned_game();
        tracker.refresh_tracking(&processes);
        let now = Instant::now();
        tracker.stopping_at = Some(now);
        assert_eq!(
            tracker.advance_stop_with(
                now + STOP_CONFIRMATION_TIMEOUT,
                || Some(processes.clone()),
                |_, _| panic!("the expired attempt must not signal"),
            ),
            SteamStopProgress::Failed("steam_process_stop_timed_out"),
        );
        let mut reused = process(20, 1, Some(r"C:\tools\unrelated.exe"), Some(CURRENT_SID));
        reused.identity.as_mut().unwrap().creation_time += 1;
        assert_eq!(
            tracker.request_stop_with(
                now + STOP_CONFIRMATION_TIMEOUT,
                TEST_STOP_GRACE,
                || Some(BTreeMap::from([(20, reused.clone())])),
                |_, _| panic!("a reused PID must not receive a retry signal"),
            ),
            SteamStopProgress::Complete,
        );
        assert!(tracker.tracked.is_empty());
        assert!(!tracker.is_stopping());
    }

    #[test]
    fn unrelated_and_other_user_processes_are_not_associated() {
        let mut tracker = tracker();
        tracker.refresh_tracking(&BTreeMap::from([
            (
                20,
                process(20, 1, Some(r"C:\tools\editor.exe"), Some(CURRENT_SID)),
            ),
            (
                21,
                process(21, 1, Some(r"C:\games\title\game.exe"), Some(OTHER_SID)),
            ),
        ]));
        assert!(tracker.tracked.is_empty());
    }

    #[test]
    fn only_new_processes_with_verified_identity_and_image_can_be_roots() {
        let mut tracker = tracker();
        tracker.refresh_tracking(&BTreeMap::from([
            (
                10,
                process(
                    10,
                    1,
                    Some(r"C:\games\title\baseline.exe"),
                    Some(CURRENT_SID),
                ),
            ),
            (
                12,
                process(
                    12,
                    1,
                    Some(r"C:\games\title\unknown-baseline.exe"),
                    Some(CURRENT_SID),
                ),
            ),
            (
                20,
                process(20, 1, Some(r"C:\games\title\missing-user.exe"), None),
            ),
            (21, process(21, 1, None, Some(CURRENT_SID))),
        ]));
        assert!(tracker.tracked.is_empty());
    }

    #[test]
    fn a_reused_pid_with_new_verified_creation_time_is_a_new_process() {
        let mut tracker = tracker();
        let mut reused = process(10, 1, Some(r"C:\games\title\game.exe"), Some(CURRENT_SID));
        reused.identity.as_mut().unwrap().creation_time = 999;
        tracker.refresh_tracking(&BTreeMap::from([(10, reused)]));
        assert_eq!(tracker.tracked.keys().copied().collect::<Vec<_>>(), [10]);
    }

    #[test]
    fn tracked_processes_expand_only_to_new_verified_same_user_descendants() {
        let mut tracker = tracker();
        tracker.refresh_tracking(&BTreeMap::from([
            (
                20,
                process(20, 1, Some(r"C:\games\title\game.exe"), Some(CURRENT_SID)),
            ),
            (
                21,
                process(21, 20, Some(r"C:\windows\renderer.exe"), Some(CURRENT_SID)),
            ),
            (
                22,
                process(22, 20, Some(r"C:\windows\foreign.exe"), Some(OTHER_SID)),
            ),
            (23, process(23, 20, None, Some(CURRENT_SID))),
            (
                24,
                process(24, 99, Some(r"C:\windows\unrelated.exe"), Some(CURRENT_SID)),
            ),
        ]));
        assert_eq!(
            tracker.tracked.keys().copied().collect::<Vec<_>>(),
            [20, 21]
        );
    }

    #[test]
    fn steam_client_and_overlay_images_are_never_selected() {
        let mut tracker = tracker();
        tracker.refresh_tracking(&BTreeMap::from([
            (
                20,
                process(20, 1, Some(r"C:\games\title\steam.exe"), Some(CURRENT_SID)),
            ),
            (
                21,
                process(
                    21,
                    1,
                    Some(r"C:\games\title\GameOverlayUI.exe"),
                    Some(CURRENT_SID),
                ),
            ),
        ]));
        assert!(tracker.tracked.is_empty());
    }

    #[test]
    fn stop_revalidation_rejects_pid_reuse_image_change_and_user_change() {
        let mut tracker = tracker();
        let root = process(20, 1, Some(r"C:\games\title\game.exe"), Some(CURRENT_SID));
        tracker.refresh_tracking(&BTreeMap::from([(20, root.clone())]));
        let tracked = tracker.tracked.get(&20).unwrap().clone();

        let mut reused = root.clone();
        reused.identity.as_mut().unwrap().creation_time += 1;
        assert!(!process_matches_tracked(&tracked, &reused, CURRENT_SID));

        let changed_image = process(
            20,
            1,
            Some(r"C:\games\title\different.exe"),
            Some(CURRENT_SID),
        );
        assert!(!process_matches_tracked(
            &tracked,
            &changed_image,
            CURRENT_SID
        ));

        let wrong_user = process(20, 1, Some(r"C:\games\title\game.exe"), Some(OTHER_SID));
        assert!(!process_matches_tracked(&tracked, &wrong_user, CURRENT_SID));
    }

    #[test]
    fn install_path_comparison_is_case_insensitive_and_component_bounded() {
        assert!(path_is_within(
            Path::new(r"C:\GAMES\TITLE\Game.exe"),
            Path::new(r"c:\games\title")
        ));
        assert!(!path_is_within(
            Path::new(r"C:\games\title-copy\Game.exe"),
            Path::new(r"C:\games\title")
        ));
        assert!(!path_is_within(
            Path::new(r"C:\games\title"),
            Path::new(r"C:\games\title")
        ));
        assert!(path_is_within(
            Path::new(r"\\SERVER\SHARE\GAMES\TITLE\Game.exe"),
            Path::new(r"\\server\share\games\title")
        ));
    }

    #[test]
    fn missing_and_root_install_directories_disable_tracking() {
        assert!(canonical_install_directory(r"C:\path\that\does\not\exist").is_none());
        assert!(canonical_install_directory(r"C:\").is_none());
    }

    #[test]
    fn native_snapshot_contains_current_process_creation_image_and_user() {
        let processes = scan_processes().expect("Windows process snapshot");
        let current = processes
            .get(&unsafe { windows_sys::Win32::System::Threading::GetCurrentProcessId() })
            .expect("current process in Toolhelp snapshot");
        assert!(current.identity.is_some());
        assert!(current.image.is_some());
        assert_eq!(
            current.user_sid.as_deref(),
            current_process_user_sid().as_deref()
        );
    }
}
