//! Development console supervision. Never replays a dispatched command.
use amaterasu_contracts::{Command as ControlCommand, HostLifecycleAction, Response};
use amaterasu_package::{
    ActivationState, BootstrapConfig, PackageResult, RootLock, VerificationPolicy,
    package_directory, read_json, unix_timestamp, verify_package, write_json_atomically,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, TryRecvError};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

const LIVENESS_TIMEOUT: Duration = Duration::from_secs(5);
// The worker may spend up to 20 seconds quiescing components before teardown.
const WORKER_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(25);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(35);
const SERVICE_READY_FILE: &str = "service-ready.json";
const OPERATIONAL_TIMEOUT: Duration = Duration::from_secs(10);
const HTTP_RESPONSE_LIMIT: usize = 24_000;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ServiceReadiness {
    schema: u32,
    process_id: u32,
    digest: String,
}

/// Queries a marker published only after the active SCM-owned service has
/// selected and started its worker. Unlike `probe`, this never acquires the
/// root lock or starts a disposable worker of its own.
pub fn probe_service_readiness(root: &Path) -> PackageResult<()> {
    let readiness: ServiceReadiness = read_json(&root.join(SERVICE_READY_FILE))?;
    if readiness.schema != 1 || readiness.process_id == 0 || readiness.digest.len() != 64 {
        return Err("service readiness marker is invalid".into());
    }
    if !service_process_is_current_instance(readiness.process_id) {
        return Err("service readiness marker does not belong to the current SCM instance".into());
    }
    let selection: serde_json::Value = read_json(&root.join("selection.json"))?;
    if selection.get("active").and_then(serde_json::Value::as_str) != Some(&readiness.digest) {
        return Err("active service readiness does not match selection".into());
    }
    Ok(())
}

/// Establishes that the active SCM service owns a live interactive worker and
/// that worker is consuming the selected signed production package's actual
/// management, authentication, catalog, pairing, and NVHTTP state. This is a
/// bounded loopback probe; it never starts a competing worker or obtains an
/// administrator session.
pub fn probe_operational_readiness(root: &Path) -> PackageResult<()> {
    probe_service_readiness(root)?;
    let configuration = selected_runtime_config(root)?;
    let management = loopback_address(&configuration, "amaterasu.management", "bind")?;
    let nvhttp = loopback_address(&configuration, "amaterasu.nvhttp", "http_bind")?;
    let deadline = Instant::now() + OPERATIONAL_TIMEOUT;

    let health = loopback_get(management, "/healthz", deadline)?;
    let health = json_response(&health)?;
    if health.get("ok").and_then(serde_json::Value::as_bool) != Some(true)
        || health
            .get("authentication_verifier")
            .and_then(serde_json::Value::as_bool)
            != Some(true)
        || health
            .get("catalog_loaded")
            .and_then(serde_json::Value::as_bool)
            != Some(true)
        || health
            .get("pairing_identity")
            .and_then(serde_json::Value::as_bool)
            != Some(true)
    {
        return Err("management operational health is incomplete".into());
    }

    let server_info = loopback_get(nvhttp, "/serverinfo", deadline)?;
    if !server_info.starts_with("HTTP/1.1 200 ")
        || !server_info.contains("status_code=\"200\"")
        || !server_info.contains("<uniqueid>")
    {
        return Err("NVHTTP server information is not operational".into());
    }
    Ok(())
}

fn selected_runtime_config(root: &Path) -> PackageResult<amaterasu_contracts::RuntimeConfig> {
    let bootstrap: BootstrapConfig = read_json(&root.join("bootstrap.json"))?;
    if bootstrap.development || bootstrap.allow_unsigned {
        return Err("operational readiness requires a signed production bootstrap".into());
    }
    let selection: ActivationState = read_json(&root.join("selection.json"))?;
    let active = selection
        .active
        .ok_or("operational readiness has no active package")?;
    let package = verify_package(
        &package_directory(root, &active)?,
        &bootstrap,
        VerificationPolicy {
            minimum_sequence: selection.highest_sequence,
            allow_historical: false,
            now: unix_timestamp(),
        },
    )?;
    if !package.signed || package.digest != active || package.manifest.host_context_schema != 1 {
        return Err("operational readiness selected package is not authenticated".into());
    }
    Ok(package.runtime_config)
}

fn loopback_address(
    configuration: &amaterasu_contracts::RuntimeConfig,
    module: &str,
    field: &str,
) -> PackageResult<SocketAddr> {
    let value = configuration
        .settings
        .get(module)
        .and_then(|settings| settings.get(field))
        .and_then(serde_json::Value::as_str)
        .ok_or("selected package lacks a required loopback listener")?;
    let address: SocketAddr = value
        .parse()
        .map_err(|_| "selected listener address is invalid")?;
    if !address.ip().is_loopback() || address.port() == 0 {
        return Err("operational readiness refuses a non-loopback listener".into());
    }
    Ok(address)
}

fn loopback_get(address: SocketAddr, path: &str, deadline: Instant) -> PackageResult<String> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err("operational readiness deadline elapsed".into());
    }
    let mut stream = TcpStream::connect_timeout(&address, remaining.min(Duration::from_secs(2)))
        .map_err(|_| "operational listener is unavailable")?;
    stream
        .set_read_timeout(Some(remaining.min(Duration::from_secs(2))))
        .map_err(|error| error.to_string())?;
    stream
        .set_write_timeout(Some(remaining.min(Duration::from_secs(2))))
        .map_err(|error| error.to_string())?;
    let host = address.to_string();
    stream
        .write_all(
            format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n").as_bytes(),
        )
        .map_err(|_| "operational listener request failed")?;
    let mut response = Vec::new();
    stream
        .take((HTTP_RESPONSE_LIMIT + 1) as u64)
        .read_to_end(&mut response)
        .map_err(|_| "operational listener response timed out")?;
    if response.len() > HTTP_RESPONSE_LIMIT {
        return Err("operational listener response exceeds its limit".into());
    }
    String::from_utf8(response).map_err(|_| "operational listener response is not UTF-8".into())
}

fn json_response(response: &str) -> PackageResult<serde_json::Value> {
    let (head, body) = response
        .split_once("\r\n\r\n")
        .ok_or("operational health response is malformed")?;
    if !head.starts_with("HTTP/1.1 200 ") {
        return Err("management operational health is unavailable".into());
    }
    serde_json::from_str(body).map_err(|_| "management operational health is malformed".into())
}

#[cfg(windows)]
fn service_process_is_current_instance(process_id: u32) -> bool {
    const SYNCHRONIZE: u32 = 0x0010_0000;
    const STILL_ACTIVE: u32 = 259;
    unsafe extern "system" {
        fn OpenProcess(access: u32, inherit: i32, process_id: u32) -> *mut std::ffi::c_void;
        fn GetExitCodeProcess(process: *mut std::ffi::c_void, code: *mut u32) -> i32;
        fn CloseHandle(handle: *mut std::ffi::c_void) -> i32;
    }
    if cfg!(test) && process_id == std::process::id() {
        return true;
    }
    let output = std::process::Command::new(r"C:\Windows\System32\sc.exe")
        .args(["queryex", "Amaterasu"])
        .output();
    let Ok(output) = output else {
        return false;
    };
    let status = String::from_utf8_lossy(&output.stdout);
    let marker = format!("PID : {process_id}");
    if !status.contains(&marker) {
        return false;
    }
    // SAFETY: the PID came from the private service marker. The handle is used
    // only for a synchronous exit-code query and is closed before return.
    unsafe {
        let handle = OpenProcess(SYNCHRONIZE, 0, process_id);
        if handle.is_null() {
            return false;
        }
        let mut code = 0;
        let queried = GetExitCodeProcess(handle, &mut code) != 0;
        let _ = CloseHandle(handle);
        queried && code == STILL_ACTIVE
    }
}

#[cfg(not(windows))]
fn service_process_is_current_instance(process_id: u32) -> bool {
    // Non-Windows service mode is a test harness. Production SCM readiness is
    // checked by the Windows process handle query above.
    process_id == std::process::id()
}

pub fn serve(root: &Path) -> PackageResult<()> {
    let _lock = RootLock::acquire(root)?;
    // Candidate health and destruction are established in a separate disposable
    // process before console input is read or sent to the persistent worker.
    let digest = crate::supervisor::select_active(root)?;
    let input = crate::protocol::lines(std::io::stdin());
    let mut restarts = crate::protocol::RestartBudget::default();
    let mut worker = start_with_recovery(root, &digest, &mut restarts, true)?;
    loop {
        match worker.check_idle() {
            Ok(Some(action)) => {
                let Some(replacement) =
                    apply_worker_action(action, worker, root, &digest, &mut restarts, true)?
                else {
                    return Ok(());
                };
                worker = replacement;
            }
            Ok(None) => {}
            Err(failure) => {
                eprintln!("Development worker failed: {failure}");
                drop(worker);
                worker = restart(root, &digest, &mut restarts, true)?;
            }
        }
        match input.recv_timeout(Duration::from_millis(20)) {
            Ok(Ok(line)) => {
                if serde_json::from_str::<serde_json::Value>(&line).ok()
                    == Some(json!({"host":"shutdown"}))
                {
                    worker.shutdown()?;
                    return write_reply(&json!({"kind":"ok"}));
                }
                // Validate before dispatch so malformed requests cannot restart workers.
                if let Err(error) = serde_json::from_str::<ControlCommand>(&line) {
                    write_reply(&Response::error("invalid_request", error.to_string()))?;
                    continue;
                }
                match worker.execute(&line) {
                    Ok(WorkerEvent::Response(reply)) => write_reply(&reply)?,
                    Ok(WorkerEvent::HostAction(action)) => {
                        write_reply(&Response::error(
                            "worker_unavailable",
                            "host lifecycle action interrupted the request; outcome may be unknown; command was not replayed",
                        ))?;
                        let Some(replacement) = apply_worker_action(
                            action,
                            worker,
                            root,
                            &digest,
                            &mut restarts,
                            true,
                        )?
                        else {
                            return Ok(());
                        };
                        worker = replacement;
                    }
                    Err(failure) => {
                        write_reply(&Response::error(
                            "worker_unavailable",
                            format!("{failure}; outcome may be unknown; command was not replayed"),
                        ))?;
                        drop(worker);
                        worker = restart(root, &digest, &mut restarts, true)?;
                    }
                }
            }
            Ok(Err(failure)) => {
                write_reply(&Response::error("invalid_request", &failure))?;
                worker.shutdown()?;
                return Err(failure);
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return worker.shutdown(),
        }
    }
}

/// Runs the development host without the console protocol.
///
/// Windows dispatches this through the Service Control Manager. Other targets
/// use stdin EOF only as a test harness stop signal; input is never interpreted
/// as a product command.
pub fn service(root: &Path) -> PackageResult<()> {
    #[cfg(windows)]
    {
        crate::windows_service::run(root)
    }
    #[cfg(not(windows))]
    {
        run_controlled_service(root)
    }
}

#[cfg(windows)]
pub(crate) fn run_controlled_service(root: &Path) -> PackageResult<()> {
    let stop_requested = Arc::new(AtomicBool::new(false));
    let reader_stop = Arc::clone(&stop_requested);
    std::thread::spawn(move || {
        let _ = std::io::copy(&mut std::io::stdin(), &mut std::io::sink());
        reader_stop.store(true, Ordering::Release);
    });
    run_managed_service(root, || stop_requested.load(Ordering::Acquire), || {})
}

#[cfg(not(windows))]
fn run_controlled_service(root: &Path) -> PackageResult<()> {
    let stop_requested = Arc::new(AtomicBool::new(false));
    let reader_stop = Arc::clone(&stop_requested);
    std::thread::spawn(move || {
        let _ = std::io::copy(&mut std::io::stdin(), &mut std::io::sink());
        reader_stop.store(true, Ordering::Release);
    });
    run_managed_service(root, || stop_requested.load(Ordering::Acquire), || {})
}

#[cfg(target_os = "linux")]
static USER_SERVICE_STOP_REQUESTED: AtomicBool = AtomicBool::new(false);

#[cfg(target_os = "linux")]
extern "C" fn request_user_service_stop(_: libc::c_int) {
    USER_SERVICE_STOP_REQUESTED.store(true, Ordering::Relaxed);
}

#[cfg(target_os = "linux")]
struct UserServiceSignals {
    previous_term: libc::sighandler_t,
    previous_int: libc::sighandler_t,
}

#[cfg(target_os = "linux")]
impl UserServiceSignals {
    fn install() -> PackageResult<Self> {
        USER_SERVICE_STOP_REQUESTED.store(false, Ordering::Relaxed);
        // SAFETY: the handler only stores to a lock-free atomic and performs no
        // allocation, logging, or other non-signal-safe work.
        let previous_term = unsafe {
            libc::signal(
                libc::SIGTERM,
                request_user_service_stop as *const () as libc::sighandler_t,
            )
        };
        if previous_term == libc::SIG_ERR {
            return Err("could not install SIGTERM handler for user service".into());
        }
        // SAFETY: same minimal signal-safe handler; restore SIGTERM if this
        // second registration fails.
        let previous_int = unsafe {
            libc::signal(
                libc::SIGINT,
                request_user_service_stop as *const () as libc::sighandler_t,
            )
        };
        if previous_int == libc::SIG_ERR {
            // SAFETY: previous_term was returned by libc::signal above.
            unsafe { libc::signal(libc::SIGTERM, previous_term) };
            return Err("could not install SIGINT handler for user service".into());
        }
        Ok(Self {
            previous_term,
            previous_int,
        })
    }
}

#[cfg(target_os = "linux")]
impl Drop for UserServiceSignals {
    fn drop(&mut self) {
        // SAFETY: restore the process handlers returned by libc::signal.
        unsafe {
            libc::signal(libc::SIGTERM, self.previous_term);
            libc::signal(libc::SIGINT, self.previous_int);
        }
    }
}

/// Runs the active signed package under a systemd --user manager. The expected
/// digest pins the unit to one immutable component set; SIGTERM/SIGINT enter
/// orderly component teardown and Linux input revocation.
#[cfg(target_os = "linux")]
pub fn user_service(root: &Path, expected_digest: &str) -> PackageResult<()> {
    // SAFETY: geteuid takes no pointers and only reports the calling process uid.
    if unsafe { libc::geteuid() } == 0 {
        return Err("Linux user service refuses to run as root".into());
    }
    let root = validate_user_service_root(root)?;
    if !valid_package_digest(expected_digest) {
        return Err("pinned active package digest is invalid".into());
    }
    let bootstrap: BootstrapConfig = read_json(&root.join("bootstrap.json"))?;
    if bootstrap.development || bootstrap.allow_unsigned || bootstrap.trusted_keys.is_empty() {
        return Err("Linux user service requires a production signed-package root".into());
    }
    let _signals = UserServiceSignals::install()?;
    run_linux_user_service(&root, expected_digest)
}

#[cfg(target_os = "linux")]
pub(crate) fn validate_user_service_root(root: &Path) -> PackageResult<std::path::PathBuf> {
    use std::path::Component;

    if !root.is_absolute()
        || root
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::CurDir))
    {
        return Err("Linux user service root must be an absolute normalized path".into());
    }
    let mut current = std::path::PathBuf::new();
    for component in root.components() {
        current.push(component.as_os_str());
        let metadata = std::fs::symlink_metadata(&current).map_err(|error| error.to_string())?;
        if metadata.file_type().is_symlink() {
            return Err("Linux user service root cannot contain symlinks".into());
        }
    }
    let canonical = root.canonicalize().map_err(|error| error.to_string())?;
    let metadata = std::fs::metadata(&canonical).map_err(|error| error.to_string())?;
    if canonical == Path::new("/") || !metadata.is_dir() {
        return Err("Linux user service root must be a non-root directory".into());
    }
    use std::os::unix::fs::MetadataExt;
    // SAFETY: geteuid takes no pointers and only reports the current uid.
    if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o022 != 0 {
        return Err(
            "Linux user service root must be user-owned and not group/world writable".into(),
        );
    }
    Ok(canonical)
}

#[cfg(target_os = "linux")]
fn valid_package_digest(digest: &str) -> bool {
    amaterasu_package::linux_user_unit::valid_digest(digest)
}

#[cfg(target_os = "linux")]
fn validate_active_package_pin(expected: &str, active: &str) -> PackageResult<()> {
    if !valid_package_digest(expected) || expected != active {
        return Err("active package digest does not match the service unit pin".into());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
enum LinuxUserAction {
    Lifecycle(HostLifecycleAction),
    Activate(amaterasu_contracts::HostPackageActivation),
}

#[cfg(target_os = "linux")]
fn run_linux_user_service(root: &Path, expected_digest: &str) -> PackageResult<()> {
    let _lock = RootLock::acquire(root)?;
    let mut trial_pending =
        match crate::package_activation::startup_transition(root, expected_digest)? {
            crate::package_activation::StartupTransition::None => false,
            crate::package_activation::StartupTransition::Trial => true,
            crate::package_activation::StartupTransition::Recovered => {
                if let Ok(state) = validate_user_state_root(&root.join("data/state")) {
                    let _ =
                        crate::package_activation::write_status(&state, expected_digest, "failed");
                }
                return Err(
                    "interrupted package transition recovered; service restart required".into(),
                );
            }
        };
    let result = run_linux_user_service_inner(root, expected_digest, &mut trial_pending);
    if result.is_err() && trial_pending {
        let recovered = crate::package_activation::recover(root, expected_digest);
        if let Ok(state) = validate_user_state_root(&root.join("data/state")) {
            let _ = crate::package_activation::write_status(&state, expected_digest, "failed");
        }
        if let Err(error) = recovered {
            return Err(format!("trial failed and recovery is required: {error}"));
        }
    }
    result
}

#[cfg(target_os = "linux")]
fn run_linux_user_service_inner(
    root: &Path,
    expected_digest: &str,
    trial_pending: &mut bool,
) -> PackageResult<()> {
    let digest = crate::supervisor::select_active(root)?;
    validate_active_package_pin(expected_digest, &digest)?;
    let bootstrap: BootstrapConfig = read_json(&root.join("bootstrap.json"))?;
    if bootstrap.development || bootstrap.allow_unsigned || bootstrap.trusted_keys.is_empty() {
        return Err("Linux user service requires a production signed-package root".into());
    }
    let selection: ActivationState = read_json(&root.join("selection.json"))?;
    if selection.active.as_deref() != Some(digest.as_str()) || selection.pending.is_some() {
        return Err("Linux user service active selection changed during startup".into());
    }
    let state_root = validate_user_state_root(&root.join("data/state"))?;
    let mut input = crate::linux_input::Supervisor::new(root, &digest)?;
    let mut restarts = crate::protocol::RestartBudget::default();
    let result = loop {
        let outcome = (|| {
            let mut components =
                load_linux_user_components(root, &digest, &bootstrap, &state_root)?;
            components.tick()?;
            components.health()?;
            let pid = std::process::id();
            let starttime = crate::linux_input::process_starttime(pid)?;
            input.refresh(root, &digest, pid, starttime)?;
            publish_service_readiness(root, &digest)?;
            if *trial_pending {
                crate::package_activation::finalize(root, &digest)?;
                *trial_pending = false;
                if let Err(error) =
                    crate::package_activation::write_status(&state_root, &digest, "committed")
                {
                    eprintln!("package activation receipt unavailable: {error}");
                }
            }

            // Give management time to deliver its accepted response before
            // stopping its listener for the trial and service restart.
            let mut pending_activation: Option<(
                amaterasu_contracts::HostPackageActivation,
                Instant,
            )> = None;
            while !USER_SERVICE_STOP_REQUESTED.load(Ordering::Relaxed) {
                let tick_started = Instant::now();
                components.tick()?;
                components.health()?;
                if pending_activation
                    .as_ref()
                    .is_some_and(|(_, deadline)| Instant::now() >= *deadline)
                {
                    let (request, _) = pending_activation.take().expect("checked pending request");
                    input.revoke();
                    if let Err(error) = components.quiesce(Duration::from_secs(20)) {
                        let _ = crate::package_activation::write_status(
                            &state_root,
                            &request.manifest_digest,
                            "failed",
                        );
                        return Err(format!(
                            "cannot retire the running package for activation: {error}"
                        ));
                    }
                    drop(components);
                    return Ok(Some(LinuxUserAction::Activate(request)));
                }
                if pending_activation.is_none()
                    && let Some(request) = components.take_host_package_activation()?
                {
                    pending_activation =
                        Some((request, Instant::now() + Duration::from_millis(250)));
                }
                if let Some(action) = components.take_host_lifecycle_action()? {
                    input.revoke();
                    drop(components);
                    return Ok(Some(LinuxUserAction::Lifecycle(action)));
                }
                input.refresh(root, &digest, pid, starttime)?;
                std::thread::sleep(crate::timing::remaining_tick_interval(
                    tick_started.elapsed(),
                ));
            }
            input.revoke();
            drop(components);
            Ok(None)
        })();
        input.revoke();
        remove_service_readiness(root);
        match outcome {
            Ok(Some(LinuxUserAction::Activate(request))) => {
                if let Err(error) = crate::package_activation::write_status(
                    &state_root,
                    &request.manifest_digest,
                    "activating",
                ) {
                    eprintln!("package activation receipt unavailable: {error}");
                    continue;
                }
                match crate::package_activation::activate(root, &digest, &state_root, &request) {
                    Ok(()) => {
                        break Err(
                            "package activation committed; restart with the updated service pin"
                                .into(),
                        );
                    }
                    Err(error) => {
                        let _ = crate::package_activation::write_status(
                            &state_root,
                            &request.manifest_digest,
                            "failed",
                        );
                        eprintln!("package activation failed: {error}");
                        if std::fs::symlink_metadata(root.join("package-activation-journal.json"))
                            .is_ok()
                        {
                            break Err("package activation recovery is required".into());
                        }
                    }
                }
            }
            Ok(Some(LinuxUserAction::Lifecycle(HostLifecycleAction::Restart))) => {
                let Some(delay) = restarts.next_delay() else {
                    break Err("user service restart budget exhausted".into());
                };
                std::thread::sleep(delay);
            }
            Ok(Some(LinuxUserAction::Lifecycle(HostLifecycleAction::Quit)) | None) => break Ok(()),
            Err(error) => break Err(error),
        }
    };
    remove_service_readiness(root);
    result
}

#[cfg(target_os = "linux")]
fn load_linux_user_components(
    root: &Path,
    digest: &str,
    bootstrap: &BootstrapConfig,
    state_root: &Path,
) -> PackageResult<crate::loading::LoadedComponents> {
    let package_path = package_directory(root, digest)?;
    let mut progress = |_: usize| Ok(());
    let mut component_progress = |_: usize, _: usize| Ok(());
    crate::publisher_selection::with_verified_selected(
        root,
        digest,
        bootstrap,
        crate::publisher_selection::SelectionPurpose::Worker,
        &mut component_progress,
        |package| {
            if !package.signed
                || package.digest != digest
                || package.manifest.host_context_schema != 1
            {
                return Err("user service package lost its signed active identity".into());
            }
            let logging = crate::logging_config::HostLoggingConfig::from_global(
                bootstrap.policy.settings.get("global"),
                state_root,
            )?;
            let mut package = package;
            logging.inject(&mut package.runtime_config)?;
            // Only this verified Linux user-service supervisor handles package requests.
            package.runtime_config.host_package_activation_control = true;
            crate::loading::LoadedComponents::load(
                root,
                &package_path,
                &package,
                state_root,
                logging.start_product_logger(),
                &mut progress,
            )
        },
    )
}

#[cfg(target_os = "linux")]
pub(crate) fn validate_user_state_root(path: &Path) -> PackageResult<std::path::PathBuf> {
    use std::os::unix::fs::MetadataExt;

    if !path.is_absolute() {
        return Err("Linux user service state root must be absolute".into());
    }
    let mut current = std::path::PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        let metadata = std::fs::symlink_metadata(&current).map_err(|error| error.to_string())?;
        if metadata.file_type().is_symlink() {
            return Err("Linux user service state root cannot contain symlinks".into());
        }
    }
    let canonical = path.canonicalize().map_err(|error| error.to_string())?;
    let metadata = std::fs::metadata(&canonical).map_err(|error| error.to_string())?;
    // SAFETY: geteuid takes no pointers and only reports the current uid.
    if !metadata.is_dir()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o022 != 0
    {
        return Err(
            "Linux user service state root must be user-owned and not group/world writable".into(),
        );
    }
    Ok(canonical)
}

pub(crate) fn run_managed_service(
    root: &Path,
    mut stop_requested: impl FnMut() -> bool,
    on_started: impl FnOnce(),
) -> PackageResult<()> {
    let _lock = RootLock::acquire(root)?;
    let digest = crate::supervisor::select_active(root)?;
    #[cfg(target_os = "linux")]
    let mut linux_input = crate::linux_input::Supervisor::new(root, &digest)?;
    let mut restarts = crate::protocol::RestartBudget::default();
    let mut worker = start_with_recovery(root, &digest, &mut restarts, false)?;
    #[cfg(target_os = "linux")]
    linux_input.refresh(root, &digest, worker.child.id(), worker.creation_time)?;
    publish_service_readiness(root, &digest)?;
    on_started();

    while !stop_requested() {
        match worker.check_idle() {
            Ok(Some(HostLifecycleAction::Restart)) => {
                #[cfg(target_os = "linux")]
                linux_input.revoke();
                let Some(replacement) = apply_worker_action(
                    HostLifecycleAction::Restart,
                    worker,
                    root,
                    &digest,
                    &mut restarts,
                    false,
                )?
                else {
                    unreachable!("restart action always creates a replacement worker")
                };
                worker = replacement;
            }
            Ok(Some(HostLifecycleAction::Quit)) => break,
            Ok(None) => {}
            Err(_) => {
                #[cfg(target_os = "linux")]
                linux_input.revoke();
                drop(worker);
                worker = restart(root, &digest, &mut restarts, false)?;
            }
        }
        #[cfg(target_os = "linux")]
        linux_input.refresh(root, &digest, worker.child.id(), worker.creation_time)?;
        std::thread::sleep(Duration::from_millis(20));
    }
    #[cfg(target_os = "linux")]
    linux_input.revoke();
    let result = worker.shutdown();
    remove_service_readiness(root);
    result
}

#[cfg(windows)]
pub(crate) fn run_interactive_service(
    root: &Path,
    mut stop_requested: impl FnMut() -> bool,
    mut session_changed: impl FnMut() -> bool,
    on_started: impl FnOnce(),
) -> PackageResult<()> {
    let _lock = RootLock::acquire(root)?;
    let digest = crate::supervisor::select_active(root)?;
    let mut supervisor = crate::interactive_worker::Supervisor::new(root, &digest)?;
    if !supervisor.has_ready_worker()? {
        // SCM may keep the service alive while no user is signed in, but that
        // is not an install-time readiness condition: no stream-capable
        // worker exists yet. Leaving the marker absent makes the MSI health
        // gate fail closed and restore a predecessor rather than claim success.
        return Err("no active interactive worker is ready".into());
    }
    publish_service_readiness(root, &digest)?;
    on_started();
    while !stop_requested() {
        if supervisor.refresh(session_changed())? == Some(HostLifecycleAction::Quit) {
            break;
        }
        if !supervisor.has_ready_worker()? {
            remove_service_readiness(root);
        } else if !root
            .join(SERVICE_READY_FILE)
            .try_exists()
            .map_err(|error| error.to_string())?
        {
            publish_service_readiness(root, &digest)?;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    let result = supervisor.shutdown();
    remove_service_readiness(root);
    result
}

fn publish_service_readiness(root: &Path, digest: &str) -> PackageResult<()> {
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err("selected service package identity is invalid".into());
    }
    write_json_atomically(
        &root.join(SERVICE_READY_FILE),
        &ServiceReadiness {
            schema: 1,
            process_id: std::process::id(),
            digest: digest.into(),
        },
    )
}

fn remove_service_readiness(root: &Path) {
    let _ = std::fs::remove_file(root.join(SERVICE_READY_FILE));
}

fn write_reply(value: &impl serde::Serialize) -> PackageResult<()> {
    let mut output = std::io::stdout().lock();
    serde_json::to_writer(&mut output, value).map_err(|error| error.to_string())?;
    writeln!(output)
        .and_then(|()| output.flush())
        .map_err(|error| error.to_string())
}

enum WorkerEvent {
    Response(serde_json::Value),
    HostAction(HostLifecycleAction),
}

struct Worker {
    child: Child,
    #[cfg(target_os = "linux")]
    creation_time: u64,
    input: Option<ChildStdin>,
    output: Receiver<Result<String, String>>,
    last_heartbeat: Instant,
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Worker {
    fn start(root: &Path, digest: &str, announce_ready: bool) -> PackageResult<Self> {
        let mut child = Command::new(std::env::current_exe().map_err(|error| error.to_string())?)
            .arg("worker-serve")
            .arg(root)
            .arg(digest)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|error| error.to_string())?;
        #[cfg(target_os = "linux")]
        let creation_time = match crate::linux_input::process_starttime(child.id()) {
            Ok(starttime) => starttime,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        };
        let input = child.stdin.take();
        let stdout = child.stdout.take().ok_or("worker stdout missing")?;
        let mut worker = Self {
            child,
            #[cfg(target_os = "linux")]
            creation_time,
            input,
            output: crate::protocol::lines(stdout),
            last_heartbeat: Instant::now(),
        };
        let mut deadline = Instant::now() + LIVENESS_TIMEOUT;
        let mut startup = crate::supervisor::StartupProtocol::default();
        loop {
            if Instant::now() >= deadline {
                return Err("worker made no startup progress for five seconds".into());
            }
            match worker.output.recv_timeout(Duration::from_millis(20)) {
                Ok(Ok(line)) => match startup.observe(&line) {
                    Ok(false) => deadline = Instant::now() + LIVENESS_TIMEOUT,
                    Ok(true) => {
                        worker.last_heartbeat = Instant::now();
                        if announce_ready {
                            eprintln!("Development worker ready: pid={}", worker.child.id());
                        }
                        return Ok(worker);
                    }
                    Err(failure) => return Err(failure),
                },
                Ok(Err(failure)) => return Err(failure),
                Err(RecvTimeoutError::Timeout) => {}
                Err(failure) => {
                    return Err(format!("worker did not become ready: {failure:?}"));
                }
            }
        }
    }

    fn check_process(&mut self) -> PackageResult<Option<HostLifecycleAction>> {
        if let Some(status) = self.child.try_wait().map_err(|error| error.to_string())? {
            let deadline = Instant::now() + Duration::from_millis(50);
            loop {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Err(format!("worker exited without lifecycle action: {status}"));
                }
                match self.output.recv_timeout(remaining) {
                    Ok(Ok(line)) if line == "HEARTBEAT" => {
                        self.last_heartbeat = Instant::now();
                    }
                    Ok(Ok(line)) => {
                        if let Some(action) = crate::protocol::parse_host_action(&line)? {
                            return crate::protocol::acknowledge_host_action(
                                action,
                                status.success(),
                            )
                            .map(Some);
                        }
                        return Err("unexpected idle worker message".into());
                    }
                    Ok(Err(failure)) => return Err(failure),
                    Err(_) => {
                        return Err(format!("worker exited without lifecycle action: {status}"));
                    }
                }
            }
        }
        if self.last_heartbeat.elapsed() >= LIVENESS_TIMEOUT {
            return Err("worker heartbeat deadline elapsed".into());
        }
        Ok(None)
    }

    fn check_idle(&mut self) -> PackageResult<Option<HostLifecycleAction>> {
        // Bound processing even if a faulty native module floods the pipe.
        for _ in 0..8 {
            match self.output.try_recv() {
                Ok(Ok(line)) if line == "HEARTBEAT" => self.last_heartbeat = Instant::now(),
                Ok(Ok(line)) => {
                    let Some(action) = crate::protocol::parse_host_action(&line)? else {
                        return Err("unexpected idle worker message".into());
                    };
                    return self.acknowledge_host_action(action).map(Some);
                }
                Ok(Err(failure)) => return Err(failure),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return Err("worker output closed".into()),
            }
        }
        self.check_process()
    }

    fn acknowledge_host_action(
        &mut self,
        action: HostLifecycleAction,
    ) -> PackageResult<HostLifecycleAction> {
        let deadline = Instant::now() + Duration::from_secs(25);
        loop {
            if let Some(status) = self.child.try_wait().map_err(|error| error.to_string())? {
                return crate::protocol::acknowledge_host_action(action, status.success());
            }
            if Instant::now() >= deadline {
                return Err("worker lifecycle action exit deadline elapsed".into());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn execute(&mut self, line: &str) -> PackageResult<WorkerEvent> {
        let mut input = self.input.take().ok_or("worker stdin closed")?;
        let line = line.to_owned();
        let (delivered, delivery) = std::sync::mpsc::sync_channel(1);
        // Pipe writes can block when a worker is stopped. Keep delivery off the
        // watchdog thread; killing/reaping the failed worker closes its reader
        // and releases this writer. At most one writer exists per live worker.
        std::thread::spawn(move || {
            let result = writeln!(input, "{line}")
                .and_then(|()| input.flush())
                .map(|()| input)
                .map_err(|error| error.to_string());
            let _ = delivered.send(result);
        });
        let mut reply = None;
        let deadline = Instant::now() + REQUEST_TIMEOUT;
        loop {
            if self.input.is_none() {
                match delivery.try_recv() {
                    Ok(Ok(input)) => self.input = Some(input),
                    Ok(Err(failure)) => return Err(failure),
                    Err(TryRecvError::Empty) => {}
                    Err(TryRecvError::Disconnected) => {
                        return Err("worker input writer stopped".into());
                    }
                }
            }
            if self.input.is_some()
                && let Some(reply) = reply.take()
            {
                return Ok(WorkerEvent::Response(reply));
            }
            if Instant::now() >= deadline {
                return Err("worker request deadline elapsed".into());
            }
            match self.output.recv_timeout(Duration::from_millis(20)) {
                Ok(Ok(line)) if line == "HEARTBEAT" => self.last_heartbeat = Instant::now(),
                Ok(Ok(line)) if line.starts_with("RESULT ") => {
                    let response: Response =
                        serde_json::from_str(&line[7..]).map_err(|error| error.to_string())?;
                    if reply.is_some() {
                        return Err("worker returned duplicate responses".into());
                    }
                    reply =
                        Some(serde_json::to_value(response).map_err(|error| error.to_string())?);
                }
                Ok(Ok(line)) => {
                    let Some(action) = crate::protocol::parse_host_action(&line)? else {
                        return Err("unexpected worker response".into());
                    };
                    return self
                        .acknowledge_host_action(action)
                        .map(WorkerEvent::HostAction);
                }
                Ok(Err(failure)) => return Err(failure),
                Err(RecvTimeoutError::Disconnected) => return Err("worker output closed".into()),
                Err(RecvTimeoutError::Timeout) => {}
            }
            if let Some(action) = self.check_process()? {
                return Ok(WorkerEvent::HostAction(action));
            }
        }
    }

    fn shutdown(&mut self) -> PackageResult<()> {
        // EOF asks modules to destroy in reverse load order before runtime teardown.
        self.input.take();
        let deadline = Instant::now() + WORKER_SHUTDOWN_TIMEOUT;
        loop {
            if let Some(status) = self.child.try_wait().map_err(|error| error.to_string())? {
                return if status.success() {
                    Ok(())
                } else {
                    Err(format!("worker shutdown failed: {status}"))
                };
            }
            if Instant::now() >= deadline {
                return Err("worker shutdown deadline elapsed".into());
            }
            // Drain output so teardown cannot be blocked by a full protocol channel.
            for _ in 0..8 {
                if self.output.try_recv().is_err() {
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

fn start_with_recovery(
    root: &Path,
    digest: &str,
    restarts: &mut crate::protocol::RestartBudget,
    announce_ready: bool,
) -> PackageResult<Worker> {
    match Worker::start(root, digest, announce_ready) {
        Ok(worker) => Ok(worker),
        Err(failure) => {
            if announce_ready {
                eprintln!("Development worker startup failed: {failure}");
            }
            restart(root, digest, restarts, announce_ready)
        }
    }
}

fn restart(
    root: &Path,
    digest: &str,
    restarts: &mut crate::protocol::RestartBudget,
    announce_ready: bool,
) -> PackageResult<Worker> {
    let delay = restarts
        .next_delay()
        .ok_or("development worker restart budget exhausted")?;
    std::thread::sleep(delay);
    start_with_recovery(root, digest, restarts, announce_ready)
}

fn apply_worker_action(
    action: HostLifecycleAction,
    mut worker: Worker,
    root: &Path,
    digest: &str,
    restarts: &mut crate::protocol::RestartBudget,
    announce_ready: bool,
) -> PackageResult<Option<Worker>> {
    match action {
        HostLifecycleAction::Restart => {
            drop(worker);
            restart(root, digest, restarts, announce_ready).map(Some)
        }
        HostLifecycleAction::Quit => {
            worker.shutdown()?;
            Ok(None)
        }
    }
}

#[cfg(test)]
mod readiness_tests {
    use super::*;

    #[test]
    fn readiness_requires_the_service_owned_marker_to_match_active_selection() {
        let root =
            std::env::temp_dir().join(format!("amaterasu-service-ready-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let digest = "a".repeat(64);
        amaterasu_package::write_json_atomically(
            &root.join("selection.json"),
            &serde_json::json!({"active":digest,"previous":null,"pending":null,"trial":null,"highest_sequence":1}),
        ).unwrap();
        publish_service_readiness(&root, &digest).unwrap();
        probe_service_readiness(&root).unwrap();
        std::fs::write(root.join(SERVICE_READY_FILE), b"{}").unwrap();
        assert!(probe_service_readiness(&root).is_err());
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn user_service_requires_the_exact_valid_active_digest_pin() {
        let digest = "a".repeat(64);
        assert!(validate_active_package_pin(&digest, &digest).is_ok());
        assert!(validate_active_package_pin(&digest, &"b".repeat(64)).is_err());
        assert!(validate_active_package_pin("../selection.json", &digest).is_err());
    }
}
