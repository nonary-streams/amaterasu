//! Optional, nonallocating commands never use the fatal allocation workflow.
//! Their broker bookkeeping retires independently; native ownership fences do not.
use super::{
    Job, Pipeline, Send, Step, StreamFailure, input, lifecycle_issues,
    record::{Record, State},
    rpc,
};
use amaterasu_module_sdk::stream_lifecycle::{
    LifecycleLayer, StreamFailureDecision, StreamFailureHandler, StreamIssue,
};

pub(super) const MAX_RETIREMENTS: usize = 8;

pub(super) struct Retirement {
    pending: rpc::Pending,
    // Conservative bookkeeping bound. Pending itself forbids admission replay
    // after the broker's retention horizon; these operations own no resources.
    abandon_after: u64,
}
const BACKOFF_MS: u64 = 100;

pub(super) fn optional(step: Step) -> bool {
    matches!(
        step,
        Step::ForceIdr | Step::FeedbackPeek | Step::FeedbackAck | Step::PollGamepadFeedback
    )
}

impl Pipeline {
    pub(super) fn advance_maintenance(
        &mut self,
        record: &mut Record,
        job: &mut Job,
        now: u64,
        send: &mut Send<'_>,
    ) -> bool {
        debug_assert!(record.state == State::Active && !job.cleanup && optional(job.step));
        if now >= job.deadline
            || job
                .request
                .is_some_and(|request| !rpc::active(send, request))
        {
            return self.skip_maintenance(record, job, now, "maintenance_deadline");
        }
        let Some(mut pending) = job.pending.take() else {
            // Reserve a bookkeeping slot before admitting another operation.
            if self.maintenance_retirements.len() >= MAX_RETIREMENTS {
                return self.skip_maintenance(record, job, now, "maintenance_settlement_pending");
            }
            let Some(sequence) = self.sequence.checked_add(1) else {
                return self.skip_maintenance(record, job, now, "operation_counter_exhausted");
            };
            self.sequence = sequence;
            if job.step == Step::FeedbackPeek {
                record.feedback_peek_generation = record.contribution_barrier.ready_generation();
            }
            let (service, payload) = job.command(record);
            job.pending = Some(rpc::Pending::new(
                &service,
                payload,
                format!("ss-{:x}-{sequence}", record.id),
                job.request,
            ));
            return true;
        };
        match pending.poll(send, now) {
            rpc::Progress::Waiting => {
                job.pending = Some(pending);
                true
            }
            rpc::Progress::Unresolved => {
                job.pending = Some(pending);
                self.skip_maintenance(record, job, now, "maintenance_unconfirmed")
            }
            rpc::Progress::Failed => {
                if matches!(job.step, Step::ForceIdr | Step::PollGamepadFeedback)
                    && pending.admission_error_code() == Some("permission_denied")
                {
                    job.fail(
                        record,
                        StreamFailure::critical("dependency_unavailable"),
                        now,
                    );
                    return true;
                }
                if !pending.admission_rejected() && pending.request_id().is_none() {
                    job.pending = Some(pending);
                }
                self.skip_maintenance(record, job, now, "maintenance_unavailable")
            }
            rpc::Progress::Result(payload) => {
                if !pending.release_result(send) {
                    pending.cancel();
                    self.maintenance_retirements.push(Retirement {
                        pending,
                        abandon_after: now.saturating_add(300_000),
                    });
                }
                // An explicit ownership/recovery fence is never demoted merely
                // because the operation normally provides optional feedback.
                if payload["recovery_required"] == true || payload["cleanup_required"] == true {
                    job.fail(
                        record,
                        StreamFailure::critical("maintenance_recovery_required"),
                        now,
                    );
                    return true;
                }
                if payload["ok"] != true
                    && matches!(job.step, Step::ForceIdr | Step::PollGamepadFeedback)
                    && (payload["error"]["code"] == "permission_denied"
                        || payload["code"] == "permission_denied")
                {
                    job.fail(record, StreamFailure::critical("dependency_rejected"), now);
                    return true;
                }
                if payload["ok"] != true {
                    if job.step == Step::FeedbackPeek
                        && record
                            .start
                            .as_ref()
                            .is_some_and(|start| start.outbound_agreement.is_some())
                        && matches!(
                            payload["error"]["code"].as_str(),
                            Some("unsupported" | "invalid_request")
                        )
                    {
                        if !record.feedback_contributions_unsupported
                            && let Some(operation) = &record.config.rtsp_operation_id
                        {
                            record.issues.push(
                                StreamIssue::advisory(
                                    operation,
                                    LifecycleLayer::StreamSession,
                                    "feedback_peek",
                                    "input_feedback_unsupported",
                                )
                                .because("unsupported")
                                .caused_by(LifecycleLayer::Input),
                            );
                        }
                        record.feedback_contributions_unsupported = true;
                    }
                    return self.skip_maintenance(record, job, now, "maintenance_rejected");
                }
                let result = match job.step {
                    Step::ForceIdr => {
                        record.force_idr = false;
                        Ok(false)
                    }
                    Step::FeedbackPeek => {
                        input::accept_feedback_peek(record, &payload, now).map(|through| {
                            if through.is_some() {
                                job.step = Step::FeedbackAck;
                                true
                            } else {
                                false
                            }
                        })
                    }
                    Step::FeedbackAck => {
                        input::accept_feedback_ack(record, &payload, now).map(|_| false)
                    }
                    Step::PollGamepadFeedback => {
                        input::accept_gamepad_feedback(record, &payload, now).map(|_| false)
                    }
                    _ => unreachable!("maintenance operation was selected"),
                };
                match result {
                    Ok(continue_job) => continue_job,
                    Err(reason) => self.skip_maintenance(record, job, now, reason),
                }
            }
        }
    }

    fn skip_maintenance(
        &mut self,
        record: &mut Record,
        job: &mut Job,
        now: u64,
        reason: &'static str,
    ) -> bool {
        if let Some(mut pending) = job.pending.take()
            && pending.was_sent()
            && !pending.admission_rejected()
        {
            pending.cancel();
            // Only this single workflow can add a slot; admission was capped.
            self.maintenance_retirements.push(Retirement {
                pending,
                abandon_after: now.saturating_add(300_000),
            });
            debug_assert!(self.maintenance_retirements.len() <= MAX_RETIREMENTS);
        }
        let fallback = match job.step {
            Step::ForceIdr => {
                record.force_idr = false;
                "await_next_keyframe"
            }
            Step::FeedbackPeek => {
                record.feedback_ack_through = None;
                record.feedback_poll_at = now.saturating_add(BACKOFF_MS);
                "skip_feedback"
            }
            Step::FeedbackAck => {
                record.feedback_poll_at = now.saturating_add(BACKOFF_MS);
                "retry_feedback_ack"
            }
            Step::PollGamepadFeedback => {
                record.rumble_poll_at = now.saturating_add(BACKOFF_MS);
                "skip_feedback"
            }
            _ => unreachable!("only maintenance can be skipped"),
        };
        if let Some(operation) = &record.config.rtsp_operation_id {
            let report = StreamIssue::demoted(
                operation,
                LifecycleLayer::StreamSession,
                lifecycle_issues::step_label(job.step),
                "maintenance_unavailable",
                fallback,
            )
            .because(reason)
            .caused_by(lifecycle_issues::step_layer(job.step));
            match StreamFailureHandler::decide(report) {
                StreamFailureDecision::Continue(continuation) => {
                    record.issues.push(continuation.into_issue())
                }
                StreamFailureDecision::Terminate(permit) => {
                    // Only the common handler can authorize a terminal effect.
                    permit.terminate(|_| {
                        job.fail(record, StreamFailure::critical("maintenance_failed"), now)
                    });
                    return true;
                }
                StreamFailureDecision::Invalid(_) => {
                    // Invalid optional diagnostics do not create authority to
                    // stop a healthy stream or change the media contract.
                }
            }
        }
        false
    }

    pub(super) fn progress_maintenance_retirements(&mut self, now: u64, send: &mut Send<'_>) {
        self.maintenance_retirements.retain_mut(|retirement| {
            let pending = &mut retirement.pending;
            if now >= retirement.abandon_after && pending.request_id().is_none() {
                // No allocation/lease was requested. Never replay an expired
                // broker operation to infer native retirement.
                return false;
            }
            match pending.poll(send, now) {
                rpc::Progress::Failed
                    if pending.was_sent()
                        && pending.request_id().is_none()
                        && !pending.admission_rejected() =>
                {
                    pending.retry();
                    true
                }
                rpc::Progress::Failed => false,
                rpc::Progress::Waiting => true,
                rpc::Progress::Result(_) => !pending.release_result(send),
                rpc::Progress::Unresolved => {
                    pending.retry();
                    true
                }
            }
        });
    }
}
