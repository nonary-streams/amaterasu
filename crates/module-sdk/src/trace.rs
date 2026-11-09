//! Flow tracing: record where an input event or a frame spends its time.
//!
//! A *flow* is one traced item, identified by a number both sides of a module
//! boundary can derive from data they already exchange (see [`flow_id`]). Each
//! module marks the stages it owns; the runtime joins the marks, keeps
//! statistics for every step and reports flows that exceed their budget,
//! together with the thread spans that overlapped the slowest step.
//!
//! A *span* is a piece of work that occupied one thread, such as a module tick
//! or an encoder submission. Spans explain why a step was slow.
//!
//! Recording costs one clock read and one lock-free queue push on the calling
//! thread. Nothing is formatted, allocated or logged there. A tracer without a
//! sink (an older runtime, or [`Tracer::disabled`]) is a single branch.
//!
//! ```ignore
//! struct InputTrace { tracer: Tracer, track: Track, dequeued: Stage, accepted: Stage }
//!
//! let tracer = Tracer::bind(&runtime);
//! let trace = InputTrace {
//!     track: tracer.track(names::INPUT_TRACK),
//!     dequeued: tracer.stage("session.dequeued"),
//!     accepted: tracer.stage("input.accepted"),
//!     tracer,
//! };
//! let flow = flow_id(control_stream, message_sequence);
//! trace.tracer.mark(trace.track, flow, trace.dequeued);
//! // ... later, when the provider acknowledged the event ...
//! trace.tracer.finish(trace.track, flow, trace.accepted, Duration::from_millis(8));
//! ```
use crate::abi::{TRACE_END, TRACE_MARK, TRACE_SPAN, TraceRecord, TraceSink};
use crate::{Command, Response, RuntimeClient};
use std::time::{Duration, Instant};

/// Names shared by independently built modules. A provider that replaces a
/// built-in one joins the same flows by using the same names.
pub mod names {
    /// One client control message carrying input, from socket to injection.
    pub const INPUT_TRACK: &str = "input";
    /// One captured frame, from capture to transport admission.
    pub const VIDEO_TRACK: &str = "video";

    /// The datagram reached the host's kernel receive queue.
    pub const SOCKET_ARRIVED: &str = "socket.arrived";
    /// The control transport authenticated the message and queued it.
    pub const CONTROL_AUTHENTICATED: &str = "control.authenticated";
    /// The session read the message from the control transport.
    pub const SESSION_DEQUEUED: &str = "session.dequeued";
    /// The session handed the event's batch to the input provider.
    pub const SESSION_SUBMITTED: &str = "session.submitted";
    /// The input provider accepted (injected or suppressed) the event.
    pub const INPUT_ACCEPTED: &str = "input.accepted";
    /// The session consumed the message without producing input.
    pub const SESSION_CONSUMED: &str = "session.consumed";
    /// A newer controller sample replaced this one before it was sent.
    pub const SESSION_COALESCED: &str = "session.coalesced";

    /// The video pipeline started capturing this frame.
    pub const VIDEO_CAPTURE_STARTED: &str = "video.capture_started";
    /// The frame was handed to the encoder.
    pub const VIDEO_ENCODER_SUBMITTED: &str = "video.encoder_submitted";
    /// The encoded frame is queued for the session.
    pub const VIDEO_PACKET_QUEUED: &str = "video.packet_queued";
    /// The session read the encoded frame.
    pub const SESSION_PACKET_READ: &str = "session.packet_read";
    /// The transport packetized the frame and accepted it for sending.
    pub const TRANSPORT_ADMITTED: &str = "transport.admitted";
}

macro_rules! trace_name {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
        pub struct $name(u16);
        impl $name {
            /// False when the sink could not name it; recording is then skipped.
            pub fn is_named(self) -> bool {
                self.0 != 0
            }
        }
    };
}
trace_name!(
    /// A family of flows, such as [`names::INPUT_TRACK`].
    Track
);
trace_name!(
    /// A point a flow reaches.
    Stage
);
trace_name!(
    /// A kind of thread-occupying work, such as `host.tick`.
    SpanKind
);
trace_name!(
    /// What a span worked on, such as a module identity. Optional.
    Label
);

/// A time on the sink's clock. Only meaningful to the tracer that produced it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct TraceTime(u64);

impl TraceTime {
    /// The time `age` before this one, for an event learned about after the
    /// fact, such as a datagram's kernel arrival.
    pub fn earlier_by(self, age: Duration) -> Self {
        let age_us = u64::try_from(age.as_micros()).unwrap_or(u64::MAX);
        // Zero means "stamp on arrival", so an explicit time never becomes it.
        Self(self.0.saturating_sub(age_us).max(1))
    }
}

/// The same flow identity on both sides of a boundary: `scope` is an
/// identifier both modules know (a stream or session handle), `sequence` the
/// item's number within it.
pub fn flow_id(scope: u64, sequence: u64) -> u64 {
    scope ^ sequence.wrapping_mul(0x9E37_79B9_7F4A_7C15)
}

/// A module's handle to the runtime's trace sink. Cheap to clone and usable
/// from any of the module's threads until the module is destroyed.
#[derive(Clone, Copy)]
pub struct Tracer {
    sink: Option<TraceSink>,
}

impl Tracer {
    /// Binds this module's sink. A runtime without tracing yields a disabled
    /// tracer; modules never need a fallback path.
    pub fn bind(runtime: &RuntimeClient) -> Self {
        let sink = match runtime.send(Command::TraceBind {}) {
            // SAFETY: this module's runtime returned the address, and the
            // module contract keeps that runtime alive past every use.
            Ok(Response::TraceBound { address }) => unsafe { TraceSink::from_address(address) },
            _ => None,
        };
        Self { sink }
    }

    pub const fn disabled() -> Self {
        Self { sink: None }
    }

    /// Wraps a sink obtained another way, such as a test implementation.
    pub fn with_sink(sink: TraceSink) -> Self {
        Self { sink: Some(sink) }
    }

    pub fn is_enabled(&self) -> bool {
        self.sink.is_some()
    }

    fn intern(&self, name: &str) -> u16 {
        self.sink.map_or(0, |sink| sink.intern(name))
    }

    /// Cold path: name things once at start and keep the handles.
    pub fn track(&self, name: &str) -> Track {
        Track(self.intern(name))
    }
    pub fn stage(&self, name: &str) -> Stage {
        Stage(self.intern(name))
    }
    pub fn span_kind(&self, name: &str) -> SpanKind {
        SpanKind(self.intern(name))
    }
    pub fn label(&self, name: &str) -> Label {
        Label(self.intern(name))
    }

    /// The current time on the sink's clock, for [`Tracer::mark_at`].
    pub fn now(&self) -> TraceTime {
        TraceTime(self.sink.map_or(1, |sink| sink.now().max(1)))
    }

    fn record(&self, kind: u8, track: u16, stage: u16, flow: u64, value: u64, timestamp_us: u64) {
        let Some(sink) = self.sink else {
            return;
        };
        if track == 0 {
            return;
        }
        sink.record(&TraceRecord {
            flow,
            value,
            timestamp_us,
            track,
            stage,
            kind,
            reserved: [0; 3],
        });
    }

    /// The flow reached `stage` now. A flow's first mark starts it.
    pub fn mark(&self, track: Track, flow: u64, stage: Stage) {
        self.record(TRACE_MARK, track.0, stage.0, flow, 0, 0);
    }

    /// As [`Tracer::mark`], with a number worth seeing next to a slow step,
    /// such as a batch size or queue depth. Never a user's data.
    pub fn mark_with(&self, track: Track, flow: u64, stage: Stage, detail: u64) {
        self.record(TRACE_MARK, track.0, stage.0, flow, detail, 0);
    }

    /// The flow reached `stage` at an earlier time.
    pub fn mark_at(&self, track: Track, flow: u64, stage: Stage, at: TraceTime) {
        self.record(TRACE_MARK, track.0, stage.0, flow, 0, at.0);
    }

    /// The flow reached its last stage. It is reported individually when its
    /// first-to-last time is at least `budget`; a zero budget selects the
    /// sink's default.
    pub fn finish(&self, track: Track, flow: u64, stage: Stage, budget: Duration) {
        let budget_us = u64::try_from(budget.as_micros()).unwrap_or(u64::MAX);
        self.record(TRACE_END, track.0, stage.0, flow, budget_us, 0);
    }

    /// Work of `kind` that just ended took `duration` on this thread.
    pub fn record_span(&self, kind: SpanKind, label: Label, duration: Duration) {
        let duration_us = u64::try_from(duration.as_micros()).unwrap_or(u64::MAX);
        self.record(TRACE_SPAN, kind.0, label.0, 0, duration_us, 0);
    }

    /// Measures a span until the guard drops.
    pub fn span(&self, kind: SpanKind, label: Label) -> ActiveSpan<'_> {
        ActiveSpan {
            tracer: self,
            kind,
            label,
            // A disabled tracer does not read the clock.
            started: (self.sink.is_some() && kind.is_named()).then(Instant::now),
        }
    }
}

/// Records its span when dropped, including during unwinding.
#[must_use = "a span measures until it is dropped"]
pub struct ActiveSpan<'a> {
    tracer: &'a Tracer,
    kind: SpanKind,
    label: Label,
    started: Option<Instant>,
}

impl Drop for ActiveSpan<'_> {
    fn drop(&mut self) {
        if let Some(started) = self.started {
            self.tracer
                .record_span(self.kind, self.label, started.elapsed());
        }
    }
}

#[cfg(test)]
mod tests;
