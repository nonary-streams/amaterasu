//! Keeps ended tickets from holding back open ones.
//!
//! A ticket that was asked to close only retires what it held. Its cleanup
//! runs in a job of its own, so a stop that is slow or never answered cannot
//! delay reading the next request, admitting the next ticket or another
//! ticket's cleanup. Admission is decided from open tickets only.
use super::*;

/// Whether one more ticket may be admitted. It receives open tickets only,
/// so a ticket that is closing or failed to close cannot use up the limit.
pub(super) fn ticket_admitted(live: &Live<'_, Record>, limit: usize) -> bool {
    live.admits(limit)
}

impl Controller {
    /// Work on one ended ticket with its job in the job slot. The workflow
    /// reads and writes that one slot; which ticket it serves is the lane.
    pub(super) fn with_retiring_job(&mut self, ticket: u64, work: impl FnOnce(&mut Self)) {
        debug_assert!(self.parked_job.is_none());
        self.parked_job = self.job.take();
        self.job = self.retiring_jobs.remove(&ticket);
        work(self);
        if let Some(job) = self.job.take() {
            self.retiring_jobs.insert(job.ticket, job);
        }
        self.job = self.parked_job.take();
    }

    /// A ticket can end while its job is in flight, and closing one starts
    /// its cleanup in the job slot. Either way the job then leaves the slot,
    /// which serves open tickets and incoming requests.
    pub(super) fn move_ended_job_to_its_own_lane(&mut self) {
        let ended = self.job.as_ref().is_some_and(|job| {
            self.records
                .get(&job.ticket)
                .is_some_and(|record| record.admission() == Admission::Ended)
        });
        if ended && let Some(job) = self.job.take() {
            self.retiring_jobs.insert(job.ticket, job);
        }
    }

    pub(super) fn advance_retiring_jobs(&mut self, runtime: &RuntimeClient, now: u64) {
        for ticket in self.retiring_jobs.keys().copied().collect::<Vec<_>>() {
            self.with_retiring_job(ticket, |controller| controller.advance_job(runtime, now));
        }
    }

    /// Start the next cleanup step of every ended ticket that is not already
    /// running one. Failed cleanup is retried here on its own schedule.
    pub(super) fn schedule_retiring(&mut self, now: u64) {
        let tickets: Vec<u64> = self
            .records
            .values()
            .filter(|record| {
                matches!(record.state, State::Closing | State::CleanupFailed)
                    && !self.has_job(record.id)
            })
            .map(|record| record.id)
            .collect();
        for ticket in tickets {
            if !self.cleanup_step_due(ticket, now) {
                continue;
            }
            self.with_retiring_job(ticket, |controller| {
                let mut record = controller
                    .records
                    .remove(&ticket)
                    .expect("selected closing record");
                controller.schedule_cleanup(&mut record, None, now);
                controller.records.insert(ticket, record);
            });
        }
    }

    /// Whether this ticket's cleanup should take a step now. A failed cleanup
    /// waits out its retry delay first and is retried a bounded number of
    /// times.
    fn cleanup_step_due(&mut self, ticket: u64, now: u64) -> bool {
        let record = self
            .records
            .get_mut(&ticket)
            .expect("selected closing record");
        if record.state == State::Closing {
            return true;
        }
        if let Some(retry_at) = record.cleanup_retry_at {
            if now < retry_at {
                return false;
            }
            record.cleanup_retry_at = None;
            record.cleanup_error = None;
            record.state = State::Closing;
            return true;
        }
        if record.cleanup_retry_count >= MAX_CLEANUP_RETRIES {
            return false;
        }
        let delay = CLEANUP_RETRY_DELAYS_MS[usize::from(record.cleanup_retry_count)];
        record.cleanup_retry_count += 1;
        record.cleanup_retry_at = Some(now.saturating_add(delay));
        false
    }

    /// One advance pass as `tick` runs it, for tests that drive the
    /// controller step by step.
    #[cfg(test)]
    pub(super) fn advance_work(&mut self, runtime: &RuntimeClient, now: u64) {
        self.move_ended_job_to_its_own_lane();
        self.advance_job(runtime, now);
        self.move_ended_job_to_its_own_lane();
        self.advance_retiring_jobs(runtime, now);
    }

    /// One scheduling pass as `tick` runs it, for the same tests.
    #[cfg(test)]
    pub(super) fn schedule_work(&mut self, now: u64) {
        self.move_ended_job_to_its_own_lane();
        self.schedule_retiring(now);
        if self.job.is_none() {
            self.schedule_background(now);
        }
    }

    pub(super) fn has_job(&self, ticket: u64) -> bool {
        self.retiring_jobs.contains_key(&ticket)
            || self
                .job
                .iter()
                .chain(self.parked_job.iter())
                .any(|job| job.ticket == ticket)
    }

    pub(super) fn no_jobs(&self) -> bool {
        self.job.is_none() && self.parked_job.is_none() && self.retiring_jobs.is_empty()
    }

    /// Record when each ticket ended, as first seen. Tickets end in many
    /// places; observing it here keeps one definition.
    pub(super) fn stamp_ended(&mut self, now: u64) {
        for record in self.records.values_mut() {
            if record.admission() == Admission::Ended {
                record.ended_at.get_or_insert(now);
            }
        }
    }

    /// Keep space for the next ticket. Ended tickets are dropped when the
    /// record table is full, the most settled first, after their retained
    /// broker calls are handed to settlement. Open tickets are never dropped,
    /// so a table full of those is the only thing that refuses a ticket.
    pub(super) fn keep_ticket_capacity(&mut self) {
        if self.records.len() < MAX_RECORDS {
            return;
        }
        let busy: BTreeSet<u64> = self
            .job
            .iter()
            .chain(self.parked_job.iter())
            .map(|job| job.ticket)
            .collect();
        let mut dropped = Vec::new();
        make_room(
            &mut self.records,
            MAX_RECORDS,
            |ticket, _| !busy.contains(ticket),
            |ticket, record| dropped.push((ticket, record)),
        );
        for (ticket, mut record) in dropped {
            eprintln!(
                "rtsp ticket record dropped for capacity state={:?} cleanup_error={}",
                record.state,
                record.cleanup_error.unwrap_or("none")
            );
            self.revoke_ticket(ticket);
            let retained = self
                .retiring_jobs
                .remove(&ticket)
                .map(|job| job.call)
                .into_iter()
                .chain(record.uncertain.take());
            for mut call in retained {
                // Settlement of the exact broker identity continues without
                // the record. Stream-session keeps its own record of what
                // the session still holds.
                call.cancel();
                if self.shutdown_settlement.len() < MAX_RECORDS {
                    self.shutdown_settlement.push(call);
                }
            }
        }
    }
}
