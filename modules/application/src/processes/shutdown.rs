//! One retained shutdown deadline owns admission, cooperative drain, and final containment.
use super::*;

const UNANNOUNCED_DROP_BUDGET: Duration = Duration::from_secs(5);

impl ProcessManager {
    /// Announcement only: do not stop a provider before its consumers drain.
    pub(crate) fn prepare_shutdown_until(&mut self, deadline: Instant) {
        let deadline = self
            .shutdown_deadline
            .map_or(deadline, |current| current.min(deadline));
        self.shutdown_deadline = Some(deadline);
        self.proton_hooks.prepare_shutdown();
        for record in self.records.values_mut() {
            record.prepare_shutdown_until(deadline);
        }
        for lease in self.preparations.values_mut() {
            lease.lifecycle.prepare_shutdown(deadline);
        }
    }

    /// Advance every owned stop once; never allocate another deadline or wait.
    pub(crate) fn poll_shutdown(&mut self, now: Instant) -> Result<bool, &'static str> {
        if self.shutdown_deadline.is_none() {
            self.prepare_shutdown_until(now + self.shutdown_budget());
        }
        self.shutdown_started = true;
        let mut release_hooks = Vec::new();
        let mut scanners_retired = true;
        for record in self.records.values_mut() {
            if let Err(error) = record.prepare_shutdown() {
                self.shutdown_error = Some(error);
            }
            if let Err(error) = record.advance_at(now) {
                self.shutdown_error = Some(error);
            }
            if let Some(tracker) = &mut record.steam_tracker {
                scanners_retired &= tracker.poll_scanner_shutdown();
            }
            if record.is_terminal()
                && let Some(app_id) = record.steam_proton_hook_app_id.take()
            {
                release_hooks.push(app_id);
            }
        }
        for app_id in release_hooks {
            self.proton_hooks.release(&app_id);
        }
        for lease in self.preparations.values_mut() {
            lease.prepare_shutdown(now);
        }
        self.proton_hooks.reap();
        let settled = scanners_retired
            && self
                .records
                .values()
                .all(ProcessRecord::settled_for_shutdown)
            && self
                .preparations
                .values()
                .all(PreparationLease::settled_for_shutdown)
            && self.proton_hooks.shutdown_complete();
        if settled {
            let failed = self
                .records
                .values()
                .any(|record| matches!(record.state, ProcessState::CleanupFailed { .. }))
                || self
                    .preparations
                    .values()
                    .any(|lease| lease.lifecycle.cleanup_failed());
            return if failed {
                Err("process_cleanup_failed")
            } else {
                Ok(true)
            };
        }
        if now
            >= self
                .shutdown_deadline
                .expect("shutdown retains its deadline")
        {
            return Err(self.shutdown_error.unwrap_or("process_wait_timeout"));
        }
        Ok(false)
    }

    /// Synchronous convenience for direct owners; an announced host deadline wins.
    pub fn shutdown(&mut self) -> Result<(), &'static str> {
        if self.shutdown_deadline.is_none() {
            self.prepare_shutdown_until(Instant::now() + self.shutdown_budget());
        }
        loop {
            if self.poll_shutdown(Instant::now())? {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Final native attempts do not restart grace, launch undo, or claim retirement.
    fn force_shutdown_once(&mut self) {
        for record in self.records.values_mut() {
            record.force_shutdown();
        }
        for lease in self.preparations.values_mut() {
            lease.lifecycle.force_shutdown();
        }
        self.proton_hooks.force_shutdown();
    }
}

impl Drop for ProcessManager {
    fn drop(&mut self) {
        if self.shutdown_deadline.is_some() || self.shutdown_started {
            self.force_shutdown_once();
            return;
        }
        // Legacy direct owners get one short fallback; host announcements never do.
        self.prepare_shutdown_until(
            Instant::now() + self.shutdown_budget().min(UNANNOUNCED_DROP_BUDGET),
        );
        if self.shutdown().is_err() {
            self.force_shutdown_once();
        }
    }
}

#[cfg(test)]
#[path = "shutdown_tests.rs"]
mod tests;
