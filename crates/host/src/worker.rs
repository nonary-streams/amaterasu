//! Short-lived development worker used to prove native loading and request routing.
use amaterasu_abi::HostApi;
use amaterasu_contracts::{Command, HostLifecycleAction, Response};
use amaterasu_package::{BootstrapConfig, PackageResult, package_directory, read_json};
use serde_json::Value;
use std::io::Write;
use std::path::Path;

use crate::loading::LoadedComponents;

pub fn run(root: &Path, digest: &str, input: Option<&str>) -> PackageResult<()> {
    let mut startup_progress = send_startup_progress;
    let state_root = development_state_root(root)?;
    let mut components = load_components(root, digest, &state_root, &mut startup_progress)?;
    components.tick()?;
    components.health()?;
    send_line("READY")?;

    if let Some(input) = input {
        let endpoint = components.console_endpoint()?;
        let mut request_progress = |completed| send_line(&format!("PROGRESS REQUEST {completed}"));
        let response = execute_input(&mut components, &endpoint, input, &mut request_progress)?;
        send_line(&format!(
            "RESULT {}",
            serde_json::to_string(&response).map_err(|error| error.to_string())?
        ))?;
    }
    // Destruction is included in the supervisor deadline as well as startup and dispatch.
    drop(components);
    Ok(())
}

fn execute_input(
    components: &mut LoadedComponents,
    endpoint: &HostApi,
    input: &str,
    progress: &mut dyn FnMut(usize) -> PackageResult<()>,
) -> PackageResult<Value> {
    if input.len() > 65_536 {
        return Err("development probe input exceeds 64 KiB".into());
    }
    let message: Value = serde_json::from_str(input).map_err(|error| error.to_string())?;
    if let Value::Array(commands) = message {
        if commands.is_empty() || commands.len() > 32 {
            return Err("a development probe batch must contain 1 through 32 commands".into());
        }
        let mut responses = Vec::with_capacity(commands.len());
        for (index, command) in commands.into_iter().enumerate() {
            let command = serde_json::from_value(command).map_err(|error| error.to_string())?;
            responses.push(execute_request(components, endpoint, command)?);
            progress(index + 1)?;
        }
        serde_json::to_value(responses).map_err(|error| error.to_string())
    } else {
        let command = serde_json::from_value(message).map_err(|error| error.to_string())?;
        serde_json::to_value(execute_request(components, endpoint, command)?)
            .map_err(|error| error.to_string())
    }
}

fn execute_request(
    components: &mut LoadedComponents,
    endpoint: &HostApi,
    command: Command,
) -> PackageResult<Response> {
    let initial_response = invoke(endpoint, &command)?;
    let Response::Accepted { id: request_id } = initial_response else {
        return Ok(initial_response);
    };
    if !matches!(command, Command::Request { .. }) {
        return Ok(initial_response);
    }

    for _ in 0..100 {
        components.tick()?;
        components.health()?;
        let response = invoke(
            endpoint,
            &Command::TakeResult {
                request: request_id,
            },
        )?;
        if !matches!(response, Response::Pending) {
            return Ok(response);
        }
        std::thread::sleep(crate::timing::TICK_INTERVAL);
    }
    Err("development request did not complete within the probe window".into())
}

pub(crate) fn invoke(endpoint: &HostApi, command: &Command) -> PackageResult<Response> {
    let bytes = serde_json::to_vec(command).map_err(|error| error.to_string())?;
    // SAFETY: the worker retains the runtime through the complete request and reply.
    let reply =
        unsafe { endpoint.call(&bytes) }.map_err(|status| format!("transport status {status}"))?;
    serde_json::from_slice(&reply).map_err(|error| error.to_string())
}

fn send_line(line: &str) -> PackageResult<()> {
    println!("{line}");
    std::io::stdout().flush().map_err(|error| error.to_string())
}

fn load_components(
    root: &Path,
    digest: &str,
    state_root: &Path,
    progress: &mut dyn FnMut(StartupProgress) -> PackageResult<()>,
) -> PackageResult<LoadedComponents> {
    let bootstrap: BootstrapConfig = read_json(&root.join("bootstrap.json"))?;
    if !bootstrap.development && bootstrap.allow_unsigned {
        return Err("production bootstrap cannot permit unsigned packages".into());
    }
    let directory = package_directory(root, digest)?;
    let progress = std::cell::RefCell::new(progress);
    let mut verification_progress = |verified, total| {
        if verified == 0 {
            progress.borrow_mut()(StartupProgress::Manifest(total))
        } else {
            progress.borrow_mut()(StartupProgress::Artifact(verified))
        }
    };
    crate::publisher_selection::with_verified_selected(
        root,
        digest,
        &bootstrap,
        crate::publisher_selection::SelectionPurpose::Worker,
        &mut verification_progress,
        |mut package| {
            if package.digest != digest {
                return Err("selected package identity does not match its manifest".into());
            }
            let logging = crate::logging_config::HostLoggingConfig::from_global(
                bootstrap.policy.settings.get("global"),
                state_root,
            )?;
            logging.inject(&mut package.runtime_config)?;
            if !bootstrap.development && !package.signed {
                return Err("production service requires a signed package".into());
            }
            if !bootstrap.development && package.manifest.host_context_schema != 1 {
                return Err("production package has an incompatible host context ABI".into());
            }
            let mut loaded = |index| progress.borrow_mut()(StartupProgress::Component(index));
            LoadedComponents::load(
                root,
                &directory,
                &package,
                state_root,
                logging.start_product_logger(),
                &mut loaded,
            )
        },
    )
}

/// Console and probe modes retain an isolated development root. Production
/// workers receive a state root from the SCM token boundary instead.
fn development_state_root(root: &Path) -> PackageResult<std::path::PathBuf> {
    let bootstrap: BootstrapConfig = read_json(&root.join("bootstrap.json"))?;
    if !bootstrap.development {
        return Err("production packages must be started by the SCM service".into());
    }
    let state_root = root.join("data").join("state");
    amaterasu_package::create_private_directory(&state_root)?;
    let state_root = state_root
        .canonicalize()
        .map_err(|error| error.to_string())?;
    #[cfg(windows)]
    if let Some(active) = crate::loading::management_tls::active_file_names(&bootstrap.policy)? {
        crate::state_root::protect_development_files(root, &state_root, &active)?;
    }
    Ok(state_root)
}

enum StartupProgress {
    Manifest(usize),
    Artifact(usize),
    Component(usize),
}

fn send_startup_progress(progress: StartupProgress) -> PackageResult<()> {
    match progress {
        StartupProgress::Manifest(total) => send_line(&format!("PROGRESS MANIFEST {total}")),
        StartupProgress::Artifact(index) => send_line(&format!("PROGRESS ARTIFACT {index}")),
        StartupProgress::Component(index) => send_line(&format!("PROGRESS COMPONENT {index}")),
    }
}

/// Development process lifetime; product behavior remains in loaded libraries.
pub fn serve(root: &Path, digest: &str) -> PackageResult<()> {
    let mut startup_progress = send_startup_progress;
    let state_root = development_state_root(root)?;
    let mut components = load_components(root, digest, &state_root, &mut startup_progress)?;
    let endpoint = components.console_endpoint()?;
    let input = crate::protocol::lines(std::io::stdin());
    components.tick()?;
    components.health()?;
    send_line("READY")?;
    let mut heartbeat = std::time::Instant::now();
    loop {
        let observed_work = components.observe_work()?;
        let tick_started = std::time::Instant::now();
        components.tick()?;
        components.health()?;
        if let Some(action) = components.take_host_lifecycle_action()? {
            return finish_lifecycle_action(components, action);
        }
        pulse(&mut heartbeat)?;
        match input.recv_timeout(std::time::Duration::ZERO) {
            Ok(Ok(line)) => {
                let outcome = match serde_json::from_str::<Command>(&line) {
                    Ok(command) => {
                        execute_live(&mut components, &endpoint, command, &mut heartbeat)?
                    }
                    Err(failure) => LiveWorkerOutput::Response(Response::error(
                        "invalid_request",
                        failure.to_string(),
                    )),
                };
                match outcome {
                    LiveWorkerOutput::Response(reply) => {
                        send_line(&format!(
                            "RESULT {}",
                            serde_json::to_string(&reply).map_err(|error| error.to_string())?
                        ))?;
                        if let Some(action) = components.take_host_lifecycle_action()? {
                            return finish_lifecycle_action(components, action);
                        }
                    }
                    LiveWorkerOutput::HostAction(action) => {
                        return finish_lifecycle_action(components, action);
                    }
                }
            }
            Ok(Err(failure)) => return Err(failure),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                components.wait_for_work(
                    observed_work,
                    crate::timing::remaining_tick_interval(tick_started.elapsed()),
                )?;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
        }
    }
}

/// Interactive-session worker lifetime. The SCM supervisor owns process
/// creation and termination; this user-token process never accepts console
/// commands or receives a privileged service handle.
pub fn serve_interactive(
    root: &Path,
    digest: &str,
    state_root: &Path,
    seed: Option<&Path>,
) -> PackageResult<()> {
    // Its stdout is a single inherited, write-only pipe to the Session 0
    // supervisor. It carries the exact bounded startup proof and is not a
    // console or a capability channel.
    #[cfg(windows)]
    crate::state_root::apply_staged_tls(root, state_root)?;
    #[cfg(windows)]
    if let Some(seed) = seed {
        crate::state_root::apply_pending_seed(seed, state_root)?;
    }
    #[cfg(not(windows))]
    if seed.is_some() {
        return Err("interactive migration seed requires Windows".into());
    }
    let mut startup_progress = |_| Ok(());
    let mut components = load_components(root, digest, state_root, &mut startup_progress)?;
    components.tick()?;
    components.health()?;
    send_line("READY")?;
    loop {
        let observed_work = components.observe_work()?;
        let tick_started = std::time::Instant::now();
        components.tick()?;
        components.health()?;
        if let Some(action) = components.take_host_lifecycle_action()? {
            return finish_lifecycle_action(components, action);
        }
        components.wait_for_work(
            observed_work,
            crate::timing::remaining_tick_interval(tick_started.elapsed()),
        )?;
    }
}

enum LiveWorkerOutput {
    Response(Response),
    HostAction(HostLifecycleAction),
}

fn execute_live(
    components: &mut LoadedComponents,
    endpoint: &HostApi,
    command: Command,
    heartbeat: &mut std::time::Instant,
) -> PackageResult<LiveWorkerOutput> {
    let response = invoke(endpoint, &command)?;
    let (Command::Request { timeout_ms, .. }, Response::Accepted { id }) = (&command, &response)
    else {
        return Ok(LiveWorkerOutput::Response(response));
    };
    let deadline =
        std::time::Instant::now() + std::time::Duration::from_millis((*timeout_ms).min(30_000));
    loop {
        components.tick()?;
        components.health()?;
        if let Some(action) = components.take_host_lifecycle_action()? {
            return Ok(LiveWorkerOutput::HostAction(action));
        }
        pulse(heartbeat)?;
        let result = invoke(endpoint, &Command::TakeResult { request: *id })?;
        if !matches!(result, Response::Pending) {
            return Ok(LiveWorkerOutput::Response(result));
        }
        if std::time::Instant::now() >= deadline {
            let _ = invoke(endpoint, &Command::Cancel { request: *id });
            return Ok(LiveWorkerOutput::Response(Response::error(
                "deadline_exceeded",
                "development request deadline elapsed; reconcile before retrying mutations",
            )));
        }
        std::thread::sleep(crate::timing::TICK_INTERVAL);
    }
}

fn finish_lifecycle_action(
    components: LoadedComponents,
    action: HostLifecycleAction,
) -> PackageResult<()> {
    lifecycle_action_after_cleanup(action, move || drop(components), send_host_action)
}

fn lifecycle_action_after_cleanup(
    action: HostLifecycleAction,
    cleanup: impl FnOnce(),
    emit: impl FnOnce(HostLifecycleAction) -> PackageResult<()>,
) -> PackageResult<()> {
    cleanup();
    emit(action)
}

fn send_host_action(action: HostLifecycleAction) -> PackageResult<()> {
    send_line(crate::protocol::host_action_line(action))
}

fn pulse(previous: &mut std::time::Instant) -> PackageResult<()> {
    if previous.elapsed() >= std::time::Duration::from_millis(100) {
        send_line("HEARTBEAT")?;
        *previous = std::time::Instant::now();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_action_is_emitted_only_after_cleanup_finishes() {
        let events = std::cell::RefCell::new(Vec::new());
        lifecycle_action_after_cleanup(
            HostLifecycleAction::Restart,
            || events.borrow_mut().push("cleanup"),
            |action| {
                assert_eq!(action, HostLifecycleAction::Restart);
                events.borrow_mut().push("action acknowledged");
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(events.into_inner(), ["cleanup", "action acknowledged"]);
    }
}
