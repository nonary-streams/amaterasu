//! Records which module held the host thread, and for how long.
//!
//! Every module is ticked in turn on one thread, so a slow tick delays input
//! and video for all of them. The host is the only place that knows exactly
//! whose call it was; it times each one itself rather than inferring it.
use amaterasu_abi::{HostApi, TRACE_SPAN, TraceRecord, TraceSink};
use amaterasu_contracts::{Command, Response};
use std::time::{Duration, Instant};

/// Shorter module calls cannot explain a millisecond-scale stall. Their totals
/// are already reported by each module's own lifecycle timing.
const MODULE_SPAN_MINIMUM: Duration = Duration::from_micros(250);
/// A wait this long is a deliberate idle sleep, worth distinguishing from a
/// busy thread when a flow waited for the next pass.
const WAIT_SPAN_MINIMUM: Duration = Duration::from_micros(500);

#[derive(Clone, Copy)]
pub(crate) enum ModuleCall {
    Tick,
    Health,
}

#[derive(Default)]
pub(crate) struct HostTrace {
    sink: Option<TraceSink>,
    pass: u16,
    tick: u16,
    health: u16,
    wait: u16,
}

impl HostTrace {
    /// A runtime without tracing leaves the host silent; nothing else changes.
    pub fn bind(endpoint: &HostApi) -> Self {
        let Some(sink) = bind_sink(endpoint) else {
            return Self::default();
        };
        Self {
            pass: sink.intern("host.pass"),
            tick: sink.intern("host.tick"),
            health: sink.intern("host.health"),
            wait: sink.intern("host.wait"),
            sink: Some(sink),
        }
    }

    /// Names a module once at load so the per-call path passes an integer.
    pub fn label(&self, module: &str) -> u16 {
        self.sink.map_or(0, |sink| sink.intern(module))
    }

    fn span(&self, kind: u16, label: u16, duration: Duration) {
        let Some(sink) = self.sink else {
            return;
        };
        if kind == 0 {
            return;
        }
        sink.record(&TraceRecord {
            value: u64::try_from(duration.as_micros()).unwrap_or(u64::MAX),
            track: kind,
            stage: label,
            kind: TRACE_SPAN,
            ..Default::default()
        });
    }

    pub fn module_call(&self, call: ModuleCall, label: u16, duration: Duration) {
        if duration >= MODULE_SPAN_MINIMUM {
            let kind = match call {
                ModuleCall::Tick => self.tick,
                ModuleCall::Health => self.health,
            };
            self.span(kind, label, duration);
        }
    }

    /// One complete round of module ticks.
    pub fn pass(&self, started: Instant) {
        self.span(self.pass, 0, started.elapsed());
    }

    pub fn wait(&self, started: Instant) {
        let waited = started.elapsed();
        if waited >= WAIT_SPAN_MINIMUM {
            self.span(self.wait, 0, waited);
        }
    }
}

fn bind_sink(endpoint: &HostApi) -> Option<TraceSink> {
    let request = serde_json::to_vec(&Command::TraceBind {}).ok()?;
    // SAFETY: the endpoint belongs to the live runtime owned by the caller.
    let reply = unsafe { endpoint.call(&request) }.ok()?;
    let Ok(Response::TraceBound { address }) = serde_json::from_slice(&reply) else {
        return None;
    };
    // SAFETY: the loaded runtime returned this address for its own table and
    // is destroyed only after every module and this host's last tick.
    unsafe { TraceSink::from_address(address) }
}
