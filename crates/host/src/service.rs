//! Development console supervision. Never replays a dispatched command.
use amaterasu_contracts::{Command as ControlCommand, Response};
use amaterasu_package::{PackageResult, RootLock};
use serde_json::json;
use std::io::Write;
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, TryRecvError};
use std::time::{Duration, Instant};

const LIVENESS_TIMEOUT: Duration = Duration::from_secs(5);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(35);
const MAX_RESTARTS: usize = 3;

pub fn serve(root: &Path) -> PackageResult<()> {
    let _lock = RootLock::acquire(root)?;
    // Candidate health and destruction are established in a separate disposable
    // process before console input is read or sent to the persistent worker.
    let digest = crate::supervisor::select_active(root)?;
    let input = crate::protocol::lines(std::io::stdin());
    let mut restarts = 0;
    let mut worker = start_with_recovery(root, &digest, &mut restarts)?;
    loop {
        if let Err(failure) = worker.check_idle() {
            eprintln!("Development worker failed: {failure}");
            drop(worker);
            worker = restart(root, &digest, &mut restarts)?;
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
                    Ok(reply) => write_reply(&reply)?,
                    Err(failure) => {
                        write_reply(&Response::error(
                            "worker_unavailable",
                            format!("{failure}; outcome may be unknown; command was not replayed"),
                        ))?;
                        drop(worker);
                        worker = restart(root, &digest, &mut restarts)?;
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

fn write_reply(value: &impl serde::Serialize) -> PackageResult<()> {
    let mut output = std::io::stdout().lock();
    serde_json::to_writer(&mut output, value).map_err(|error| error.to_string())?;
    writeln!(output)
        .and_then(|()| output.flush())
        .map_err(|error| error.to_string())
}

struct Worker {
    child: Child,
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
    fn start(root: &Path, digest: &str) -> PackageResult<Self> {
        let mut child = Command::new(std::env::current_exe().map_err(|error| error.to_string())?)
            .arg("worker-serve")
            .arg(root)
            .arg(digest)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|error| error.to_string())?;
        let input = child.stdin.take();
        let stdout = child.stdout.take().ok_or("worker stdout missing")?;
        let mut worker = Self {
            child,
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
                        eprintln!("Development worker ready: pid={}", worker.child.id());
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

    fn check_process(&mut self) -> PackageResult<()> {
        if let Some(status) = self.child.try_wait().map_err(|error| error.to_string())? {
            return Err(format!("worker exited: {status}"));
        }
        if self.last_heartbeat.elapsed() >= LIVENESS_TIMEOUT {
            return Err("worker heartbeat deadline elapsed".into());
        }
        Ok(())
    }

    fn check_idle(&mut self) -> PackageResult<()> {
        // Bound processing even if a faulty native module floods the pipe.
        for _ in 0..8 {
            match self.output.try_recv() {
                Ok(Ok(line)) if line == "HEARTBEAT" => self.last_heartbeat = Instant::now(),
                Ok(Ok(_)) => return Err("unexpected idle worker message".into()),
                Ok(Err(failure)) => return Err(failure),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return Err("worker output closed".into()),
            }
        }
        self.check_process()
    }

    fn execute(&mut self, line: &str) -> PackageResult<serde_json::Value> {
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
                return Ok(reply);
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
                Ok(Ok(_)) => return Err("unexpected worker response".into()),
                Ok(Err(failure)) => return Err(failure),
                Err(RecvTimeoutError::Disconnected) => return Err("worker output closed".into()),
                Err(RecvTimeoutError::Timeout) => {}
            }
            self.check_process()?;
        }
    }

    fn shutdown(&mut self) -> PackageResult<()> {
        // EOF asks modules to destroy in reverse load order before runtime teardown.
        self.input.take();
        let deadline = Instant::now() + LIVENESS_TIMEOUT;
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

fn start_with_recovery(root: &Path, digest: &str, restarts: &mut usize) -> PackageResult<Worker> {
    match Worker::start(root, digest) {
        Ok(worker) => Ok(worker),
        Err(failure) => {
            eprintln!("Development worker startup failed: {failure}");
            restart(root, digest, restarts)
        }
    }
}

fn restart(root: &Path, digest: &str, restarts: &mut usize) -> PackageResult<Worker> {
    if *restarts >= MAX_RESTARTS {
        return Err("development worker restart budget exhausted".into());
    }
    let delay = Duration::from_millis(100 << *restarts);
    *restarts += 1;
    std::thread::sleep(delay);
    start_with_recovery(root, digest, restarts)
}
