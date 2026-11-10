//! Keeps an earlier launch's cleanup from deciding whether a later one starts.
//!
//! Failed cleanup is retried on its own, then abandoned. A launch that arrives
//! while an earlier application is still closing waits for that cleanup
//! instead of being refused. Unconfirmed cleanup is never reported as stopped:
//! an abandoned record keeps its `cleanup_error` and says so to management.
use super::{
    Controller, INITIAL_DEADLINE, LaunchRequest, PeerIdentity, Reply, Send, StreamResponse,
    private_error,
    record::{Record, State},
};
use amaterasu_module_sdk::LogLevel;
use std::time::Instant;

/// A paired launch held while an earlier application finishes closing.
pub(super) struct DeferredLaunch {
    pub(super) connection: u64,
    pub(super) request: LaunchRequest,
    pub(super) peer: PeerIdentity,
    pub(super) response: StreamResponse,
    pub(super) deadline: Instant,
}

/// Whether the host's single application slot can take a new root launch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ApplicationAdmission {
    Free,
    /// Every earlier root is closing; the slot frees without client action.
    Closing,
    /// An earlier application is starting, live or retained for resume.
    Active,
}

impl Controller {
    pub(super) fn application_admission(&self) -> ApplicationAdmission {
        let mut admission = ApplicationAdmission::Free;
        for record in self
            .records
            .values()
            .filter(|record| record.auxiliary.is_none() && !record.state.terminal())
        {
            if !record.state.closing() {
                return ApplicationAdmission::Active;
            }
            admission = ApplicationAdmission::Closing;
        }
        admission
    }

    /// Hold one launch until the closing application settles. Its own launch
    /// deadline starts only once it is admitted.
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
        });
        None
    }

    /// Admit the held launch once nothing is closing. It goes through the
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
        let admission = self.application_admission();
        if admission == ApplicationAdmission::Closing {
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
        let deferred = self.deferred_launch.take().expect("deferred launch");
        if !allowed(deferred.peer.certificate(), crate::wire::Permission::Launch) {
            replies.push((
                deferred.connection,
                private_error(401, "paired client is no longer trusted"),
            ));
            return;
        }
        if let Some(reply) = self.launch_for_route(
            deferred.connection,
            deferred.request,
            deferred.peer,
            deferred.response,
            now,
        ) {
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
            if self.job.as_ref().is_some_and(|job| job.record == id) {
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
