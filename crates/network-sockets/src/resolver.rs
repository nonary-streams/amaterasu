//! OS name-service lookup runs in an independently executable helper. A timeout
//! never leaves a thread executing provider code after its native library unloads.

#[allow(dead_code)]
mod helperprotocol;
mod process;

use helperprotocol::{MAX_RESPONSE_BYTES, Request, Response};
use process::{HelperExecutable, ResolverChild};
use std::{
    io,
    net::{IpAddr, SocketAddr},
    path::Path,
    sync::{
        Arc, Mutex, TryLockError,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

const MAX_WORKERS: usize = 8;
const POLL_INTERVAL: Duration = Duration::from_millis(1);
const STANDALONE_DROP_BUDGET: Duration = Duration::from_millis(50);
const _: () = assert!(helperprotocol::MAX_ADDRESSES == amaterasu_network_api::MAX_ADDRESSES);

pub(super) struct Resolver {
    state: Mutex<ResolverState>,
    active_calls: AtomicUsize,
    clock_origin: Instant,
    shutdown_after_ns: AtomicU64,
    quiescing: AtomicBool,
}

struct ResolverState {
    executable: Option<HelperExecutable>,
    lookups: Vec<Lookup>,
    next_lookup: u64,
}

struct Lookup {
    id: u64,
    child: ResolverChild,
    response: Vec<u8>,
    stdout_closed: bool,
    cancelled: Arc<AtomicBool>,
    kill_requested: bool,
}

struct ActiveCall<'a>(&'a AtomicUsize);

impl Drop for ActiveCall<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

impl Resolver {
    pub fn new() -> Self {
        Self::with_executable(None)
    }

    pub fn with_runtime_state(root: &Path) -> io::Result<Self> {
        Ok(Self::with_executable(Some(HelperExecutable::new(Some(
            root,
        ))?)))
    }

    fn with_executable(executable: Option<HelperExecutable>) -> Self {
        Self {
            state: Mutex::new(ResolverState {
                executable,
                lookups: Vec::new(),
                next_lookup: 1,
            }),
            active_calls: AtomicUsize::new(0),
            clock_origin: Instant::now(),
            shutdown_after_ns: AtomicU64::new(u64::MAX),
            quiescing: AtomicBool::new(false),
        }
    }

    #[cfg(test)]
    fn state(&self) -> io::Result<std::sync::MutexGuard<'_, ResolverState>> {
        self.state
            .lock()
            .map_err(|_| io::Error::other("resolver unavailable"))
    }

    fn try_state(&self) -> io::Result<std::sync::MutexGuard<'_, ResolverState>> {
        self.state.try_lock().map_err(|error| match error {
            TryLockError::WouldBlock => io::ErrorKind::WouldBlock.into(),
            TryLockError::Poisoned(_) => io::Error::other("resolver unavailable"),
        })
    }

    pub fn prepare_shutdown(&self, remaining_ms: u64) -> io::Result<()> {
        let now = u64::try_from(self.clock_origin.elapsed().as_nanos()).unwrap_or(u64::MAX - 1);
        let deadline = remaining_ms
            .checked_mul(1_000_000)
            .and_then(|remaining| now.checked_add(remaining))
            .filter(|deadline| *deadline < u64::MAX)
            .unwrap_or(now);
        self.shutdown_after_ns.fetch_min(deadline, Ordering::AcqRel);
        Ok(())
    }

    pub fn poll(&self) -> io::Result<()> {
        match self.try_state() {
            Ok(mut state) => {
                state.reap_cancelled();
                Ok(())
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(()),
            Err(error) => Err(error),
        }
    }

    pub fn quiesce(&self) -> io::Result<bool> {
        self.quiescing.store(true, Ordering::Release);
        let mut state = match self.try_state() {
            Ok(state) => state,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(false),
            Err(error) => return Err(error),
        };
        for lookup in &mut state.lookups {
            lookup.cancel();
        }
        state.reap_cancelled();
        Ok(state.lookups.is_empty() && self.active_calls.load(Ordering::Acquire) == 0)
    }

    pub fn resolve(&self, host: &str, port: u16, timeout_ms: u32) -> io::Result<Vec<SocketAddr>> {
        if timeout_ms == 0 {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        self.active_calls.fetch_add(1, Ordering::AcqRel);
        let _active = ActiveCall(&self.active_calls);
        let request_deadline = Instant::now() + Duration::from_millis(u64::from(timeout_ms));
        self.admit(request_deadline)?;
        let mut state = self.try_state()?;
        state.reap_cancelled();
        self.admit(request_deadline)?;
        if let Ok(ip) = host.parse::<IpAddr>() {
            return Ok(vec![SocketAddr::new(ip, port)]);
        }
        let request = Request {
            host: host.into(),
            port,
        }
        .encode()?;
        if state.lookups.len() >= MAX_WORKERS {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        if state.executable.is_none() {
            state.executable = Some(HelperExecutable::new(None)?);
        }
        let id = state.next_lookup;
        state.next_lookup = id
            .checked_add(1)
            .ok_or_else(|| io::Error::other("resolver identity exhausted"))?;
        let child = state.executable.as_ref().unwrap().spawn(&request)?;
        let cancelled = Arc::new(AtomicBool::new(false));
        state.lookups.push(Lookup {
            id,
            child,
            response: Vec::new(),
            stdout_closed: false,
            cancelled: cancelled.clone(),
            kill_requested: false,
        });
        drop(state);

        loop {
            if let Err(error) = self.admit(request_deadline) {
                cancelled.store(true, Ordering::Release);
                let _ = self.poll();
                return Err(error);
            }
            let mut state = match self.try_state() {
                Ok(state) => state,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    thread::sleep(
                        POLL_INTERVAL.min(
                            self.deadline(request_deadline)
                                .saturating_duration_since(Instant::now()),
                        ),
                    );
                    continue;
                }
                Err(error) => {
                    cancelled.store(true, Ordering::Release);
                    return Err(error);
                }
            };
            let Some(index) = state.lookups.iter().position(|lookup| lookup.id == id) else {
                return Err(io::ErrorKind::Interrupted.into());
            };
            if let Err(error) = self.admit(request_deadline) {
                state.lookups[index].cancel();
                state.reap_cancelled();
                return Err(error);
            }
            match state.lookups[index].poll_response() {
                Ok(Some(response)) => {
                    state.lookups.swap_remove(index);
                    return response;
                }
                Ok(None) => {}
                Err(error) => {
                    state.lookups[index].cancel();
                    state.reap_cancelled();
                    return Err(error);
                }
            }
            let deadline = self.deadline(request_deadline);
            drop(state);
            thread::sleep(POLL_INTERVAL.min(deadline.saturating_duration_since(Instant::now())));
        }
    }
}

impl Resolver {
    fn deadline(&self, request_deadline: Instant) -> Instant {
        let shutdown = self.shutdown_after_ns.load(Ordering::Acquire);
        if shutdown == u64::MAX {
            return request_deadline;
        }
        self.clock_origin
            .checked_add(Duration::from_nanos(shutdown))
            .unwrap_or(self.clock_origin)
            .min(request_deadline)
    }

    fn admit(&self, request_deadline: Instant) -> io::Result<()> {
        if self.quiescing.load(Ordering::Acquire) {
            return Err(io::ErrorKind::Interrupted.into());
        }
        if Instant::now() >= self.deadline(request_deadline) {
            return Err(io::ErrorKind::TimedOut.into());
        }
        Ok(())
    }
}

impl ResolverState {
    fn reap_cancelled(&mut self) {
        let mut index = 0;
        while index < self.lookups.len() {
            let lookup = &mut self.lookups[index];
            if lookup.cancelled.load(Ordering::Acquire) {
                lookup.cancel();
                if matches!(lookup.child.try_wait(), Ok(Some(_))) {
                    self.lookups.swap_remove(index);
                    continue;
                }
            }
            index += 1;
        }
    }
}

impl Lookup {
    fn cancel(&mut self) {
        self.cancelled.store(true, Ordering::Release);
        if !self.kill_requested {
            self.kill_requested = self.child.kill().is_ok();
        }
    }

    fn poll_response(&mut self) -> io::Result<Option<io::Result<Vec<SocketAddr>>>> {
        if self.cancelled.load(Ordering::Acquire) {
            return Err(io::ErrorKind::Interrupted.into());
        }
        let mut buffer = [0; 4096];
        while !self.stdout_closed {
            match self.child.try_read(&mut buffer) {
                Ok(0) => self.stdout_closed = true,
                Ok(length) => {
                    if self.response.len() + length > MAX_RESPONSE_BYTES {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "resolver response exceeds its bound",
                        ));
                    }
                    self.response.extend_from_slice(&buffer[..length]);
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => break,
                Err(error) => return Err(error),
            }
        }
        let Some(status) = self.child.try_wait()? else {
            return Ok(None);
        };
        if !self.stdout_closed {
            return Ok(None);
        }
        if !status.success() {
            return Ok(Some(Err(io::Error::other("resolver helper failed"))));
        }
        Ok(Some(match Response::decode(&self.response)? {
            Response::Addresses(addresses) if !addresses.is_empty() => Ok(addresses),
            Response::Addresses(_) => Err(io::ErrorKind::NotFound.into()),
            Response::Error(error) => Err(error.into_io()),
        }))
    }
}

impl Drop for Resolver {
    fn drop(&mut self) {
        self.quiescing.store(true, Ordering::Release);
        let deadline = self.deadline(Instant::now() + STANDALONE_DROP_BUDGET);
        let state = self
            .state
            .get_mut()
            .unwrap_or_else(|error| error.into_inner());
        for lookup in &mut state.lookups {
            lookup.cancel();
        }
        // Production destruction reuses the host's frozen deadline. Standalone
        // callers get only a small reap tail; no native-library thread survives.
        loop {
            state.reap_cancelled();
            if state.lookups.is_empty() || Instant::now() >= deadline {
                break;
            }
            thread::sleep(POLL_INTERVAL.min(deadline.saturating_duration_since(Instant::now())));
        }
    }
}

#[cfg(test)]
mod tests;
