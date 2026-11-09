//! Seal admission before the host drains consumers, then retire every owned
//! input context while the normal broker jobs and cleanup retries keep ticking.
use super::*;

const DEFAULT_SHUTDOWN_BUDGET_MS: u64 = 20_000;

#[derive(Default)]
pub(super) struct Shutdown {
    deadline: Option<Instant>,
    quiescing: bool,
    rejected: Option<QueuedRequest>,
}

impl Shutdown {
    pub(super) fn deadline(&self) -> Instant {
        self.deadline.expect("shutdown announced")
    }

    pub(super) fn announce(&mut self, now: Instant, remaining_ms: u64) {
        let announced = now
            .checked_add(Duration::from_millis(remaining_ms))
            .unwrap_or(now);
        self.deadline = Some(
            self.deadline
                .map_or(announced, |prior| prior.min(announced)),
        );
    }

    fn rejects(&self, payload: &Value) -> bool {
        let action = payload.get("action").and_then(Value::as_str);
        if self.quiescing {
            !matches!(
                action,
                Some(
                    "status"
                        | "lookup"
                        | "binding_status"
                        | "settings_get"
                        | "confirm_delivery"
                        | "feedback_peek"
                        | "feedback_ack"
                        | "poll_gamepad_feedback"
                        | "release"
                        | "revoke_binding"
                        | "acknowledge_binding_retirement"
                )
            )
        } else {
            self.deadline.is_some() && matches!(action, Some("acquire" | "settings_update"))
        }
    }
}

impl InputModule {
    /// Whether shutdown has closed admission for this request.
    pub(super) fn shutdown_rejects(&self, payload: &Value) -> bool {
        self.shutdown.rejects(payload)
    }

    pub(super) fn reject_shutdown_request(
        &mut self,
        request: &QueuedRequest,
    ) -> ModuleResult<bool> {
        if request.service != SERVICE || !self.shutdown.rejects(&request.payload) {
            return Ok(false);
        }
        if let Err(error) = self.complete(
            request.id,
            InputError::new(
                "shutting_down",
                "input admission is closed for host shutdown",
            )
            .payload(),
            &request.caller,
            false,
        ) {
            self.shutdown.rejected = Some(request.clone());
            return Err(error);
        }
        Ok(true)
    }

    pub(super) fn settle_shutdown_rejection(&mut self) -> ModuleResult<()> {
        let Some(request) = self.shutdown.rejected.take() else {
            return Ok(());
        };
        // Retry only this exact completion. Its lost acknowledgement never
        // authorizes executing the rejected acquisition or input delivery.
        self.reject_shutdown_request(&request).map(|_| ())
    }

    pub(super) fn quiesce_input(&mut self, now: Instant) -> ModuleResult<bool> {
        self.product_settings.prepare_shutdown();
        if self.shutdown.deadline.is_none() {
            self.shutdown.announce(now, DEFAULT_SHUTDOWN_BUDGET_MS);
        }
        self.controller
            .restrict_shutdown_deadline(self.shutdown.deadline());
        self.shutdown.quiescing = true;
        let _ = self.settle_shutdown_rejection();
        let _ = self.advance_retirement_acknowledgement();
        // Reject deferred acquisitions and input delivery even if a settings or
        // shortcut operation currently prevents the ordinary queue from draining.
        // Already admitted asynchronous jobs retain their exact broker/state
        // ownership and finish through the normal tick path before teardown.
        for _ in 0..self.queued.len() {
            if self.shutdown.rejected.is_some() {
                break;
            }
            let request = self.queued.pop_front().expect("queued request");
            match self.reject_shutdown_request(&request) {
                Ok(true) => {}
                Ok(false) => self.queued.push_back(request),
                Err(_) => {
                    // A transport failure may have lost only the completion
                    // acknowledgement. Retain this exact request for settlement.
                    break;
                }
            }
        }
        let jobs_pending = self.acquire_job.is_some()
            || self.activation_job.is_some()
            || self.shortcut_job.is_some()
            || self.pending_revoke.is_some()
            || self.pending_retirement_acknowledgement.is_some()
            || self.product_settings.busy()
            || self.shutdown.rejected.is_some()
            || !self.queued.is_empty();
        let deadline_expired = now >= self.shutdown.deadline.expect("shutdown announced");
        if jobs_pending && !deadline_expired {
            return Ok(false);
        }
        // A stalled metadata or durable shortcut operation may consume the
        // budget, but it must not suppress the final owned native release pass.
        let native_retired = self.controller.quiesce_all(now);
        if !jobs_pending && native_retired {
            return Ok(true);
        }
        if deadline_expired {
            return Err(
                "input shutdown deadline exceeded; owned cleanup remains unresolved".into(),
            );
        }
        Ok(false)
    }
}
