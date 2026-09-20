use amaterasu_package::{
    ActivationState, BootstrapConfig, PackageResult, RootLock, VerificationPolicy,
    package_directory, read_json, unix_timestamp, verify_package, write_json_atomically,
};
use std::io::BufReader;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::protocol::read_line as read_protocol_line;

const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

pub fn probe(root: &Path, input: Option<&str>) -> PackageResult<Option<String>> {
    let _lock = RootLock::acquire(root)?;
    let active = select_active(root)?;
    run_worker(root, &active, input)
}

pub(crate) fn select_active(root: &Path) -> PackageResult<String> {
    let bootstrap: BootstrapConfig = read_json(&root.join("bootstrap.json"))?;
    if bootstrap.allow_unsigned {
        if !bootstrap.development {
            return Err("production bootstrap cannot permit unsigned packages".into());
        }
        eprintln!("UNSIGNED MODULE MODE: native modules run with this process's privileges");
    }

    let selection_path = root.join("selection.json");
    let mut selection: ActivationState = read_json(&selection_path)?;
    recover_interrupted_trial(&selection_path, &mut selection)?;

    if !bootstrap.development && selection.pending.is_some() {
        return Err(
            "production package activation must be completed by authenticated provisioning".into(),
        );
    }

    if bootstrap.development
        && let Some(candidate) = selection.pending.clone()
    {
        probe_candidate(root, &selection_path, &mut selection, candidate)?;
    }
    let active = selection
        .active
        .as_ref()
        .ok_or("no active or pending component set")?;
    let active = active.clone();
    if !bootstrap.development {
        let package = verify_package(
            &package_directory(root, &active)?,
            &bootstrap,
            VerificationPolicy {
                minimum_sequence: selection.highest_sequence,
                allow_historical: false,
                now: unix_timestamp(),
            },
        )?;
        if !package.signed || package.digest != active || package.manifest.host_context_schema != 1
        {
            return Err("production active package is not an authenticated selection".into());
        }
    }
    Ok(active)
}

fn recover_interrupted_trial(path: &Path, selection: &mut ActivationState) -> PackageResult<()> {
    if selection.trial.take().is_some() {
        eprintln!("Previous activation was interrupted; retaining the known-good component set.");
        selection.pending = None;
        write_json_atomically(path, selection)?;
    }
    Ok(())
}

fn probe_candidate(
    root: &Path,
    selection_path: &Path,
    selection: &mut ActivationState,
    candidate: String,
) -> PackageResult<()> {
    selection.trial = Some(candidate.clone());
    write_json_atomically(selection_path, selection)?;

    // Activation probes must not execute caller mutations: retrying those against
    // a fallback could duplicate side effects. Establish health first, commit the
    // selected set, then execute the caller's input exactly once in another worker.
    let result = run_worker(root, &candidate, None);
    selection.pending = None;
    selection.trial = None;

    match result {
        Ok(_) => {
            if selection.active.as_ref() != Some(&candidate) {
                selection.previous = selection.active.replace(candidate.clone());
            }
            write_json_atomically(selection_path, selection)?;
            Ok(())
        }
        Err(failure) => {
            write_json_atomically(selection_path, selection)?;
            eprintln!("Candidate failed: {failure}");
            let active = selection
                .active
                .as_ref()
                .ok_or("candidate failed and no known-good set exists")?;
            eprintln!("Probing the previous known-good set.");
            run_worker(root, active, None).map(|_| ())
        }
    }
}

struct WorkerProcess {
    child: Child,
}

impl Drop for WorkerProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn run_worker(root: &Path, digest: &str, input: Option<&str>) -> PackageResult<Option<String>> {
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    let mut command = Command::new(executable);
    command
        .arg("worker")
        .arg(root)
        .arg(digest)
        .stdin(Stdio::null())
        .stdout(Stdio::piped());
    if let Some(input) = input {
        command.arg(input);
    }

    let child = command.spawn().map_err(|error| error.to_string())?;
    let mut worker = WorkerProcess { child };
    let stdout = worker
        .child
        .stdout
        .take()
        .ok_or("worker stdout was not piped")?;
    let (sender, receiver) = mpsc::sync_channel(8);
    let reader_thread = std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        while let Ok(Some(line)) = read_protocol_line(&mut reader) {
            if sender.send(line).is_err() {
                break;
            }
        }
    });

    let expected_requests = input.map(request_progress_count).unwrap_or(0);
    let result = monitor_worker(&mut worker, receiver, input.is_some(), expected_requests);
    drop(worker);
    // Do not let a helper that inherited stdout defeat the supervisor deadline.
    // The short-lived CLI process will reclaim any remaining reader thread on exit.
    drop(reader_thread);
    result
}

fn monitor_worker(
    worker: &mut WorkerProcess,
    receiver: mpsc::Receiver<String>,
    expect_reply: bool,
    expected_requests: usize,
) -> PackageResult<Option<String>> {
    let mut monitor = ProbeMonitor::new(Instant::now(), expect_reply, expected_requests);

    loop {
        if monitor.expired(Instant::now()) {
            return Err("native worker made no progress for five seconds".into());
        }
        match receiver.recv_timeout(Duration::from_millis(20)) {
            Ok(line) => monitor.observe(&line, Instant::now())?,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                if let Some(status) = worker.child.try_wait().map_err(|error| error.to_string())? {
                    if status.success() && monitor.complete() {
                        return Ok(monitor.reply);
                    }
                    return Err(format!("native worker failed: {status}"));
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
}

fn request_progress_count(input: &str) -> usize {
    match serde_json::from_str::<serde_json::Value>(input) {
        Ok(serde_json::Value::Array(commands)) if (1..=32).contains(&commands.len()) => {
            commands.len()
        }
        _ => 0,
    }
}

#[derive(Default)]
pub(crate) struct StartupProtocol {
    components: Option<usize>,
    verified_artifacts: usize,
    loaded: usize,
    ready: bool,
}

impl StartupProtocol {
    pub(crate) fn observe(&mut self, line: &str) -> PackageResult<bool> {
        let fields: Vec<_> = line.split_whitespace().collect();
        match fields.as_slice() {
            ["PROGRESS", "MANIFEST", count] if self.components.is_none() => {
                let count = count
                    .parse::<usize>()
                    .map_err(|_| "unexpected worker protocol message")?;
                if !(1..=64).contains(&count) {
                    return Err("unexpected worker protocol message".into());
                }
                self.components = Some(count);
                Ok(false)
            }
            ["PROGRESS", "ARTIFACT", index]
                if self.components.is_some() && self.loaded == 0 && !self.ready =>
            {
                let index = index
                    .parse::<usize>()
                    .map_err(|_| "unexpected worker protocol message")?;
                if index != self.verified_artifacts + 1 || index > self.components.unwrap() {
                    return Err("unexpected worker protocol message".into());
                }
                self.verified_artifacts = index;
                Ok(false)
            }
            ["PROGRESS", "COMPONENT", index]
                if self.components == Some(self.verified_artifacts) && !self.ready =>
            {
                let index = index
                    .parse::<usize>()
                    .map_err(|_| "unexpected worker protocol message")?;
                if index != self.loaded + 1 || index > self.components.unwrap() {
                    return Err("unexpected worker protocol message".into());
                }
                self.loaded = index;
                Ok(false)
            }
            ["READY"]
                if self.components == Some(self.verified_artifacts)
                    && self.components == Some(self.loaded)
                    && !self.ready =>
            {
                self.ready = true;
                Ok(true)
            }
            _ => Err("unexpected worker protocol message".into()),
        }
    }

    fn ready(&self) -> bool {
        self.ready
    }
}

struct ProbeMonitor {
    deadline: Instant,
    startup: StartupProtocol,
    expect_reply: bool,
    expected_requests: usize,
    completed_requests: usize,
    reply: Option<String>,
}

impl ProbeMonitor {
    fn new(now: Instant, expect_reply: bool, expected_requests: usize) -> Self {
        Self {
            deadline: now + PROBE_TIMEOUT,
            startup: StartupProtocol::default(),
            expect_reply,
            expected_requests,
            completed_requests: 0,
            reply: None,
        }
    }

    fn observe(&mut self, line: &str, now: Instant) -> PackageResult<()> {
        if !self.startup.ready() {
            self.startup.observe(line)?;
        } else if let Some(index) = line.strip_prefix("PROGRESS REQUEST ") {
            let index = index
                .parse::<usize>()
                .map_err(|_| "unexpected worker protocol message")?;
            if self.reply.is_some()
                || index != self.completed_requests + 1
                || index > self.expected_requests
            {
                return Err("unexpected worker protocol message".into());
            }
            self.completed_requests = index;
        } else if let Some(result) = line.strip_prefix("RESULT ") {
            if !self.expect_reply
                || self.reply.is_some()
                || self.completed_requests != self.expected_requests
            {
                return Err("unexpected worker protocol message".into());
            }
            self.reply = Some(result.to_owned());
        } else {
            return Err("unexpected worker protocol message".into());
        }
        self.deadline = now + PROBE_TIMEOUT;
        Ok(())
    }

    fn expired(&self, now: Instant) -> bool {
        now >= self.deadline
    }

    fn complete(&self) -> bool {
        self.startup.ready() && (!self.expect_reply || self.reply.is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legitimate_progress_extends_the_inactivity_deadline() {
        let start = Instant::now();
        let mut monitor = ProbeMonitor::new(start, false, 0);
        monitor.observe("PROGRESS MANIFEST 2", start).unwrap();
        monitor
            .observe("PROGRESS ARTIFACT 1", start + Duration::from_secs(4))
            .unwrap();
        assert!(!monitor.expired(start + Duration::from_secs(8)));
        monitor
            .observe("PROGRESS ARTIFACT 2", start + Duration::from_secs(8))
            .unwrap();
        monitor
            .observe("PROGRESS COMPONENT 1", start + Duration::from_secs(12))
            .unwrap();
        monitor
            .observe("PROGRESS COMPONENT 2", start + Duration::from_secs(16))
            .unwrap();
        monitor
            .observe("READY", start + Duration::from_secs(16))
            .unwrap();
        assert!(!monitor.expired(start + Duration::from_secs(20)));
        assert!(monitor.complete());
    }

    #[test]
    fn progress_is_finite_ordered_and_phase_specific() {
        for invalid in [
            vec!["PROGRESS MANIFEST 0"],
            vec!["PROGRESS MANIFEST 65"],
            vec!["PROGRESS MANIFEST 1", "PROGRESS MANIFEST 1"],
            vec!["PROGRESS MANIFEST 2", "PROGRESS ARTIFACT 2"],
            vec!["PROGRESS MANIFEST 2", "PROGRESS COMPONENT 1"],
            vec![
                "PROGRESS MANIFEST 1",
                "PROGRESS ARTIFACT 1",
                "PROGRESS ARTIFACT 1",
            ],
            vec![
                "PROGRESS MANIFEST 1",
                "PROGRESS ARTIFACT 1",
                "PROGRESS COMPONENT 1",
                "PROGRESS COMPONENT 2",
            ],
            vec!["PROGRESS MANIFEST 1", "PROGRESS ARTIFACT 1", "READY"],
        ] {
            let start = Instant::now();
            let mut monitor = ProbeMonitor::new(start, false, 0);
            assert!(
                invalid
                    .into_iter()
                    .any(|line| monitor.observe(line, start).is_err())
            );
        }
    }

    #[test]
    fn quiet_verification_tick_and_destruction_retain_five_second_bound() {
        let start = Instant::now();
        let mut verification = ProbeMonitor::new(start, false, 0);
        assert!(verification.expired(start + PROBE_TIMEOUT));

        verification.observe("PROGRESS MANIFEST 1", start).unwrap();
        verification.observe("PROGRESS ARTIFACT 1", start).unwrap();
        verification.observe("PROGRESS COMPONENT 1", start).unwrap();
        assert!(verification.expired(start + PROBE_TIMEOUT));

        let mut destruction = ProbeMonitor::new(start, false, 0);
        destruction.observe("PROGRESS MANIFEST 1", start).unwrap();
        destruction.observe("PROGRESS ARTIFACT 1", start).unwrap();
        destruction.observe("PROGRESS COMPONENT 1", start).unwrap();
        destruction.observe("READY", start).unwrap();
        assert!(destruction.expired(start + PROBE_TIMEOUT));
    }

    #[test]
    fn request_progress_is_bounded_by_the_validated_batch() {
        let start = Instant::now();
        let mut monitor = ProbeMonitor::new(start, true, 2);
        monitor.observe("PROGRESS MANIFEST 1", start).unwrap();
        monitor.observe("PROGRESS ARTIFACT 1", start).unwrap();
        monitor.observe("PROGRESS COMPONENT 1", start).unwrap();
        monitor.observe("READY", start).unwrap();
        monitor.observe("PROGRESS REQUEST 1", start).unwrap();
        assert!(monitor.observe("PROGRESS REQUEST 1", start).is_err());

        let mut monitor = ProbeMonitor::new(start, true, 2);
        monitor.observe("PROGRESS MANIFEST 1", start).unwrap();
        monitor.observe("PROGRESS ARTIFACT 1", start).unwrap();
        monitor.observe("PROGRESS COMPONENT 1", start).unwrap();
        monitor.observe("READY", start).unwrap();
        monitor.observe("PROGRESS REQUEST 1", start).unwrap();
        monitor.observe("PROGRESS REQUEST 2", start).unwrap();
        monitor.observe("RESULT []", start).unwrap();
        assert!(monitor.complete());
        assert!(monitor.observe("PROGRESS REQUEST 3", start).is_err());
    }
}
