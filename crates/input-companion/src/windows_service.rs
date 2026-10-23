//! Installed companion's single named-pipe authorization and VHF owner.
//! This module is not selected by the ordinary Amaterasu worker by default.
use crate::windows_identity::{inspect_process, opening_process, pipe_peer, service_process_id};
use crate::windows_pipe::{Pipe, PipeKind};
use crate::{
    GrantRegistry, InputEvent, InputPacket, InputResult, InputSession, PermitLedger,
    ProcessIdentity, VhfDeviceSink, WindowsVhfTransport,
};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};

const MAX_CONNECTIONS: usize = 8;
const SYSTEM_SID: &str = "S-1-5-18";

#[derive(Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
    HostPermit {
        process_id: u32,
        creation_time: u64,
        digest: String,
        keyboard: bool,
        mouse: bool,
    },
    WorkerStart,
    WorkerEvent {
        packet: InputPacket,
    },
    WorkerClose,
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Reply {
    Ok,
    Granted { generation: u64, token: String },
    Denied,
}

fn write_reply(pipe: &Pipe, reply: Reply) -> InputResult<()> {
    let bytes = serde_json::to_vec(&reply).map_err(|_| crate::InputError::InvalidEvent)?;
    pipe.write_line(&bytes)
}

fn read_request(pipe: &mut Pipe, deadline: Instant) -> InputResult<Option<Request>> {
    let Some(bytes) = pipe.read_line(deadline)? else {
        return Ok(None);
    };
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|_| crate::InputError::InvalidEvent)
}

fn handle_host(
    pipe: &Pipe,
    caller: &ProcessIdentity,
    request: Request,
    ledger: &Arc<Mutex<PermitLedger>>,
) -> InputResult<()> {
    if caller.principal != SYSTEM_SID || caller.process_id != service_process_id("Amaterasu")? {
        return write_reply(pipe, Reply::Denied);
    }
    let Request::HostPermit {
        process_id,
        creation_time,
        digest,
        keyboard,
        mouse,
    } = request
    else {
        return write_reply(pipe, Reply::Denied);
    };
    let process = inspect_process(process_id)?;
    if !process.matches_instance(process_id, creation_time)
        || process.session == 0
        || process.principal == SYSTEM_SID
    {
        return write_reply(pipe, Reply::Denied);
    }
    let granted = ledger
        .lock()
        .map_err(|_| crate::InputError::Closed)?
        .refresh_from_host(process, digest, keyboard, mouse, Instant::now())
        .is_ok();
    write_reply(pipe, if granted { Reply::Ok } else { Reply::Denied })
}

fn handle_worker(
    mut pipe: Pipe,
    process: ProcessIdentity,
    ledger: Arc<Mutex<PermitLedger>>,
    stop: Arc<AtomicBool>,
    cleanup_failed: Arc<AtomicBool>,
) -> InputResult<()> {
    let lease = ledger
        .lock()
        .map_err(|_| crate::InputError::Closed)?
        .claim(&process, Instant::now())?;
    let sink = WindowsVhfTransport::open().and_then(VhfDeviceSink::create);
    let sink = match sink {
        Ok(sink) => sink,
        Err(error) => {
            ledger
                .lock()
                .map_err(|_| crate::InputError::Closed)?
                .release(&process);
            return Err(error);
        }
    };
    let mut session = match InputSession::connect(&lease.peer, lease.grant, sink, Instant::now()) {
        Ok(session) => session,
        Err(error) => {
            ledger
                .lock()
                .map_err(|_| crate::InputError::Closed)?
                .release(&process);
            return Err(error);
        }
    };
    let result = run_worker(
        &mut pipe,
        &process,
        &ledger,
        &mut session,
        &stop,
        lease.generation,
        lease.token,
    );
    let cleanup = session.revoke();
    if !session.cleanup_required() {
        ledger
            .lock()
            .map_err(|_| crate::InputError::Closed)?
            .release(&process);
    }
    if session.cleanup_required() {
        cleanup_failed.store(true, Ordering::Release);
        stop.store(true, Ordering::Release);
        wake_listener();
        cleanup?;
        return Err(crate::InputError::Device);
    }
    result
}

fn run_worker(
    pipe: &mut Pipe,
    process: &ProcessIdentity,
    ledger: &Arc<Mutex<PermitLedger>>,
    session: &mut InputSession<VhfDeviceSink<WindowsVhfTransport>>,
    stop: &AtomicBool,
    generation: u64,
    token: String,
) -> InputResult<()> {
    write_reply(pipe, Reply::Granted { generation, token })?;
    while !stop.load(Ordering::Acquire) {
        if !ledger
            .lock()
            .map_err(|_| crate::InputError::Closed)?
            .is_current(process, Instant::now())
        {
            break;
        }
        session.refresh_host_authorized_expiry(Instant::now())?;
        if session.poll_expiry(Instant::now()).is_err() {
            break;
        }
        match read_request(pipe, Instant::now() + Duration::from_millis(50)) {
            Ok(Some(Request::WorkerEvent { packet })) => {
                if !ledger
                    .lock()
                    .map_err(|_| crate::InputError::Closed)?
                    .is_current(process, Instant::now())
                {
                    write_reply(pipe, Reply::Denied)?;
                    break;
                }
                // Re-authenticate the sender of every message, not just the
                // initial connection. A passed pipe handle cannot assert a
                // different Windows token or interactive session.
                if pipe_peer(pipe.raw())? != *process {
                    write_reply(pipe, Reply::Denied)?;
                    break;
                }
                let accepted = session.submit(&packet, Instant::now()).is_ok();
                write_reply(pipe, if accepted { Reply::Ok } else { Reply::Denied })?;
                if !accepted {
                    break;
                }
            }
            Ok(Some(Request::WorkerClose)) => {
                let _ = session.revoke();
                let removed = !session.cleanup_required();
                if removed {
                    ledger
                        .lock()
                        .map_err(|_| crate::InputError::Closed)?
                        .release(process);
                }
                write_reply(pipe, if removed { Reply::Ok } else { Reply::Denied })?;
                break;
            }
            Ok(Some(_)) | Err(_) => {
                let _ = write_reply(pipe, Reply::Denied);
                break;
            }
            Ok(None) => {}
        }
    }
    Ok(())
}

fn serve_connection(
    mut pipe: Pipe,
    ledger: Arc<Mutex<PermitLedger>>,
    stop: Arc<AtomicBool>,
    cleanup_failed: Arc<AtomicBool>,
    worker_pipe: bool,
) -> InputResult<()> {
    if worker_pipe {
        let opening = opening_process(pipe.raw())?;
        if !ledger
            .lock()
            .map_err(|_| crate::InputError::Closed)?
            .may_open(&opening, Instant::now())
        {
            return write_reply(&pipe, Reply::Denied);
        }
    }
    let request = read_request(&mut pipe, Instant::now() + Duration::from_secs(5))?
        .ok_or(crate::InputError::Closed)?;
    let caller = pipe_peer(pipe.raw())?;
    match request {
        Request::HostPermit { .. } => handle_host(&pipe, &caller, request, &ledger),
        Request::WorkerStart if caller.session != 0 => {
            handle_worker(pipe, caller, ledger, stop, cleanup_failed)
        }
        _ => write_reply(&pipe, Reply::Denied),
    }
}

fn finish_host_listener(
    result: InputResult<()>,
    stop: &AtomicBool,
    wake_worker: impl FnOnce(),
) -> InputResult<()> {
    if result.is_err() {
        stop.store(true, Ordering::Release);
        wake_worker();
    }
    result
}

pub fn run(state_root: &Path, stop: Arc<AtomicBool>, on_ready: impl FnOnce()) -> InputResult<()> {
    let self_process = inspect_process(std::process::id())?;
    if self_process.principal != SYSTEM_SID || self_process.session != 0 {
        return Err(crate::InputError::PermissionDenied);
    }
    let registry = GrantRegistry::open_protected(&state_root.join("input-companion"))?;
    let ledger = Arc::new(Mutex::new(PermitLedger::new(registry)));
    let connections = Arc::new(AtomicUsize::new(0));
    let cleanup_failed = Arc::new(AtomicBool::new(false));
    let host_pipe = Pipe::listen(PipeKind::Host, true)?;
    let worker_pipe = Pipe::listen(PipeKind::Worker, true)?;
    let host_ledger = Arc::clone(&ledger);
    let host_stop = Arc::clone(&stop);
    let host_cleanup_failed = Arc::clone(&cleanup_failed);
    let host_listener = std::thread::spawn(move || -> InputResult<()> {
        let result = (|| -> InputResult<()> {
            let mut pipe = host_pipe;
            while !host_stop.load(Ordering::Acquire) {
                if !pipe.accept()? {
                    pipe = Pipe::listen(PipeKind::Host, false)?;
                    continue;
                }
                let next = Pipe::listen(PipeKind::Host, false)?;
                if !host_stop.load(Ordering::Acquire) {
                    let _ = serve_connection(
                        pipe,
                        Arc::clone(&host_ledger),
                        Arc::clone(&host_stop),
                        Arc::clone(&host_cleanup_failed),
                        false,
                    );
                }
                pipe = next;
            }
            Ok(())
        })();
        finish_host_listener(result, &host_stop, || {
            let _ = Pipe::connect(PipeKind::Worker);
        })
    });
    on_ready();
    let mut workers: Vec<std::thread::JoinHandle<()>> = Vec::new();
    let listen_result = (|| -> InputResult<()> {
        let mut pipe = worker_pipe;
        while !stop.load(Ordering::Acquire) {
            if !pipe.accept()? {
                pipe = Pipe::listen(PipeKind::Worker, false)?;
                continue;
            }
            if stop.load(Ordering::Acquire) {
                break;
            }
            let next = Pipe::listen(PipeKind::Worker, false)?;
            let mut index = 0;
            while index < workers.len() {
                if workers[index].is_finished() {
                    workers
                        .swap_remove(index)
                        .join()
                        .map_err(|_| crate::InputError::Device)?;
                } else {
                    index += 1;
                }
            }
            if connections.fetch_add(1, Ordering::AcqRel) >= MAX_CONNECTIONS {
                connections.fetch_sub(1, Ordering::AcqRel);
                pipe = next;
                continue;
            }
            let ledger = Arc::clone(&ledger);
            let stop = Arc::clone(&stop);
            let connections = Arc::clone(&connections);
            let cleanup_failed = Arc::clone(&cleanup_failed);
            workers.push(std::thread::spawn(move || {
                let _ = serve_connection(pipe, ledger, stop, cleanup_failed, true);
                connections.fetch_sub(1, Ordering::AcqRel);
            }));
            pipe = next;
        }
        Ok(())
    })();
    stop.store(true, Ordering::Release);
    let _ = Pipe::connect(PipeKind::Host);
    for worker in workers {
        worker.join().map_err(|_| crate::InputError::Device)?;
    }
    host_listener
        .join()
        .map_err(|_| crate::InputError::Device)??;
    if cleanup_failed.load(Ordering::Acquire) {
        return Err(crate::InputError::Device);
    }
    listen_result
}

pub fn wake_listener() {
    let _ = Pipe::connect(PipeKind::Host);
    let _ = Pipe::connect(PipeKind::Worker);
}

pub fn authorize_worker(
    process_id: u32,
    creation_time: u64,
    digest: &str,
    keyboard: bool,
    mouse: bool,
) -> InputResult<()> {
    let mut pipe = Pipe::connect(PipeKind::Host)?;
    let bytes = serde_json::to_vec(&Request::HostPermit {
        process_id,
        creation_time,
        digest: digest.into(),
        keyboard,
        mouse,
    })
    .map_err(|_| crate::InputError::InvalidEvent)?;
    pipe.write_line(&bytes)?;
    let response = pipe
        .read_line(Instant::now() + Duration::from_secs(2))?
        .ok_or(crate::InputError::Device)?;
    match serde_json::from_slice::<Reply>(&response) {
        Ok(Reply::Ok) => Ok(()),
        _ => Err(crate::InputError::PermissionDenied),
    }
}

pub struct WindowsInputClient {
    pipe: Pipe,
    token: String,
    generation: u64,
    next_sequence: u64,
    closed: bool,
}

impl WindowsInputClient {
    pub fn connect() -> InputResult<Self> {
        let mut pipe = Pipe::connect(PipeKind::Worker)?;
        pipe.write_line(
            &serde_json::to_vec(&Request::WorkerStart)
                .map_err(|_| crate::InputError::InvalidEvent)?,
        )?;
        let reply = pipe
            .read_line(Instant::now() + Duration::from_secs(2))?
            .ok_or(crate::InputError::Device)?;
        let Reply::Granted { generation, token } =
            serde_json::from_slice(&reply).map_err(|_| crate::InputError::Device)?
        else {
            return Err(crate::InputError::PermissionDenied);
        };
        if generation == 0 || token.len() != 32 {
            return Err(crate::InputError::Device);
        }
        Ok(Self {
            pipe,
            token,
            generation,
            next_sequence: 1,
            closed: false,
        })
    }

    pub fn send(&mut self, event: InputEvent) -> InputResult<()> {
        if self.closed {
            return Err(crate::InputError::Closed);
        }
        let packet = InputPacket {
            version: crate::PROTOCOL_VERSION,
            generation: self.generation,
            sequence: self.next_sequence,
            lease_token: self.token.clone(),
            event,
        };
        self.next_sequence = self
            .next_sequence
            .checked_add(1)
            .ok_or(crate::InputError::InvalidSequence)?;
        let bytes = serde_json::to_vec(&Request::WorkerEvent { packet })
            .map_err(|_| crate::InputError::InvalidEvent)?;
        self.pipe.write_line(&bytes)?;
        let response = self
            .pipe
            .read_line(Instant::now() + Duration::from_secs(2))?
            .ok_or(crate::InputError::Device)?;
        match serde_json::from_slice::<Reply>(&response) {
            Ok(Reply::Ok) => Ok(()),
            _ => Err(crate::InputError::PermissionDenied),
        }
    }

    pub fn close(&mut self) -> InputResult<()> {
        if self.closed {
            return Ok(());
        }
        let bytes = serde_json::to_vec(&Request::WorkerClose)
            .map_err(|_| crate::InputError::InvalidEvent)?;
        self.pipe.write_line(&bytes)?;
        let response = self
            .pipe
            .read_line(Instant::now() + Duration::from_secs(2))?
            .ok_or(crate::InputError::Device)?;
        match serde_json::from_slice::<Reply>(&response) {
            Ok(Reply::Ok) => {
                self.closed = true;
                Ok(())
            }
            _ => Err(crate::InputError::Device),
        }
    }
}

impl Drop for WindowsInputClient {
    fn drop(&mut self) {
        // Closing the pipe is authoritative; the server revokes and destroys
        // the child independently even if this best-effort message is lost.
        if !self.closed
            && let Ok(bytes) = serde_json::to_vec(&Request::WorkerClose)
        {
            let _ = self.pipe.write_line(&bytes);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_host_listener_stops_and_wakes_worker_listener() {
        let stop = AtomicBool::new(false);
        let mut woke = false;
        assert_eq!(
            finish_host_listener(Err(crate::InputError::Device), &stop, || woke = true),
            Err(crate::InputError::Device)
        );
        assert!(stop.load(Ordering::Acquire));
        assert!(woke);
    }
}
