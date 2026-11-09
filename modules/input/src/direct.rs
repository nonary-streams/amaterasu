//! Synchronous delivery of an input batch on the calling session's thread.
//!
//! A queued batch waits for this module's next tick, and its reply for the
//! caller's. A direct batch is injected before the caller's call returns.
//! Only batches that already complete within one dispatch are served here.
//! Everything else is declined before any effect, so the queued path handles
//! it with its jobs, deadlines and cleanup rules exactly as before.
use super::*;
use amaterasu_module_sdk::DirectCall;
use amaterasu_module_sdk::trace::{Label, SpanKind, Tracer};

/// Names the time this module spends delivering batches, by transport.
pub(super) struct BatchTrace {
    tracer: Tracer,
    batch: SpanKind,
    direct: Label,
    queued: Label,
}

impl BatchTrace {
    pub(super) fn bind(runtime: &RuntimeClient) -> Self {
        let tracer = Tracer::bind(runtime);
        Self {
            batch: tracer.span_kind("input.batch"),
            direct: tracer.label("direct"),
            queued: tracer.label("queued"),
            tracer,
        }
    }

    pub(super) fn queued_batch(&self, started: Instant) {
        self.tracer
            .record_span(self.batch, self.queued, started.elapsed());
    }

    fn direct_batch(&self, started: Instant) {
        self.tracer
            .record_span(self.batch, self.direct, started.elapsed());
    }
}

impl InputModule {
    pub(super) fn direct_batch(&mut self, call: DirectCall<'_>) -> Option<Value> {
        if call.service != SERVICE
            || call.payload.get("action").and_then(Value::as_str) != Some("batch")
            || self.batch_must_keep_queue_order(&call.payload)
        {
            return None;
        }
        self.controller.select_request(call.caller, &call.payload);
        let started = Instant::now();
        let bound = call.payload.get("operation_id").is_some()
            || call.payload.get("input_binding").is_some();
        let reply = if bound {
            self.direct_bound_batch(call.caller, call.payload)
        } else {
            self.direct_unbound_batch(call.caller, call.payload)
        };
        if reply.is_some() {
            self.batch_trace.direct_batch(started);
        }
        reply
    }

    /// A batch must not overtake requests that are waiting for a later tick,
    /// nor run while a job that fences input is still settling.
    fn batch_must_keep_queue_order(&self, payload: &Value) -> bool {
        self.shutdown_rejects(payload)
            || self.shortcut_job.is_some()
            || self.acquire_job.is_some()
            || self.activation_job.is_some()
            || self.pending_revoke.is_some()
            || self.pending_retirement_acknowledgement.is_some()
            || !self.queued.is_empty()
            || should_defer_settings_request(&self.product_settings, SERVICE, payload)
    }

    fn direct_unbound_batch(&mut self, caller: &str, payload: Value) -> Option<Value> {
        let lease = payload.get("lease").and_then(Value::as_str).unwrap_or("");
        self.bindings.authorize_batch(caller, lease, None).ok()?;
        let Ok(operation @ Operation::Batch { .. }) = serde_json::from_value(payload) else {
            return None;
        };
        // The caller is blocked in this call, so it cannot cancel midway.
        let mut uncancelled = || Ok(());
        Some(
            self.controller
                .handle(caller, operation, Instant::now(), &mut uncancelled),
        )
    }

    /// A lifecycle-bound batch. Keyboard input that may match an approved
    /// shortcut needs the shortcut job and is left to the queued path.
    fn direct_bound_batch(&mut self, caller: &str, payload: Value) -> Option<Value> {
        let batch: BoundBatch = serde_json::from_value(payload).ok()?;
        let target = self
            .bindings
            .batch_target(caller, &batch.lease, &batch.input_binding)
            .ok()?;
        if !self
            .bindings
            .batch_active(caller, &batch.lease, &batch.input_binding)
            .ok()?
        {
            return None;
        }
        let now = Instant::now();
        let completes_now = non_keyboard_batch(&batch.events)
            || self.shortcut_cache.can_bypass_keyboard(
                &batch.input_binding,
                &target,
                now,
                self.controller.consumed_held(),
            );
        if !completes_now {
            return None;
        }
        // Admitting the sequence is the first effect. A refusal changes
        // nothing, and the queued path then reports it to the caller.
        self.bindings
            .admit_sequence(
                caller,
                &batch.lease,
                &batch.input_binding,
                &batch.operation_id,
            )
            .ok()?;
        let bindings = &self.bindings;
        let (lease, binding) = (batch.lease.clone(), batch.input_binding.clone());
        let mut still_active = || match bindings.batch_active(caller, &lease, &binding) {
            Ok(true) => Ok(()),
            Ok(false) => Err(InputError::new("binding_inactive", "input binding retired")),
            Err(code) => Err(InputError::new(code, "input binding retired")),
        };
        let mut reply = self.controller.handle(
            caller,
            Operation::Batch {
                lease: batch.lease,
                events: batch.events,
            },
            now,
            &mut still_active,
        );
        if reply["ok"] == true {
            reply["operation_id"] = batch.operation_id.into();
        }
        Some(reply)
    }
}
