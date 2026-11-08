//! Stream-owned Proton hook for launches handed to an already-running Steam client.
use crate::protocol::LaunchSpec;
#[cfg(target_os = "linux")]
use std::collections::BTreeMap;
#[cfg(all(target_os = "linux", not(test)))]
use std::collections::BTreeSet;
#[cfg(target_os = "linux")]
use std::io::{self, Read};
#[cfg(target_os = "linux")]
use std::os::fd::AsRawFd;
#[cfg(target_os = "linux")]
use std::path::{Path, PathBuf};
#[cfg(target_os = "linux")]
use std::process::{Child, ChildStdin, ChildStdout};
#[cfg(target_os = "linux")]
use std::process::{Command, Stdio};
#[cfg(target_os = "linux")]
use std::time::{Duration, Instant};

#[cfg(target_os = "linux")]
const HOOK_SERVER: &str = include_str!("proton_hook_server.py");
pub(crate) const SERVER_READY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
#[cfg(target_os = "linux")]
const RESTART_RETRY_DELAY: Duration = Duration::from_secs(1);

#[derive(Default)]
pub(crate) struct ProtonHookManager {
    #[cfg(target_os = "linux")]
    active: BTreeMap<String, ActiveHook>,
    #[cfg(target_os = "linux")]
    retiring: Vec<(String, Child)>,
    #[cfg(target_os = "linux")]
    shutting_down: bool,
    #[cfg(all(test, target_os = "linux"))]
    root_override: Option<Vec<PathBuf>>,
}

#[cfg(target_os = "linux")]
struct ActiveHook {
    enabled: bool,
    references: usize,
    roots: Vec<PathBuf>,
    server: Option<HookServer>,
    startup_failed: bool,
    was_ready: bool,
    next_start_at: Instant,
}

#[cfg(target_os = "linux")]
struct HookServer {
    child: Child,
    // Closing the owner side of this pipe makes the helper exit after a
    // service crash, so the installed Proton hook cannot outlive its owner.
    _keepalive: ChildStdin,
    readiness: Option<Readiness>,
}

#[cfg(target_os = "linux")]
struct Readiness {
    stdout: ChildStdout,
    deadline: Instant,
    line: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HookReadiness {
    Pending,
    Ready,
    Failed,
}

impl ProtonHookManager {
    /// Acquire the per-Steam-app hook before a launch can reach the Steam client.
    pub(crate) fn acquire(&mut self, spec: &LaunchSpec) -> Result<Option<String>, &'static str> {
        #[cfg(target_os = "linux")]
        {
            if self.shutting_down {
                return Err("host_stopping");
            }
            let Some(app_id) = spec.steam_id.as_deref() else {
                return Ok(None);
            };
            let roots = self.roots(spec.steam_install_dir.as_deref());
            let retiring = self.retiring.iter().any(|(id, _)| id == app_id);
            if let Some(active) = self.active.get_mut(app_id) {
                if active.startup_failed {
                    return Err("proton_hook_unavailable");
                }
                if active.enabled != spec.proton_dualsense_compatibility {
                    return Err("proton_hook_policy_conflict");
                }
                if !retiring && Instant::now() >= active.next_start_at {
                    start_for_acquire(app_id, active, ensure_server)?;
                }
                active.references = active
                    .references
                    .checked_add(1)
                    .ok_or("proton_hook_limit")?;
                return Ok(Some(app_id.to_owned()));
            }
            let mut active = ActiveHook {
                enabled: spec.proton_dualsense_compatibility,
                references: 1,
                roots,
                server: None,
                startup_failed: false,
                was_ready: false,
                next_start_at: Instant::now(),
            };
            if !retiring {
                ensure_server(app_id, &mut active)?;
            }
            self.active.insert(app_id.to_owned(), active);
            Ok(Some(app_id.to_owned()))
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = spec;
            Ok(None)
        }
    }

    pub(crate) fn readiness(&self, app_id: &str) -> HookReadiness {
        #[cfg(target_os = "linux")]
        {
            let Some(active) = self.active.get(app_id) else {
                return HookReadiness::Failed;
            };
            if active.startup_failed {
                HookReadiness::Failed
            } else if self.retiring.iter().any(|(id, _)| id == app_id) {
                HookReadiness::Pending
            } else if !active.enabled
                || active
                    .server
                    .as_ref()
                    .is_some_and(|server| server.readiness.is_none())
            {
                HookReadiness::Ready
            } else {
                HookReadiness::Pending
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = app_id;
            HookReadiness::Ready
        }
    }

    pub(crate) fn release(&mut self, _app_id: &str) {
        #[cfg(target_os = "linux")]
        {
            let Some(active) = self.active.get_mut(_app_id) else {
                return;
            };
            active.references = active.references.saturating_sub(1);
            if active.references == 0
                && let Some(mut active) = self.active.remove(_app_id)
                && let Some(child) = retire_server(active.server.take())
            {
                self.retiring.push((_app_id.to_owned(), child));
            }
        }
    }

    /// Restart a helper that exited unexpectedly while an application owns it.
    pub(crate) fn reap(&mut self) {
        #[cfg(target_os = "linux")]
        self.reap_with_starter(ensure_server);
    }

    #[cfg(target_os = "linux")]
    fn reap_with_starter(
        &mut self,
        mut start: impl FnMut(&str, &mut ActiveHook) -> Result<(), &'static str>,
    ) {
        self.retiring.retain_mut(|(_, child)| !retire_child(child));
        for (app_id, active) in &mut self.active {
            if let Some(server) = &mut active.server
                && let Some(readiness) = &mut server.readiness
            {
                match readiness.poll() {
                    Ok(Some(true)) => {
                        server.readiness = None;
                        active.was_ready = true;
                    }
                    Ok(None) => {}
                    Ok(Some(false)) | Err(_) => {
                        active.startup_failed = !active.was_ready;
                        active.next_start_at = Instant::now() + RESTART_RETRY_DELAY;
                        if let Some(child) = retire_server(active.server.take()) {
                            self.retiring.push((app_id.clone(), child));
                        }
                    }
                }
            }
            if active
                .server
                .as_mut()
                .is_some_and(|server| server.child.try_wait().ok().flatten().is_some())
            {
                if active
                    .server
                    .as_ref()
                    .is_some_and(|server| server.readiness.is_some())
                {
                    active.startup_failed = !active.was_ready;
                }
                active.next_start_at = Instant::now() + RESTART_RETRY_DELAY;
                active.server = None;
            }
            if !self.shutting_down
                && !active.startup_failed
                && !self.retiring.iter().any(|(id, _)| id == app_id)
                && Instant::now() >= active.next_start_at
                && start(app_id, active).is_err()
            {
                active.startup_failed = !active.was_ready;
                active.next_start_at = Instant::now() + RESTART_RETRY_DELAY;
            }
        }
    }

    /// Close helper restart admission without stopping the current consumer's hook.
    pub(crate) fn prepare_shutdown(&mut self) {
        #[cfg(target_os = "linux")]
        {
            self.shutting_down = true;
        }
    }

    pub(crate) fn shutdown_complete(&self) -> bool {
        #[cfg(target_os = "linux")]
        {
            self.active.is_empty() && self.retiring.is_empty()
        }
        #[cfg(not(target_os = "linux"))]
        {
            true
        }
    }

    pub(crate) fn force_shutdown(&mut self) {
        #[cfg(target_os = "linux")]
        {
            for (app_id, mut active) in std::mem::take(&mut self.active) {
                if let Some(child) = retire_server(active.server.take()) {
                    self.retiring.push((app_id, child));
                }
            }
            for (_, child) in &mut self.retiring {
                let _ = retire_child(child);
            }
        }
    }

    #[cfg(all(test, target_os = "linux"))]
    fn with_roots(roots: Vec<PathBuf>) -> Self {
        Self {
            active: BTreeMap::new(),
            retiring: Vec::new(),
            shutting_down: false,
            root_override: Some(roots),
        }
    }

    #[cfg(target_os = "linux")]
    fn roots(&self, steam_install_directory: Option<&str>) -> Vec<PathBuf> {
        #[cfg(test)]
        {
            // Ordinary unit tests never inspect or mutate live Steam profiles.
            let _ = steam_install_directory;
            self.root_override.clone().unwrap_or_default()
        }
        #[cfg(not(test))]
        {
            steam_roots(steam_install_directory)
        }
    }
}

impl Drop for ProtonHookManager {
    fn drop(&mut self) {
        self.force_shutdown();
    }
}

#[cfg(target_os = "linux")]
fn ensure_server(app_id: &str, active: &mut ActiveHook) -> Result<(), &'static str> {
    if active.startup_failed {
        return Err("proton_hook_unavailable");
    }
    if !active.enabled || active.server.is_some() {
        return Ok(());
    }
    active.server = Some(start_server(app_id, &active.roots)?);
    Ok(())
}

#[cfg(target_os = "linux")]
fn start_for_acquire(
    app_id: &str,
    active: &mut ActiveHook,
    mut start: impl FnMut(&str, &mut ActiveHook) -> Result<(), &'static str>,
) -> Result<(), &'static str> {
    match start(app_id, active) {
        Ok(()) => Ok(()),
        Err(error) => {
            active.next_start_at = Instant::now() + RESTART_RETRY_DELAY;
            Err(error)
        }
    }
}

#[cfg(target_os = "linux")]
fn start_server(app_id: &str, roots: &[PathBuf]) -> Result<HookServer, &'static str> {
    let mut command = Command::new("python3");
    command
        .arg("-u")
        .arg("-c")
        .arg(HOOK_SERVER)
        .arg(app_id)
        .arg("1")
        .args(roots)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = command.spawn().map_err(|_| "proton_hook_unavailable")?;
    let Some(keepalive) = child.stdin.take() else {
        terminate_server(&mut child);
        return Err("proton_hook_unavailable");
    };
    let Some(stdout) = child.stdout.take() else {
        drop(keepalive);
        terminate_server(&mut child);
        return Err("proton_hook_unavailable");
    };
    Ok(HookServer {
        child,
        _keepalive: keepalive,
        readiness: Some(Readiness {
            stdout,
            deadline: Instant::now() + SERVER_READY_TIMEOUT,
            line: Vec::new(),
        }),
    })
}

#[cfg(target_os = "linux")]
impl Readiness {
    fn poll(&mut self) -> io::Result<Option<bool>> {
        poll_ready_line(&mut self.stdout, &mut self.line, self.deadline)
    }
}

/// Read at most one available chunk per host tick. An inherited open pipe cannot
/// block shutdown or leave a reader thread executing unloaded module code.
#[cfg(target_os = "linux")]
fn poll_ready_line(
    stdout: &mut (impl Read + AsRawFd),
    line: &mut Vec<u8>,
    deadline: Instant,
) -> io::Result<Option<bool>> {
    const MAX_READY_LINE: usize = 4096;
    if Instant::now() >= deadline {
        return Ok(Some(false));
    }
    let mut descriptor = libc::pollfd {
        fd: stdout.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: descriptor is valid writable storage for one pollfd, and the
    // borrowed stream keeps the file descriptor open throughout this call.
    let ready = unsafe { libc::poll(&mut descriptor, 1, 0) };
    if ready < 0 {
        let error = io::Error::last_os_error();
        return if error.kind() == io::ErrorKind::Interrupted {
            Ok(None)
        } else {
            Err(error)
        };
    }
    if ready == 0 {
        return Ok(None);
    }
    let mut bytes = [0; 256];
    match stdout.read(&mut bytes) {
        Ok(0) => Ok(Some(false)),
        Ok(count) => {
            line.extend_from_slice(&bytes[..count]);
            if let Some(end) = line.iter().position(|byte| *byte == b'\n') {
                return Ok(Some(end < MAX_READY_LINE && line.starts_with(b"READY ")));
            }
            Ok((line.len() >= MAX_READY_LINE).then_some(false))
        }
        Err(error) if error.kind() == io::ErrorKind::Interrupted => Ok(None),
        Err(error) => Err(error),
    }
}

#[cfg(target_os = "linux")]
fn terminate_server(server: &mut Child) {
    if server.try_wait().ok().flatten().is_none() {
        let _ = server.kill();
    }
    let _ = server.wait();
}

#[cfg(target_os = "linux")]
fn retire_server(server: Option<HookServer>) -> Option<Child> {
    let mut server = server?;
    drop(server._keepalive);
    if retire_child(&mut server.child) {
        None
    } else {
        Some(server.child)
    }
}

#[cfg(target_os = "linux")]
fn retire_child(child: &mut Child) -> bool {
    retire_child_with(
        child,
        |child| child.try_wait().map(|status| status.is_some()),
        Child::kill,
    )
}

/// Only an observed live, still-owned child authorizes a final signal. A failed
/// wait retains unresolved ownership and cannot authorize numeric-PID signaling.
#[cfg(target_os = "linux")]
fn retire_child_with<T>(
    child: &mut T,
    mut observe: impl FnMut(&mut T) -> std::io::Result<bool>,
    mut signal: impl FnMut(&mut T) -> std::io::Result<()>,
) -> bool {
    match observe(child) {
        Ok(true) => true,
        Err(_) => false,
        Ok(false) => {
            let _ = signal(child);
            matches!(observe(child), Ok(true))
        }
    }
}

#[cfg(all(target_os = "linux", not(test)))]
fn steam_roots(steam_install_directory: Option<&str>) -> Vec<PathBuf> {
    let mut roots = BTreeSet::new();
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        roots.insert(home.join(".steam/root"));
        roots.insert(home.join(".steam/steam"));
        roots.insert(home.join(".local/share/Steam"));
    }
    if let Some(data_home) = std::env::var_os("XDG_DATA_HOME") {
        roots.insert(PathBuf::from(data_home).join("Steam"));
    }
    for name in ["STEAM_COMPAT_CLIENT_INSTALL_PATH", "STEAM_PATH"] {
        if let Some(path) = std::env::var_os(name) {
            roots.insert(PathBuf::from(path));
        }
    }
    if let Some(install_directory) = steam_install_directory {
        let install_directory = Path::new(install_directory);
        for ancestor in install_directory.ancestors() {
            if ancestor.file_name().is_some_and(|name| name == "steamapps")
                && let Some(library) = ancestor.parent()
            {
                roots.insert(library.to_owned());
                break;
            }
        }
    }
    roots.into_iter().collect()
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::os::fd::FromRawFd;

    fn test_pipe() -> (std::fs::File, std::fs::File) {
        let mut descriptors = [-1; 2];
        // SAFETY: the array has room for both descriptors and successful pipe2
        // transfers unique ownership of them to the two File values below.
        assert_eq!(
            unsafe { libc::pipe2(descriptors.as_mut_ptr(), libc::O_CLOEXEC) },
            0
        );
        unsafe {
            (
                std::fs::File::from_raw_fd(descriptors[0]),
                std::fs::File::from_raw_fd(descriptors[1]),
            )
        }
    }

    #[test]
    fn helper_readiness_is_bounded_when_a_pipe_stays_open() {
        let (mut reader, _held_writer) = test_pipe();
        let start = Instant::now();
        let mut line = Vec::new();
        assert_eq!(
            poll_ready_line(&mut reader, &mut line, start + Duration::from_secs(5)).unwrap(),
            None
        );
        assert!(start.elapsed() < Duration::from_millis(100));
        assert_eq!(
            poll_ready_line(&mut reader, &mut line, Instant::now()).unwrap(),
            Some(false)
        );
    }

    #[test]
    fn helper_readiness_requires_a_complete_bounded_line() {
        use std::io::Write;

        for (bytes, expected) in [
            (b"READY 1 2 0\n".to_vec(), true),
            (b"READY incomplete".to_vec(), false),
            (
                [b"READY ".as_slice(), &vec![b'x'; 4096], b"\n"].concat(),
                false,
            ),
        ] {
            let (mut reader, mut writer) = test_pipe();
            let writer = std::thread::spawn(move || writer.write_all(&bytes));
            let deadline = Instant::now() + Duration::from_secs(2);
            let mut line = Vec::new();
            let ready = loop {
                if let Some(ready) = poll_ready_line(&mut reader, &mut line, deadline).unwrap() {
                    break ready;
                }
                assert!(Instant::now() < deadline);
                std::thread::yield_now();
            };
            // The oversized case may stop reading before its writer finishes.
            drop(reader);
            let _ = writer.join().unwrap();
            assert_eq!(ready, expected);
        }
    }

    fn wait_hook_ready(manager: &mut ProtonHookManager, app_id: &str) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while manager.readiness(app_id) == HookReadiness::Pending {
            manager.reap();
            assert!(Instant::now() < deadline, "helper did not start");
            std::thread::yield_now();
        }
        assert_eq!(manager.readiness(app_id), HookReadiness::Ready);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_hook_installer_preserves_existing_settings_and_is_idempotent() {
        let root = fixture_root("install");
        let tool = root.join("steamapps/common/Proton Experimental");
        std::fs::create_dir_all(&tool).unwrap();
        std::fs::write(tool.join("proton"), b"fixture").unwrap();
        std::fs::write(
            tool.join("user_settings.sample.py"),
            b"user_settings = {}\n",
        )
        .unwrap();
        let existing = b"user_settings = {'CUSTOM': 'kept'}\n";
        let settings = tool.join("user_settings.py");
        std::fs::write(&settings, existing).unwrap();
        // Simulate two helpers reading the same original metadata before the
        // first helper atomically adds the hook.
        let race = Command::new("python3")
            .arg("-c")
            .arg(HOOK_SERVER)
            .arg("--test-racing-update")
            .arg(&settings)
            .output()
            .unwrap();
        assert!(
            race.status.success(),
            "{}",
            String::from_utf8_lossy(&race.stderr)
        );
        assert_eq!(race.stdout, b"ok\n");

        let first = install_only(&root);
        assert_eq!(first, "1 1 0");
        let installed = std::fs::read(&settings).unwrap();
        assert!(installed.starts_with(existing));
        assert_eq!(
            installed
                .windows(b"# BEGIN Amaterasu Proton compatibility v1".len())
                .filter(|window| *window == b"# BEGIN Amaterasu Proton compatibility v1")
                .count(),
            1
        );
        assert_eq!(install_only(&root), "1 1 0");
        assert_eq!(std::fs::read(settings).unwrap(), installed);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_hook_installer_refuses_symlink_and_hardlink_settings_files() {
        let root = fixture_root("unsafe");
        let tool = root.join("compatibilitytools.d/Proton GE");
        std::fs::create_dir_all(&tool).unwrap();
        std::fs::write(tool.join("proton"), b"fixture").unwrap();
        std::fs::write(
            tool.join("user_settings.sample.py"),
            b"user_settings = {}\n",
        )
        .unwrap();
        let external = root.join("external.py");
        std::fs::write(&external, b"custom = True\n").unwrap();
        std::os::unix::fs::symlink(&external, tool.join("user_settings.py")).unwrap();
        assert_eq!(install_only(&root), "1 0 1");
        assert_eq!(std::fs::read(&external).unwrap(), b"custom = True\n");
        std::fs::remove_file(tool.join("user_settings.py")).unwrap();
        std::fs::hard_link(&external, tool.join("user_settings.py")).unwrap();
        assert_eq!(install_only(&root), "1 0 1");
        assert_eq!(std::fs::read(&external).unwrap(), b"custom = True\n");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_proton_user_settings_hook_applies_defaults_and_becomes_inert_after_stop() {
        let root = fixture_root("session");
        let tool = root.join("steamapps/common/Proton 9");
        std::fs::create_dir_all(&tool).unwrap();
        std::fs::write(tool.join("proton"), b"fixture").unwrap();
        std::fs::write(
            tool.join("user_settings.sample.py"),
            b"user_settings = {}\n",
        )
        .unwrap();
        std::fs::write(tool.join("user_settings.py"), b"user_settings = {}\n").unwrap();

        let app_id = format!("{}", 2_000_000_000 + (std::process::id() % 1_000_000_000));
        let mut manager = ProtonHookManager::with_roots(vec![root.clone()]);
        let enabled_spec = launch_spec(&app_id, true);
        assert_eq!(
            manager.acquire(&enabled_spec).unwrap().as_deref(),
            Some(app_id.as_str())
        );
        wait_hook_ready(&mut manager, &app_id);
        assert_eq!(
            manager.acquire(&enabled_spec).unwrap().as_deref(),
            Some(app_id.as_str())
        );
        assert!(manager.acquire(&launch_spec(&app_id, false)).is_err());
        let settings_file = tool.join("user_settings.py");
        let installed = std::fs::read_to_string(&settings_file).unwrap();
        let hook_start = installed
            .find("\n# BEGIN Amaterasu Proton compatibility v1")
            .unwrap();
        let hook_block = installed[hook_start..].to_owned();
        let explicit = run_user_settings(
            &settings_file,
            &serde_json::json!({
                "SteamAppId": app_id,
                "PROTON_KEEP_SONY_AUDIO_ENDPOINT_VISIBLE": ""
            }),
        );
        assert_eq!(
            explicit["settings"]["PROTON_SONY_WINDOWS_DEVICE_NAMES"],
            "1"
        );
        assert_eq!(
            explicit["env"]["PROTON_KEEP_SONY_AUDIO_ENDPOINT_VISIBLE"],
            ""
        );
        assert_eq!(explicit["env"]["PROTON_SONY_WINDOWS_DEVICE_NAMES"], "1");

        let mut configured_source =
            b"user_settings = {'PROTON_SONY_WINDOWS_DEVICE_NAMES': ''}\n".to_vec();
        configured_source.extend_from_slice(hook_block.as_bytes());
        std::fs::write(&settings_file, configured_source).unwrap();
        let configured =
            run_user_settings(&settings_file, &serde_json::json!({"SteamAppId": app_id}));
        assert_eq!(
            configured["settings"]["PROTON_SONY_WINDOWS_DEVICE_NAMES"],
            ""
        );
        assert_eq!(
            configured["settings"]["PROTON_KEEP_SONY_AUDIO_ENDPOINT_VISIBLE"],
            "1"
        );

        manager.release(&app_id);
        assert!(manager.active.contains_key(&app_id));
        manager.release(&app_id);
        assert!(!manager.active.contains_key(&app_id));
        let deadline = Instant::now() + Duration::from_secs(3);
        while !manager.retiring.is_empty() {
            manager.reap();
            assert!(Instant::now() < deadline, "helper did not retire");
            std::thread::yield_now();
        }
        let mut inert_source = b"user_settings = {}\n".to_vec();
        inert_source.extend_from_slice(hook_block.as_bytes());
        std::fs::write(&settings_file, inert_source).unwrap();
        let inert = run_user_settings(&settings_file, &serde_json::json!({"SteamAppId": app_id}));
        assert_eq!(inert["settings"], serde_json::json!({}));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_proton_hook_helper_exits_when_its_owner_pipe_closes() {
        let app_id = format!("{}", 3_000_000_000u64 + (std::process::id() as u64));
        let mut server = start_server(&app_id, &[]).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while server.readiness.is_some() {
            if server.readiness.as_mut().unwrap().poll().unwrap() == Some(true) {
                server.readiness = None;
            }
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        drop(server._keepalive);
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            if server.child.try_wait().unwrap().is_some() {
                break;
            }
            if std::time::Instant::now() >= deadline {
                terminate_server(&mut server.child);
                panic!("Proton hook helper stayed alive after its owner pipe closed");
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    struct HelperProbe {
        observations: std::collections::VecDeque<std::io::Result<bool>>,
        signals: usize,
    }

    fn retire_probe(probe: &mut HelperProbe) -> bool {
        retire_child_with(
            probe,
            |probe| {
                probe
                    .observations
                    .pop_front()
                    .expect("unexpected extra wait")
            },
            |probe| {
                probe.signals += 1;
                Ok(())
            },
        )
    }

    #[test]
    fn failed_helper_waits_retain_ownership_without_signaling() {
        for error in [
            std::io::Error::from_raw_os_error(libc::ECHILD),
            std::io::Error::other("unreadable child status"),
        ] {
            let mut probe = HelperProbe {
                observations: std::collections::VecDeque::from([Err(error)]),
                signals: 0,
            };
            assert!(!retire_probe(&mut probe));
            assert_eq!(probe.signals, 0);
        }
    }

    #[test]
    fn helper_retirement_requires_observed_exit_after_the_signal() {
        let mut probe = HelperProbe {
            observations: std::collections::VecDeque::from([Ok(false), Ok(false)]),
            signals: 0,
        };
        assert!(!retire_probe(&mut probe));
        assert_eq!(probe.signals, 1);
        probe.observations.push_back(Ok(true));
        assert!(retire_probe(&mut probe));
        assert_eq!(probe.signals, 1);
    }

    #[test]
    fn shutdown_retires_helper_handles_without_restart_or_blocking_wait() {
        let app_id = format!("{}", 3_500_000_000u64 + std::process::id() as u64);
        let mut manager = ProtonHookManager::with_roots(Vec::new());
        manager.acquire(&launch_spec(&app_id, true)).unwrap();
        wait_hook_ready(&mut manager, &app_id);
        let pid = manager
            .active
            .get(&app_id)
            .unwrap()
            .server
            .as_ref()
            .unwrap()
            .child
            .id();
        manager.prepare_shutdown();
        assert_eq!(
            manager
                .active
                .get(&app_id)
                .unwrap()
                .server
                .as_ref()
                .unwrap()
                .child
                .id(),
            pid
        );
        assert!(!manager.shutdown_complete());
        manager.release(&app_id);
        assert!(manager.active.is_empty());
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while !manager.shutdown_complete() {
            manager.reap();
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert!(manager.acquire(&launch_spec(&app_id, true)).is_err());
    }

    #[test]
    fn replacement_waits_for_previous_helper_to_retire() {
        let app_id = format!("{}", 3_700_000_000u64 + std::process::id() as u64);
        let mut manager = ProtonHookManager::with_roots(Vec::new());
        let previous = Command::new("/bin/sleep").arg("5").spawn().unwrap();
        manager.retiring.push((app_id.clone(), previous));
        manager.acquire(&launch_spec(&app_id, true)).unwrap();
        assert_eq!(manager.readiness(&app_id), HookReadiness::Pending);
        assert!(manager.active[&app_id].server.is_none());
        let deadline = Instant::now() + Duration::from_secs(3);
        while !manager.retiring.is_empty() {
            manager.reap();
            assert!(Instant::now() < deadline, "previous helper did not retire");
            std::thread::yield_now();
        }
        wait_hook_ready(&mut manager, &app_id);
        manager.release(&app_id);
    }

    #[test]
    fn disabled_replacement_waits_for_an_enabled_helper_to_retire() {
        let app_id = format!("{}", 3_800_000_000u64 + std::process::id() as u64);
        let mut manager = ProtonHookManager::with_roots(Vec::new());
        let previous = Command::new("/bin/sleep").arg("5").spawn().unwrap();
        manager.retiring.push((app_id.clone(), previous));
        manager.acquire(&launch_spec(&app_id, false)).unwrap();
        assert_eq!(manager.readiness(&app_id), HookReadiness::Pending);
        let deadline = Instant::now() + Duration::from_secs(3);
        while !manager.retiring.is_empty() {
            manager.reap();
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert_eq!(manager.readiness(&app_id), HookReadiness::Ready);
        manager.release(&app_id);
    }

    #[test]
    fn delayed_helper_spawn_failure_is_terminal_for_waiting_launches() {
        let app_id = "123456";
        let mut manager = ProtonHookManager::with_roots(Vec::new());
        manager.acquire(&launch_spec(app_id, false)).unwrap();
        manager.active.get_mut(app_id).unwrap().enabled = true;
        manager.reap_with_starter(|_, _| Err("injected spawn failure"));
        assert_eq!(manager.readiness(app_id), HookReadiness::Failed);
        manager.release(app_id);
    }

    #[test]
    fn already_running_hook_can_retry_after_helper_restart_failure() {
        let app_id = "123457";
        let mut manager = ProtonHookManager::with_roots(Vec::new());
        manager.acquire(&launch_spec(app_id, false)).unwrap();
        let active = manager.active.get_mut(app_id).unwrap();
        active.enabled = true;
        active.was_ready = true;
        let mut attempts = 0;
        for _ in 0..2 {
            manager.reap_with_starter(|_, _| {
                attempts += 1;
                Err("injected restart failure")
            });
        }
        assert_eq!(attempts, 1, "failed restarts must be rate limited");
        manager.active.get_mut(app_id).unwrap().next_start_at = Instant::now();
        manager.reap_with_starter(|_, _| {
            attempts += 1;
            Err("injected restart failure")
        });
        assert_eq!(attempts, 2);
        assert_eq!(manager.readiness(app_id), HookReadiness::Pending);
        manager.release(app_id);
    }

    #[test]
    fn new_reference_cannot_bypass_helper_restart_backoff() {
        let app_id = "123458";
        let mut manager = ProtonHookManager::with_roots(Vec::new());
        manager.acquire(&launch_spec(app_id, false)).unwrap();
        let active = manager.active.get_mut(app_id).unwrap();
        active.enabled = true;
        active.was_ready = true;
        active.next_start_at = Instant::now() + Duration::from_secs(60);
        manager.acquire(&launch_spec(app_id, true)).unwrap();
        assert!(manager.active[app_id].server.is_none());
        assert_eq!(manager.readiness(app_id), HookReadiness::Pending);
        manager.release(app_id);
        manager.release(app_id);
    }

    #[test]
    fn failed_acquire_start_applies_restart_backoff() {
        let app_id = "123459";
        let mut manager = ProtonHookManager::with_roots(Vec::new());
        manager.acquire(&launch_spec(app_id, false)).unwrap();
        let active = manager.active.get_mut(app_id).unwrap();
        active.enabled = true;
        active.was_ready = true;
        assert!(start_for_acquire(app_id, active, |_, _| Err("spawn failed")).is_err());
        manager.acquire(&launch_spec(app_id, true)).unwrap();
        assert!(manager.active[app_id].server.is_none());
        assert_eq!(manager.readiness(app_id), HookReadiness::Pending);
        manager.release(app_id);
        manager.release(app_id);
    }

    #[test]
    fn announced_shutdown_does_not_restart_an_exited_owned_helper() {
        let app_id = format!("{}", 3_600_000_000u64 + std::process::id() as u64);
        let mut manager = ProtonHookManager::with_roots(Vec::new());
        manager.acquire(&launch_spec(&app_id, true)).unwrap();
        wait_hook_ready(&mut manager, &app_id);
        manager.prepare_shutdown();
        let server = manager
            .active
            .get_mut(&app_id)
            .unwrap()
            .server
            .as_mut()
            .unwrap();
        server.child.kill().unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while !matches!(server.child.try_wait(), Ok(Some(_))) {
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        }
        manager.reap();
        assert!(manager.active.get(&app_id).unwrap().server.is_none());
        manager.release(&app_id);
        assert!(manager.shutdown_complete());
    }

    #[cfg(target_os = "linux")]
    fn install_only(root: &Path) -> String {
        let output = Command::new("python3")
            .arg("-c")
            .arg(HOOK_SERVER)
            .arg("--install-only")
            .arg(root)
            .output()
            .expect("Python 3 is available for Proton");
        assert!(output.status.success());
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    #[cfg(target_os = "linux")]
    fn run_user_settings(
        settings_file: &Path,
        environment: &serde_json::Value,
    ) -> serde_json::Value {
        let script = r#"
import json, runpy, sys, types
session = types.SimpleNamespace(env=json.loads(sys.argv[2]))
sys.modules["__main__"].g_session = session
namespace = runpy.run_path(sys.argv[1])
print(json.dumps({"settings": namespace.get("user_settings", {}), "env": session.env}))
"#;
        let output = Command::new("python3")
            .arg("-c")
            .arg(script)
            .arg(settings_file)
            .arg(serde_json::to_string(environment).unwrap())
            .output()
            .expect("Python 3 is available for Proton");
        assert!(
            output.status.success(),
            "user settings fixture failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }

    #[cfg(target_os = "linux")]
    fn fixture_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "amaterasu-proton-hook-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[cfg(target_os = "linux")]
    fn launch_spec(app_id: &str, enabled: bool) -> LaunchSpec {
        LaunchSpec {
            output: None,
            executable: "/usr/bin/true".into(),
            args: Vec::new(),
            environment: BTreeMap::new(),
            cwd: None,
            preparation_cwd: None,
            detached_cwd: crate::protocol::DetachedWorkingDirectory::default(),
            elevated: false,
            auto_detach: true,
            wait_all: true,
            exit_timeout_seconds: 10,
            steam_id: Some(app_id.into()),
            proton_dualsense_compatibility: enabled,
            steam_install_dir: None,
            detached: Vec::new(),
            preparation: Vec::new(),
        }
    }
}
