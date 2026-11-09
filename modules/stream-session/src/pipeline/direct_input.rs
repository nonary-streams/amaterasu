//! Delivers queued input in the same tick that received it.
//!
//! A queued batch request needs one tick to schedule, one to send, the input
//! provider's next tick to inject and another to read the reply, and it
//! shares the pipeline's single workflow slot with every other session
//! operation. A direct call injects before it returns. The provider may
//! decline any batch; the queued request then carries the identical payload,
//! so ordering and lifecycle sequence numbers are the same on both paths.
use super::input::{
    accept_acknowledged_input_batch, accept_controller_batch_failure, input_batch_payload,
    next_input_batch_size, stop_for_input_failure,
};
use super::record::{Record, State};
use super::{Pipeline, Step};
use amaterasu_module_sdk::input::INPUT_SERVICE;
use amaterasu_module_sdk::{DirectReply, ModuleResult};
use serde_json::Value;

pub(super) type DirectCall<'a> = dyn FnMut(&str, Value) -> ModuleResult<DirectReply> + 'a;

/// Bounds one tick's work when events alternate between batch kinds.
const MAX_DIRECT_BATCHES_PER_TICK: usize = 8;
/// A provider may decline single batches, such as keyboard input that needs
/// its shortcut job. Repeated declines mean it does not serve batches
/// directly at all, and offering each one first would only add a call.
const DECLINES_BEFORE_BACKOFF: u8 = 3;
const DECLINE_BACKOFF_MS: u64 = 1_000;

impl Pipeline {
    pub(super) fn deliver_input_directly(&mut self, now: u64, direct: &mut DirectCall<'_>) {
        // A queued batch owns the queue's front until its reply is consumed.
        let queued_batch_owner = self
            .job
            .as_ref()
            .filter(|job| job.step == Step::InputBatch)
            .map(|job| job.session);
        for record in self.records.values_mut() {
            record.input_direct_active = queued_batch_owner != Some(record.id)
                && direct_lane_open(record, now)
                && deliver(record, now, direct);
        }
    }
}

fn direct_lane_open(record: &Record, now: u64) -> bool {
    record.state == State::Active
        && record.input_lease.is_some()
        && record.input_inflight == 0
        && !record.input_events.is_empty()
        && now >= record.input_direct_retry_at
        // Managed capture admits control work one validated permit at a time.
        && !record
            .start
            .as_ref()
            .is_some_and(|start| start.manages_lifecycle_capture())
}

/// Returns whether the direct lane still owns this record's queue.
fn deliver(record: &mut Record, now: u64, direct: &mut DirectCall<'_>) -> bool {
    for _ in 0..MAX_DIRECT_BATCHES_PER_TICK {
        if record.input_events.is_empty() {
            break;
        }
        record.input_inflight = next_input_batch_size(record);
        record
            .input_timing
            .begin_batch(record.input_events.len(), record.input_inflight);
        match direct(INPUT_SERVICE, input_batch_payload(record)) {
            Ok(DirectReply::Completed(reply)) => {
                record.input_direct_declines = 0;
                if let Err(code) = accept_direct_reply(record, &reply) {
                    record.input_inflight = 0;
                    stop_for_input_failure(record, code);
                    return false;
                }
            }
            Ok(DirectReply::Unavailable { .. }) => {
                // Nothing reached the provider. Leave the events queued for
                // an ordinary request, which sends this same batch.
                record.input_inflight = 0;
                record.input_direct_declines = record.input_direct_declines.saturating_add(1);
                if record.input_direct_declines >= DECLINES_BEFORE_BACKOFF {
                    record.input_direct_retry_at = now.saturating_add(DECLINE_BACKOFF_MS);
                }
                return false;
            }
            Err(_) => {
                // The provider was entered and its outcome is unknown; a
                // replay could inject the same events twice.
                record.input_inflight = 0;
                stop_for_input_failure(record, "input_batch_failed");
                return false;
            }
        }
    }
    true
}

/// Applies a provider reply exactly as the queued request path does.
fn accept_direct_reply(record: &mut Record, reply: &Value) -> Result<(), &'static str> {
    if reply["ok"] == true {
        return accept_acknowledged_input_batch(record, reply);
    }
    if record.lifecycle_input_token.is_none() && reply["error"]["code"] == "controller_unavailable"
    {
        record.session_effects.vigem_missing(record.id);
        return accept_controller_batch_failure(record, reply);
    }
    Err("dependency_rejected")
}
