//! Keeps an earlier launch's cleanup from deciding whether a later one starts.
//!
//! The application slot is decided from open records only. An application
//! that was asked to end never holds it. A launch that arrives while such an
//! application is still closing waits a bounded time for the cleanup and
//! then starts beside it. Unconfirmed cleanup is never reported as stopped:
//! an abandoned record keeps its `cleanup_error` and says so to management.
use super::{
    Controller, INITIAL_DEADLINE, LaunchRequest, MAX_RECORDS, PeerIdentity, Reply, Send,
    StreamResponse, private_error,
    record::{Record, State},
};
use amaterasu_module_sdk::LogLevel;
use amaterasu_module_sdk::lifecycle_ledger::{Admission, Live, Owner, make_room};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::time::{Duration, Instant};

/// How long a launch waits for an application that was asked to end. Normal
/// cleanup and its automatic retries finish inside this. Past it the earlier
/// cleanup continues on its own and the launch starts.
pub(super) const SUCCESSOR_GRACE: Duration = Duration::from_secs(15);

/// A paired launch held until it can be answered.
pub(super) struct DeferredLaunch {
    pub(super) connection: u64,
    pub(super) request: LaunchRequest,
    pub(super) peer: PeerIdentity,
    pub(super) response: StreamResponse,
    /// The latest this launch waits for a retained application to settle.
    pub(super) deadline: Instant,
    /// The latest this launch waits for an ended application's cleanup.
    pub(super) takeover_at: Instant,
}

/// Whether the host's single application slot can take a new root launch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ApplicationAdmission {
    Free,
    /// A root is retiring only its own transport and keeps its application.
    /// Whether the slot is taken is known once that settles.
    Settling,
    /// An application is starting, live or retained for resume.
    Active,
}

/// Why a launch that could be admitted is held for a moment first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LaunchHold {
    /// An open root is settling its transport.
    Settling,
    /// An ended application is still closing, until this instant.
    Predecessor(Instant),
}

/// Decide the application slot. It receives open records only, so an ended
/// application cannot occupy the slot whatever became of its cleanup.
pub(super) fn application_admission(live: &Live<'_, Record>) -> ApplicationAdmission {
    let mut admission = ApplicationAdmission::Free;
    for record in live.iter().filter(|record| record.auxiliary.is_none()) {
        if !record.state.closing() {
            return ApplicationAdmission::Active;
        }
        admission = ApplicationAdmission::Settling;
    }
    admission
}

impl Controller {
    pub(super) fn application_admission(&self) -> ApplicationAdmission {
        application_admission(&Live::of(self.records.values()))
    }

    /// Record when each record stopped holding the application. Transitions
    /// happen in many places; observing them here keeps one definition.
    pub(super) fn stamp_ended(&mut self, now: Instant) {
        for record in self.records.values_mut() {
            match record.admission() {
                Admission::Open => record.ended_at = None,
                Admission::Ended => {
                    record.ended_at.get_or_insert(now);
                }
            }
        }
    }

    /// The instant until which a new launch still waits for applications that
    /// were asked to end and have not finished closing.
    fn predecessor_release_by(&self) -> Option<Instant> {
        self.records
            .values()
            .filter(|record| {
                record.auxiliary.is_none()
                    && record.admission() == Admission::Ended
                    && !record.retirement().settled()
            })
            .filter_map(|record| record.ended_at)
            .map(|ended| ended + SUCCESSOR_GRACE)
            .max()
    }

    /// Why an admissible launch should wait a moment, if it should. An active
    /// application is not a hold: that launch is refused by its caller.
    pub(super) fn launch_hold(&self, now: Instant) -> Option<LaunchHold> {
        match self.application_admission() {
            ApplicationAdmission::Active => None,
            ApplicationAdmission::Settling => Some(LaunchHold::Settling),
            ApplicationAdmission::Free => self
                .predecessor_release_by()
                .filter(|until| now < *until)
                .map(LaunchHold::Predecessor),
        }
    }

    /// Hold one launch. Its own launch deadline starts only once it is
    /// admitted.
    pub(super) fn defer_launch(
        &mut self,
        connection: u64,
        request: LaunchRequest,
        peer: PeerIdentity,
        response: StreamResponse,
        now: Instant,
    ) -> Option<Reply> {
        if self.deferred_launch.is_some() {
            return Some(private_error(503, "another launch is waiting to start"));
        }
        self.deferred_launch = Some(DeferredLaunch {
            connection,
            request,
            peer,
            response,
            deadline: now + INITIAL_DEADLINE,
            takeover_at: now + SUCCESSOR_GRACE,
        });
        None
    }

    /// Admit the held launch once nothing holds it. It goes through the
    /// ordinary admission path again, so every check sees current state.
    pub(super) fn admit_deferred_launch(
        &mut self,
        now: Instant,
        allowed: &mut impl FnMut(&[u8], crate::wire::Permission) -> bool,
        replies: &mut Vec<(u64, Reply)>,
    ) {
        let Some(deferred) = self.deferred_launch.as_ref() else {
            return;
        };
        match self.launch_hold(now) {
            Some(LaunchHold::Settling) => {
                if now >= deferred.deadline {
                    let connection = deferred.connection;
                    self.deferred_launch = None;
                    replies.push((
                        connection,
                        private_error(503, "previous application is still closing"),
                    ));
                }
                return;
            }
            // An ended application is waited for, never deferred to: past the
            // grace this launch starts while that cleanup carries on.
            Some(LaunchHold::Predecessor(_)) if now < deferred.takeover_at => return,
            Some(LaunchHold::Predecessor(_)) | None => {}
        }
        let mut deferred = self.deferred_launch.take().expect("deferred launch");
        // The wait is over. Admission must not hold this launch again.
        deferred.takeover_at = now;
        self.admitting_deferred = true;
        if !allowed(deferred.peer.certificate(), crate::wire::Permission::Launch) {
            self.admitting_deferred = false;
            replies.push((
                deferred.connection,
                private_error(401, "paired client is no longer trusted"),
            ));
            return;
        }
        let reply = self.launch_for_route(
            deferred.connection,
            deferred.request,
            deferred.peer,
            deferred.response,
            now,
        );
        self.admitting_deferred = false;
        if let Some(reply) = reply {
            replies.push((deferred.connection, reply));
        }
    }

    pub(super) fn drop_deferred_launch(&mut self, connection: u64) {
        if self
            .deferred_launch
            .as_ref()
            .is_some_and(|deferred| deferred.connection == connection)
        {
            self.deferred_launch = None;
        }
    }

    /// Keep space for the next launch. Settled records are pruned first; if
    /// the table is still full, ended records are given up, the most settled
    /// first. Records that hold an application are never dropped, so a full
    /// table of those is the only thing that refuses a launch.
    pub(super) fn keep_launch_capacity(&mut self, send: &mut Send<'_>) {
        self.prune_finished_transports();
        if self.records.len() < MAX_RECORDS {
            return;
        }
        let mut pinned: BTreeSet<u64> = self
            .records
            .values()
            .filter_map(|record| record.application_session)
            .collect();
        pinned.extend(self.job.iter().map(|job| job.record));
        pinned.extend(self.pending_resume.as_ref().map(|pending| pending.record));
        let mut dropped = Vec::new();
        make_room(
            &mut self.records,
            MAX_RECORDS,
            |id, _| !pinned.contains(id),
            |id, record| dropped.push((id, record)),
        );
        for (id, mut record) in dropped {
            // Settle or release each retained broker call before it is
            // dropped, including a cleanup step still in flight. The
            // downstream owners keep their own records of what they hold.
            if let Some(mut job) = self.retiring_jobs.remove(&id) {
                job.call.shutdown(send);
            }
            for (_, call) in &mut record.uncertain {
                call.shutdown(send);
            }
            self.retired_stream_keys
                .insert(Sha256::digest(record.request.remote_input_key).into());
            if let Some(logger) = &self.logger {
                let _ = logger.emit(
                    LogLevel::Warning,
                    "launch",
                    &format!(
                        "record={id} event=record_dropped_for_capacity state={:?} \
                         cleanup_error={}",
                        record.state,
                        record.cleanup_error.unwrap_or("none"),
                    ),
                );
            }
            for connection in [record.cancel_connection, record.launch_connection]
                .into_iter()
                .flatten()
            {
                self.management_replies.push((
                    connection,
                    private_error(503, "stream cleanup is incomplete"),
                ));
            }
        }
    }

    /// Give failed cleanup its automatic retries, then stop it from holding
    /// admission and shutdown.
    pub(super) fn recover_failed_cleanup(&mut self, now: Instant, send: &mut Send<'_>) {
        // Shutdown has no later pass to wait for: whatever is still failed is
        // released now so the host can drain. Process exit is the backstop.
        let stopping = self.quiesce_history_deadline.is_some();
        let failed: Vec<u64> = self
            .records
            .values()
            .filter(|record| record.state == State::CleanupFailed)
            .map(|record| record.id)
            .collect();
        for id in failed {
            if self.has_job(id) {
                continue;
            }
            if !stopping && self.anchors_live_clients(id) {
                // Other clients still stream this application. Its root stays
                // in place until they retire or the application ends.
                continue;
            }
            let record = self.records.get_mut(&id).expect("failed cleanup record");
            let delay = record.automatic_cleanup_retry_delay().filter(|_| !stopping);
            let Some(delay) = delay else {
                self.release_failed_cleanup(id, now, stopping, send);
                continue;
            };
            let due = *record
                .next_automatic_cleanup_retry
                .get_or_insert(now + delay);
            if now < due {
                continue;
            }
            match record.retry_cleanup_automatically(now) {
                Ok(Some(pending)) => record.uncertain.push_front(pending),
                Ok(None) => {}
                // No cleanup identity exists to retry; waiting cannot help.
                Err(_) => self.release_failed_cleanup(id, now, stopping, send),
            }
        }
    }

    fn anchors_live_clients(&self, root: u64) -> bool {
        self.records
            .values()
            .any(|member| member.application_session == Some(root) && !member.state.terminal())
    }

    /// Release what the failed cleanup was holding. A root that only failed
    /// to retire its own transport keeps the application it launched.
    fn release_failed_cleanup(
        &mut self,
        id: u64,
        now: Instant,
        stopping: bool,
        send: &mut Send<'_>,
    ) {
        let record = self.records.get_mut(&id).expect("failed cleanup record");
        // Settle or release each retained broker call before it is dropped.
        for (_, call) in &mut record.uncertain {
            call.shutdown(send);
        }
        let error = record.cleanup_error.unwrap_or("cleanup_unconfirmed");
        let retained = !stopping && record.retains_application();
        let cancel_connection = record.cancel_connection.take();
        let launch_connection = record.launch_connection.take();
        if retained {
            record.abandon_transport(now);
        } else {
            record.abandon_cleanup();
        }
        if let Some(logger) = &self.logger {
            let _ = logger.emit(
                LogLevel::Warning,
                "launch",
                &format!(
                    "record={id} event={} cleanup_error={error}",
                    if retained {
                        "transport_cleanup_abandoned_application_retained"
                    } else {
                        "cleanup_abandoned"
                    }
                ),
            );
        }
        if let Some(connection) = cancel_connection {
            self.management_replies.push((
                connection,
                private_error(503, "stream cleanup is incomplete"),
            ));
        }
        if let Some(connection) = launch_connection {
            self.management_replies
                .push((connection, private_error(503, "stream launch failed")));
        }
    }

    /// Write each record's failure and cleanup codes to the log once per
    /// change. The codes are fixed internal identifiers; request contents and
    /// client identities are excluded.
    pub(super) fn report_outcomes(&mut self) {
        let Some(logger) = &self.logger else {
            return;
        };
        for record in self.records.values_mut() {
            let outcome = (record.state, record.failure, record.cleanup_error);
            if record.reported_outcome == Some(outcome) {
                continue;
            }
            record.reported_outcome = Some(outcome);
            if !reportable(record) {
                continue;
            }
            let level = if matches!(record.state, State::CleanupFailed | State::CleanupAbandoned) {
                LogLevel::Warning
            } else {
                LogLevel::Info
            };
            let _ = logger.emit(
                level,
                "launch",
                &format!(
                    "record={} state={:?} stage={:?} failure={} cleanup_error={} \
                     automatic_cleanup_retries={}",
                    record.id,
                    record.state,
                    record.stage,
                    record.failure.unwrap_or("none"),
                    record.cleanup_error.unwrap_or("none"),
                    record.automatic_cleanup_retries,
                ),
            );
        }
    }
}

/// A record is worth a log line when it carries a failure or cleanup code, or
/// when its cleanup was abandoned.
fn reportable(record: &Record) -> bool {
    record.failure.is_some()
        || record.cleanup_error.is_some()
        || record.state == State::CleanupAbandoned
}
