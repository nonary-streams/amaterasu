//! Installed companion's single named-pipe authorization and VHF owner.
//! This module is not selected by the ordinary Amaterasu worker by default.
use crate::windows_identity::{inspect_process, opening_process, pipe_peer, service_process_id};
use crate::windows_pipe::{Pipe, PipeKind};
use crate::worker_client::{REPLY_TIMEOUT, WorkerClient, WorkerTransport};
use crate::worker_server::{
    IO_POLL_INTERVAL, Reply, Request, WorkerConnection, WorkerContext, decode_request,
    encode_reply, serve_worker,
};
use crate::{
    GrantRegistry, InputEvent, InputResult, InputSession, PermitLedger, ProcessIdentity,
    VhfDeviceSink, WindowsVhfTransport,
};
use std::path::Path;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};

const MAX_CONNECTIONS: usize = 8;
const SYSTEM_SID: &str = "S-1-5-18";

fn write_reply(pipe: &Pipe, reply: Reply, stop: &AtomicBool) -> InputResult<()> {
    let bytes = encode_reply(&reply)?;
    pipe.write_line_until(&bytes, Instant::now() + IO_POLL_INTERVAL, stop)
}

fn read_request(
    pipe: &mut Pipe,
    deadline: Instant,
    stop: &AtomicBool,
) -> InputResult<Option<Request>> {
    let Some(bytes) = pipe.read_line_until(deadline, stop)? else {
        return Ok(None);
    };
    decode_request(&bytes).map(Some)
}

fn handle_host(
    pipe: &Pipe,
    caller: &ProcessIdentity,
    request: Request,
    ledger: &Arc<Mutex<PermitLedger>>,
    stop: &AtomicBool,
) -> InputResult<()> {
    if stop.load(Ordering::Acquire) {
        return Err(crate::InputError::Closed);
    }
    if caller.principal != SYSTEM_SID || caller.process_id != service_process_id("Amaterasu")? {
        return write_reply(pipe, Reply::Denied, stop);
    }
    let Request::HostPermit {
        process_id,
        creation_time,
        digest,
        keyboard,
        mouse,
    } = request
    else {
        return write_reply(pipe, Reply::Denied, stop);
    };
    let process = inspect_process(process_id)?;
    if !process.matches_instance(process_id, creation_time)
        || process.session == 0
        || process.principal == SYSTEM_SID
    {
        return write_reply(pipe, Reply::Denied, stop);
    }
    let granted = ledger
        .lock()
        .map_err(|_| crate::InputError::Closed)?
        .refresh_from_host(process, digest, keyboard, mouse, Instant::now())
        .is_ok();
    write_reply(pipe, if granted { Reply::Ok } else { Reply::Denied }, stop)
}

fn handle_worker(
    mut pipe: Pipe,
    process: ProcessIdentity,
    ledger: Arc<Mutex<PermitLedger>>,
    stop: Arc<AtomicBool>,
    cleanup_failed: Arc<AtomicBool>,
) -> InputResult<()> {
    if stop.load(Ordering::Acquire) {
        return Err(crate::InputError::Closed);
    }
    let lease = ledger
        .lock()
        .map_err(|_| crate::InputError::Closed)?
        .claim(&process, Instant::now())?;
    if stop.load(Ordering::Acquire) {
        ledger
            .lock()
            .map_err(|_| crate::InputError::Closed)?
            .release_lease(&process, lease.generation, &lease.token);
        return Err(crate::InputError::Closed);
    }
    let sink = WindowsVhfTransport::open().and_then(VhfDeviceSink::create);
    let sink = match sink {
        Ok(sink) => sink,
        Err(error) => {
            ledger
                .lock()
                .map_err(|_| crate::InputError::Closed)?
                .release_lease(&process, lease.generation, &lease.token);
            return Err(error);
        }
    };
    let session = match InputSession::connect(&lease.peer, lease.grant, sink, Instant::now()) {
        Ok(session) => session,
        Err(error) => {
            ledger
                .lock()
                .map_err(|_| crate::InputError::Closed)?
                .release_lease(&process, lease.generation, &lease.token);
            return Err(error);
        }
    };
    let mut connection = WorkerPipe { pipe: &mut pipe };
    serve_worker(
        &mut connection,
        session,
        WorkerContext {
            process: &process,
            ledger: &ledger,
            stop: &stop,
            cleanup_failed: &cleanup_failed,
            generation: lease.generation,
            token: &lease.token,
        },
        wake_listener,
    )
}

struct WorkerPipe<'a> {
    pipe: &'a mut Pipe,
}

impl WorkerConnection for WorkerPipe<'_> {
    fn authenticated_peer(&self) -> InputResult<ProcessIdentity> {
        pipe_peer(self.pipe.raw())
    }

    fn read_frame(&mut self, deadline: Instant, stop: &AtomicBool) -> InputResult<Option<Vec<u8>>> {
        self.pipe.read_line_until(deadline, stop)
    }

    fn write_frame(
        &mut self,
        frame: &[u8],
        deadline: Instant,
        stop: &AtomicBool,
    ) -> InputResult<()> {
        self.pipe.write_line_until(frame, deadline, stop)
    }
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
            return write_reply(&pipe, Reply::Denied, &stop);
        }
    }
    let request = read_request(&mut pipe, Instant::now() + Duration::from_secs(5), &stop)?
        .ok_or(crate::InputError::Closed)?;
    let caller = pipe_peer(pipe.raw())?;
    if stop.load(Ordering::Acquire) {
        return Err(crate::InputError::Closed);
    }
    match request {
        Request::HostPermit { .. } => handle_host(&pipe, &caller, request, &ledger, &stop),
        Request::WorkerStart if caller.session != 0 => {
            handle_worker(pipe, caller, ledger, stop, cleanup_failed)
        }
        _ => write_reply(&pipe, Reply::Denied, &stop),
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
                match pipe.accept_until(Instant::now() + IO_POLL_INTERVAL, &host_stop)? {
                    None => continue,
                    Some(false) => {
                        pipe = Pipe::listen(PipeKind::Host, false)?;
                        continue;
                    }
                    Some(true) => {}
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
            match pipe.accept_until(Instant::now() + IO_POLL_INTERVAL, &stop)? {
                None => continue,
                Some(false) => {
                    pipe = Pipe::listen(PipeKind::Worker, false)?;
                    continue;
                }
                Some(true) => {}
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
    connection: WorkerClient<Pipe>,
}

impl WindowsInputClient {
    pub fn connect() -> InputResult<Self> {
        Ok(Self {
            connection: WorkerClient::connect(Pipe::connect(PipeKind::Worker)?, REPLY_TIMEOUT)?,
        })
    }

    pub fn connect_with_deadline(deadline: Instant) -> InputResult<Self> {
        if Instant::now() >= deadline {
            return Err(crate::InputError::Device);
        }
        Ok(Self {
            connection: WorkerClient::connect_with_deadline(
                Pipe::connect(PipeKind::Worker)?,
                REPLY_TIMEOUT,
                Some(deadline),
            )?,
        })
    }

    pub fn restrict_deadline(&mut self, deadline: Instant) {
        self.connection.restrict_deadline(deadline);
    }

    pub fn send(&mut self, event: InputEvent) -> InputResult<()> {
        self.connection.send(event)
    }

    pub fn close(&mut self) -> InputResult<()> {
        self.connection.close()
    }
}

impl WorkerTransport for Pipe {
    fn write_frame(&mut self, frame: &[u8], deadline: Instant) -> InputResult<()> {
        if Instant::now() >= deadline {
            return Err(crate::InputError::Device);
        }
        self.write_line_before(frame, deadline)
    }

    fn read_frame(&mut self, deadline: Instant) -> InputResult<Option<Vec<u8>>> {
        self.read_line(deadline)
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
