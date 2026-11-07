//! Stream-owned Proton hook for launches handed to an already-running Steam client.
use crate::protocol::LaunchSpec;
#[cfg(target_os = "linux")]
use std::collections::BTreeMap;
#[cfg(all(target_os = "linux", not(test)))]
use std::collections::BTreeSet;
#[cfg(target_os = "linux")]
use std::io::{BufRead, BufReader};
#[cfg(target_os = "linux")]
use std::path::{Path, PathBuf};
#[cfg(target_os = "linux")]
use std::process::{Child, ChildStdin};
#[cfg(target_os = "linux")]
use std::process::{Command, Stdio};
#[cfg(target_os = "linux")]
use std::sync::mpsc;
#[cfg(target_os = "linux")]
use std::time::Duration;

#[cfg(target_os = "linux")]
const HOOK_SERVER: &str = include_str!("proton_hook_server.py");
#[cfg(target_os = "linux")]
const SERVER_READY_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Default)]
pub(crate) struct ProtonHookManager {
    #[cfg(target_os = "linux")]
    active: BTreeMap<String, ActiveHook>,
    #[cfg(target_os = "linux")]
    retiring: Vec<Child>,
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
}

#[cfg(target_os = "linux")]
struct HookServer {
    child: Child,
    // Closing the owner side of this pipe makes the helper exit after a
    // service crash, so the installed Proton hook cannot outlive its owner.
    _keepalive: ChildStdin,
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
            if let Some(active) = self.active.get_mut(app_id) {
                if active.enabled != spec.proton_dualsense_compatibility {
                    return Err("proton_hook_policy_conflict");
                }
                ensure_server(app_id, active)?;
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
            };
            ensure_server(app_id, &mut active)?;
            self.active.insert(app_id.to_owned(), active);
            Ok(Some(app_id.to_owned()))
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = spec;
            Ok(None)
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
            {
                if self.shutting_down {
                    if let Some(child) = retire_server(active.server.take()) {
                        self.retiring.push(child);
                    }
                } else {
                    // Preserve ordinary Stop's existing same-app restart ordering.
                    stop_server(active.server.take());
                }
            }
        }
    }

    /// Restart a helper that exited unexpectedly while an application owns it.
    pub(crate) fn reap(&mut self) {
        #[cfg(target_os = "linux")]
        for (app_id, active) in &mut self.active {
            if active
                .server
                .as_mut()
                .is_some_and(|server| server.child.try_wait().ok().flatten().is_some())
            {
                active.server = None;
            }
            if !self.shutting_down {
                let _ = ensure_server(app_id, active);
            }
        }
        #[cfg(target_os = "linux")]
        self.retiring
            .retain_mut(|child| !matches!(child.try_wait(), Ok(Some(_))));
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
            for (_, mut active) in std::mem::take(&mut self.active) {
                if let Some(child) = retire_server(active.server.take()) {
                    self.retiring.push(child);
                }
            }
            for child in &mut self.retiring {
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
    if !active.enabled || active.server.is_some() {
        return Ok(());
    }
    active.server = Some(start_server(app_id, &active.roots)?);
    Ok(())
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
    let (sender, receiver) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut line = String::new();
        let result = BufReader::new(stdout).read_line(&mut line).map(|_| line);
        let _ = sender.send(result);
    });
    match receiver.recv_timeout(SERVER_READY_TIMEOUT) {
        Ok(Ok(line)) if line.starts_with("READY ") => Ok(HookServer {
            child,
            _keepalive: keepalive,
        }),
        _ => {
            drop(keepalive);
            terminate_server(&mut child);
            Err("proton_hook_unavailable")
        }
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
fn stop_server(server: Option<HookServer>) {
    if let Some(mut server) = server {
        drop(server._keepalive);
        terminate_server(&mut server.child);
    }
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
    fn announced_shutdown_does_not_restart_an_exited_owned_helper() {
        let app_id = format!("{}", 3_600_000_000u64 + std::process::id() as u64);
        let mut manager = ProtonHookManager::with_roots(Vec::new());
        manager.acquire(&launch_spec(&app_id, true)).unwrap();
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
