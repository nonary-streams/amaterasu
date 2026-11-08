//! Process-lifetime ownership for detached children.
//!
//! This is an internal provider implementation, not an ABI. It must be linked
//! into the host executable, whose code remains mapped while its worker runs.
//! A versioned provider contract is responsible for authorizing and translating
//! requests; neither `Command` nor `Child` crosses that contract.
//!
//! Dropping an observation never signals its child. The worker retains each
//! child until an exact wait succeeds, even after the final registry handle is
//! dropped. Do not independently wait on children admitted to this registry.

use std::collections::BTreeMap;
use std::io;
use std::process::{Child, Command, ExitStatus};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread;
use std::time::Duration;

mod output;
pub use output::Sink;

const POLL_INTERVAL: Duration = Duration::from_millis(50);
const MAX_CAPACITY: usize = 65_536;

struct State {
    entries: BTreeMap<u64, Entry>,
    next_token: u64,
    sinks: BTreeMap<u64, Option<std::fs::File>>,
    next_sink: u64,
    owners: usize,
}

enum Entry {
    Reserved,
    Spawned {
        child: Option<Child>,
        exit: Option<ExitStatus>,
        wait_error: Option<(io::ErrorKind, String)>,
        observed: bool,
    },
}

struct Shared {
    capacity: usize,
    sink_capacity: usize,
    state: Mutex<State>,
    wake: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }
}

/// Bounded admission to a reaper linked into the host process.
pub struct Registry {
    shared: Arc<Shared>,
}

impl Registry {
    /// Starts one host-owned worker. The capacity includes completed results
    /// still held by observers, in-flight spawns, and detached live children.
    pub fn new(capacity: usize) -> io::Result<Self> {
        Self::with_limits(capacity, capacity)
    }

    /// Bounds process and output leases independently. An in-flight open or
    /// spawn reserves its respective slot before any filesystem or OS effect.
    pub fn with_limits(capacity: usize, sink_capacity: usize) -> io::Result<Self> {
        if !(1..=MAX_CAPACITY).contains(&capacity) || !(1..=MAX_CAPACITY).contains(&sink_capacity) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid capacity",
            ));
        }
        let shared = Arc::new(Shared {
            capacity,
            sink_capacity,
            state: Mutex::new(State {
                entries: BTreeMap::new(),
                next_token: 1,
                sinks: BTreeMap::new(),
                next_sink: 1,
                owners: 1,
            }),
            wake: Condvar::new(),
        });
        let worker = Arc::clone(&shared);
        thread::Builder::new()
            .name("amaterasu-detached-reaper".into())
            .spawn(move || run(worker))?;
        Ok(Self { shared })
    }

    /// Reserves capacity before spawning. If admission or spawning fails, no
    /// child was created and no reservation remains. The caller must not wait
    /// on the returned child's diagnostic PID by any other means.
    pub fn spawn(&self, command: &mut Command) -> io::Result<Process> {
        let token = {
            let mut state = self.shared.lock();
            if state.entries.len() >= self.shared.capacity {
                return Err(io::Error::other("detached process capacity exhausted"));
            }
            let token = state.next_token;
            state.next_token = token
                .checked_add(1)
                .ok_or_else(|| io::Error::other("detached process tokens exhausted"))?;
            state.entries.insert(token, Entry::Reserved);
            token
        };
        match command.spawn() {
            Ok(child) => {
                let pid = child.id();
                let mut state = self.shared.lock();
                state.entries.insert(
                    token,
                    Entry::Spawned {
                        child: Some(child),
                        exit: None,
                        wait_error: None,
                        observed: true,
                    },
                );
                state.owners += 1;
                drop(state);
                self.shared.wake.notify_one();
                Ok(Process {
                    shared: Arc::clone(&self.shared),
                    token,
                    pid,
                })
            }
            Err(error) => {
                self.shared.lock().entries.remove(&token);
                self.shared.wake.notify_one();
                Err(error)
            }
        }
    }
}

impl Clone for Registry {
    fn clone(&self) -> Self {
        self.shared.lock().owners += 1;
        Self {
            shared: Arc::clone(&self.shared),
        }
    }
}

impl Drop for Registry {
    fn drop(&mut self) {
        self.shared.lock().owners -= 1;
        self.shared.wake.notify_one();
    }
}

/// An opaque observation of a process. Dropping it releases the result, not
/// the process, and the host worker still waits for the child to exit.
pub struct Process {
    shared: Arc<Shared>,
    token: u64,
    pid: u32,
}

impl Process {
    /// The PID is diagnostic only and must never be used as an ownership token.
    pub fn id(&self) -> u32 {
        self.pid
    }

    /// Returns a retained terminal result or the latest wait error. Wait errors
    /// do not release ownership; the worker continues bounded retries.
    pub fn status(&self) -> io::Result<Option<ExitStatus>> {
        let state = self.shared.lock();
        match state.entries.get(&self.token) {
            Some(Entry::Spawned {
                exit, wait_error, ..
            }) => {
                if let Some((kind, message)) = wait_error {
                    Err(io::Error::new(*kind, message.clone()))
                } else {
                    Ok(*exit)
                }
            }
            _ => Err(io::Error::other("detached process observation unavailable")),
        }
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        let mut state = self.shared.lock();
        if let Some(Entry::Spawned {
            child, observed, ..
        }) = state.entries.get_mut(&self.token)
        {
            *observed = false;
            if child.is_none() {
                state.entries.remove(&self.token);
            }
        }
        state.owners -= 1;
        self.shared.wake.notify_one();
    }
}

fn run(shared: Arc<Shared>) {
    loop {
        let mut state = shared.lock();
        state
            .entries
            .retain(|_, entry| poll_entry(entry, Child::try_wait));
        if state.entries.is_empty() && state.owners == 0 {
            break;
        }
        if state.entries.is_empty() {
            drop(
                shared
                    .wake
                    .wait(state)
                    .unwrap_or_else(|error| error.into_inner()),
            );
        } else {
            let _guard = shared
                .wake
                .wait_timeout(state, POLL_INTERVAL)
                .unwrap_or_else(|error| error.into_inner());
        }
    }
}

fn poll_entry(
    entry: &mut Entry,
    mut wait: impl FnMut(&mut Child) -> io::Result<Option<ExitStatus>>,
) -> bool {
    let Entry::Spawned {
        child,
        exit,
        wait_error,
        observed,
    } = entry
    else {
        return true;
    };
    if let Some(process) = child {
        match wait(process) {
            Ok(Some(status)) => {
                *child = None;
                *exit = Some(status);
                *wait_error = None;
            }
            Ok(None) => *wait_error = None,
            Err(error) => *wait_error = Some((error.kind(), error.to_string())),
        }
    }
    *observed || child.is_some()
}

#[cfg(test)]
mod tests;
