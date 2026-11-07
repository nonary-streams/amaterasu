//! Cooperative shutdown keeps providers ticking through the normal owned stop.
use super::*;

const SHUTDOWN_BUDGET_MS: u64 = 15_000;

impl Pipeline {
    pub fn prepare_shutdown(&mut self, now: u64, remaining_ms: u64) {
        let deadline = now.saturating_add(remaining_ms);
        self.shutdown_announced_deadline = Some(
            self.shutdown_announced_deadline
                .map_or(deadline, |prior| prior.min(deadline)),
        );
    }

    pub(super) fn shutdown_admission_closed(&self) -> bool {
        self.shutdown_announced_deadline.is_some() || self.shutdown_deadline.is_some()
    }

    pub fn quiesce(&mut self, now: u64) -> bool {
        let local_deadline = now.saturating_add(SHUTDOWN_BUDGET_MS);
        let deadline = *self.shutdown_deadline.get_or_insert_with(|| {
            self.shutdown_announced_deadline
                .map_or(local_deadline, |announced| announced.min(local_deadline))
        });
        if now < deadline {
            if let Some(job) = self.job.as_mut()
                && !job.cleanup
                && job.step != Step::PersistJournal
                && let Some(record) = self.records.get_mut(&job.session)
                && !matches!(record.state, State::Active | State::Stopping)
            {
                // Let active background work finish within its original bounded
                // deadline so the ordinary stop can drain the native plane.
                // Persist startup ownership before cancelling dependent work.
                job.fail(record, "host_stopping", now);
                job.deadline = job.deadline.min(deadline);
            }
            if self.job.is_none()
                && let Some(record) = self.records.values_mut().find(|record| {
                    !record.state.terminal()
                        && !matches!(record.state, State::Stopping | State::CleanupFailed)
                })
            {
                let mut job =
                    actions::stop_job(record, None, now, self.settings.drain_timeout_ms, false);
                job.deadline = job.deadline.min(deadline);
                self.job = Some(job);
            }
        }
        self.package_replacement_ready()
    }
}

impl Pipeline {
    pub(super) fn progress_shutdown_observations(&mut self, now: u64, send: &mut Send<'_>) {
        if !self
            .shutdown_deadline
            .is_some_and(|deadline| now < deadline)
        {
            return;
        }
        for record in self.records.values_mut() {
            record.shutdown_observations.retain_mut(|pending| {
                match pending.poll(send, now) {
                    rpc::Progress::Failed => false,
                    rpc::Progress::Result(_) => !pending.release_result(send),
                    rpc::Progress::Waiting => true,
                    rpc::Progress::Unresolved => {
                        // This is the exact read-only operation, never an
                        // allocation. Retry only settlement within the same cap.
                        pending.retry();
                        true
                    }
                }
            });
        }
    }
}
