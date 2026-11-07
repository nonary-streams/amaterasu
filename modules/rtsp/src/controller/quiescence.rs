//! Drain transport ownership while the host continues ticking every provider.
use super::*;

pub(super) struct Quiescence {
    deadline: u64,
}

impl Controller {
    pub fn prepare_shutdown(&mut self, now: u64, remaining_ms: u64) {
        let announced_deadline = now.saturating_add(remaining_ms);
        let deadline = self
            .shutdown_deadline
            .map_or(announced_deadline, |existing| {
                existing.min(announced_deadline)
            });
        self.shutdown_deadline = Some(deadline);
        if let Some(quiescence) = self.quiescence.as_mut() {
            quiescence.deadline = quiescence.deadline.min(deadline);
            self.cap_quiescence_deadlines();
        }
        // Admission guards observe the announcement immediately. Existing
        // transport and native effects remain available until all declared
        // consumers have drained and the host begins this module's quiescence.
        self.cap_announced_cleanup_deadline();
    }

    pub(super) fn cap_announced_cleanup_deadline(&mut self) {
        let Some(deadline) = self.shutdown_deadline else {
            return;
        };
        let Some(job) = self.job.as_mut() else {
            return;
        };
        let cleanup = matches!(job.phase, Phase::Stop | Phase::Find)
            || self.records.get(&job.ticket).is_some_and(|record| {
                matches!(record.state, State::Closing | State::CleanupFailed)
            });
        if cleanup {
            // An owner may request cleanup while its provider waits for DAG
            // quiescence. That cleanup still shares the host's original bound.
            job.deadline = job.deadline.min(deadline);
        }
    }

    pub fn quiesce(&mut self, runtime: &RuntimeClient, now: u64) -> bool {
        if self.quiescence.is_none() {
            self.quiescence = Some(Quiescence {
                deadline: now
                    .saturating_add(CLEANUP_BUDGET_MS)
                    .min(self.shutdown_deadline.unwrap_or(u64::MAX)),
            });
            self.begin_quiescence(runtime);
        }
        self.quiescence_ready()
    }

    fn begin_quiescence(&mut self, runtime: &RuntimeClient) {
        if self.shutdown_settlement.len() < MAX_RECORDS
            && self
                .job
                .as_ref()
                .is_some_and(|job| job.phase == Phase::Observe)
        {
            let mut job = self.job.take().expect("background status job");
            job.call.cancel();
            // A read-only broker observation cannot fence native Stop. Retain
            // its independent settlement proof until cancellation/release ends.
            self.shutdown_settlement.push(job.call);
        }
        let deadline = self
            .quiescence
            .as_ref()
            .expect("quiescence started")
            .deadline;
        if let Some(job) = self.job.as_mut() {
            if self.records.get(&job.ticket).is_some_and(|record| {
                !record.state.terminal()
                    && !matches!(record.state, State::Closing | State::CleanupFailed)
            }) {
                // Enter the shared cleanup budget once when cancelling forward
                // progress; a nearly expired prepare budget is not its cleanup
                // budget. Already-running cleanup retains its earlier bound.
                job.deadline = deadline;
            }
            if matches!(
                job.phase,
                Phase::Prepare | Phase::LifecyclePrepare | Phase::Arm
            ) {
                // Settle the exact submitted mutation before lookup or cleanup.
                // Broker cancellation never proves its effects were undone.
                job.call.cancel();
            }
        }
        if let Some(mut admission) = self.display_admission.take() {
            rpc::complete(runtime, admission.request, error("module_quiescing"));
            if !admission.verified {
                admission.call.cancel();
                self.display_cleanup.push(DisplayCleanup {
                    owner: admission.caller,
                    operation_id: admission.config.operation_id,
                    call: admission.call,
                });
            }
        }
        for record in self.records.values_mut() {
            // Closing the transport replaces a delayed TEARDOWN reply. Native
            // resource ownership still needs its independent terminal receipt.
            record.revoke_after = None;
        }
        self.transport = None;
        self.local = None;
        self.locals.clear();
        self.pending_network.clear();
        self.close_quiescing_records();
        self.cap_quiescence_deadlines();
    }

    pub(super) fn tick_quiescing(&mut self, runtime: &RuntimeClient, now: u64) -> ModuleResult<()> {
        self.cap_quiescence_deadlines();
        let deadline = self
            .quiescence
            .as_ref()
            .expect("quiescence started")
            .deadline;
        if now >= deadline {
            self.expire_quiescence(runtime);
            if self.job.is_none() {
                self.poll_caller(runtime, now)?;
            }
            // Explicit status/stop operations remain callable, but cannot open
            // a fresh cleanup budget or dispatch work after this fixed bound.
            self.expire_quiescence(runtime);
            return Ok(());
        }

        self.advance_startup_recovery(runtime, now);
        self.advance_job(runtime, now);
        self.close_quiescing_records();
        self.advance_display_cleanup(runtime);
        self.advance_shutdown_settlement(runtime);
        if self.job.is_none()
            && let Some(ticket) = self
                .records
                .values()
                .find(|record| record.state == State::Closing)
                .map(|record| record.id)
        {
            let mut record = self
                .records
                .remove(&ticket)
                .expect("selected closing record");
            self.schedule_cleanup(&mut record, None, now);
            self.records.insert(ticket, record);
        }
        if self.job.is_none() {
            self.poll_caller(runtime, now)?;
        }
        // Existing response handlers may schedule a following lookup/stop.
        // They all consume the original aggregate deadline during shutdown.
        self.cap_quiescence_deadlines();
        Ok(())
    }

    pub(super) fn advance_shutdown_settlement(&mut self, runtime: &RuntimeClient) {
        self.shutdown_settlement
            .retain_mut(|call| match call.poll(runtime) {
                Outcome::SettledFailure | Outcome::Result(_) => false,
                Outcome::Waiting => true,
                Outcome::Uncertain => {
                    // Retrying metadata settlement does not repeat a failed undo.
                    // Quiescence applies its aggregate deadline separately.
                    call.retry_settlement();
                    true
                }
            });
    }

    fn close_quiescing_records(&mut self) {
        let observed = self
            .job
            .as_ref()
            .and_then(|job| (job.phase == Phase::Observe).then_some(job.ticket));
        for record in self.records.values_mut() {
            if record.state.terminal() || record.state == State::CleanupFailed {
                continue;
            }
            if observed == Some(record.id) && record.state == State::Playing {
                // Let an in-flight background observation finish first. Marking
                // it Closing would convert an active reply into endless status
                // polling without ever requesting the stream's stop.
                continue;
            }
            if record.state != State::Closing {
                record.close(Some("module_stopping"));
            }
            if record.cleanup_confirmed
                && record.uncertain.is_none()
                && !record.lifecycle_prepare_outstanding
            {
                finish_record(record);
            }
        }
    }

    fn cap_quiescence_deadlines(&mut self) {
        let deadline = self
            .quiescence
            .as_ref()
            .expect("quiescence started")
            .deadline;
        if let Some(job) = self.job.as_mut() {
            job.deadline = job.deadline.min(deadline);
        }
        self.startup_recovery.deadline = self.startup_recovery.deadline.min(deadline);
    }

    fn expire_quiescence(&mut self, runtime: &RuntimeClient) {
        let mut caller = None;
        if let Some(mut job) = self.job.take() {
            caller = job.caller.map(|caller| (job.ticket, caller));
            job.call.cancel();
            if let Some(record) = self.records.get_mut(&job.ticket) {
                if job.call.proven_not_dispatched() {
                    apply_undispatched_shutdown_proof(record, job.phase, &job.call);
                } else {
                    record.uncertain_phase = Some(job.phase);
                    record.uncertain = Some(job.call);
                }
            }
        }
        if self.startup_recovery.phase != RecoveryPhase::Complete
            && self.startup_recovery.phase != RecoveryPhase::Blocked
        {
            self.startup_recovery.phase = RecoveryPhase::Blocked;
            self.startup_recovery.error = Some("quiescence_deadline");
            if let Some(call) = self.startup_recovery.call.as_mut() {
                call.cancel();
            }
        }
        for record in self.records.values_mut() {
            if record.state.terminal() || record.state == State::CleanupFailed {
                continue;
            }
            if record.session.is_none()
                && !record.needs_lookup
                && record.lifecycle_target.is_none()
                && !record.lifecycle_prepare_outstanding
                && record.uncertain.is_none()
            {
                record.cleanup_confirmed = true;
                finish_record(record);
            } else {
                record.state = State::CleanupFailed;
                record.cleanup_error = Some("quiescence_deadline");
            }
        }
        if let Some((ticket, caller)) = caller
            && let Some(record) = self.records.get(&ticket)
        {
            complete_caller(record, Some(caller), runtime);
        }
    }

    fn quiescence_ready(&self) -> bool {
        self.job.is_none()
            && self.display_admission.is_none()
            && self.display_cleanup.is_empty()
            && self.shutdown_settlement.is_empty()
            && self.startup_recovery.ready()
            && self.startup_recovery.call.is_none()
            && self.records.values().all(|record| {
                record.state.terminal()
                    && record.cleanup_confirmed
                    && record.cleanup_error.is_none()
                    && record.uncertain.is_none()
                    && !record.needs_lookup
                    && !record.lifecycle_prepare_outstanding
            })
    }
}
