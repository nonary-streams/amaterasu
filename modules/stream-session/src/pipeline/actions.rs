use super::*;
use crate::model::{PrepareConfig, StartConfig, valid_name};

#[derive(Clone, Copy, PartialEq, Eq)]
enum StartMode {
    Immediate,
    WhenAuthenticated,
}

struct Incoming {
    request: u64,
    owner: String,
    now: u64,
    remaining: u64,
}

impl Pipeline {
    pub(super) fn receive(
        &mut self,
        request: u64,
        owner: String,
        operation: Operation,
        now: u64,
        remaining: u64,
        send: &mut Send<'_>,
    ) {
        let opens_session = matches!(
            &operation,
            Operation::Prepare(_)
                | Operation::Start { .. }
                | Operation::StartWhenAuthenticated { .. }
        );
        if opens_session
            && (!self.journal.loaded
                || self.journal.failed
                || !self.recovery_ready
                || self.recovery_blocked)
        {
            rpc::complete(send, request, error("restart_recovery_unconfirmed"));
            return;
        }
        if matches!(
            operation,
            Operation::Prepare(_)
                | Operation::Start { .. }
                | Operation::StartWhenAuthenticated { .. }
        ) && let Err(code) = self.authorize_open(&owner)
        {
            rpc::complete(send, request, error(code));
            return;
        }
        let input = Incoming {
            request,
            owner,
            now,
            remaining,
        };
        match operation {
            Operation::ReserveHistoryActivity {
                activity_id,
                rtsp_operation_id,
            } => {
                if input.owner != "amaterasu.nvhttp" || !self.settings.enabled {
                    rpc::complete(send, input.request, error("permission_denied"));
                } else if !valid_name(&activity_id) || activity_id != rtsp_operation_id {
                    rpc::complete(send, input.request, error("invalid_history_activity"));
                } else {
                    self.history_activities.reserve(
                        input.request,
                        activity_id,
                        rtsp_operation_id,
                        input.now,
                        send,
                    );
                }
            }
            Operation::ReleaseHistoryActivity {
                activity_id,
                rtsp_operation_id,
            } => {
                if input.owner != "amaterasu.nvhttp" || !self.settings.enabled {
                    rpc::complete(send, input.request, error("permission_denied"));
                } else if !valid_name(&activity_id) || activity_id != rtsp_operation_id {
                    rpc::complete(send, input.request, error("invalid_history_activity"));
                } else {
                    self.history_activities.release(
                        input.request,
                        &activity_id,
                        &rtsp_operation_id,
                        input.now,
                        &self.records,
                        send,
                    );
                }
            }
            Operation::Prepare(config) => self.prepare(input, *config, send),
            Operation::Start {
                session,
                configuration,
            } => self.start(input, session, configuration, StartMode::Immediate, send),
            Operation::StartWhenAuthenticated {
                session,
                configuration,
            } => self.start(
                input,
                session,
                configuration,
                StartMode::WhenAuthenticated,
                send,
            ),
            Operation::Stop { session } | Operation::Close { session } => {
                self.stop(input, session, send)
            }
            Operation::Status { session } => self.status(input, session, send),
            Operation::Lookup { operation_id } => self.lookup(input, operation_id, send),
        }
    }

    fn authorize_open(&self, owner: &str) -> Result<(), &'static str> {
        if !self.settings.enabled {
            return Err("disabled");
        }
        if !self
            .settings
            .allowed_principals
            .iter()
            .any(|allowed| allowed == owner)
        {
            return Err("permission_denied");
        }
        Ok(())
    }

    fn prepare(&mut self, input: Incoming, config: PrepareConfig, send: &mut Send<'_>) {
        let Incoming {
            request,
            owner,
            now,
            remaining,
        } = input;

        if let Err(code) = config.validate(self.settings.allow_remote) {
            rpc::complete(send, request, error(code));
            return;
        }
        if let Some(record) = self.records.values().find(|record| {
            record.owner == owner && record.config.operation_id == config.operation_id
        }) {
            let reply = if record.config == config {
                record.response()
            } else {
                error("operation_conflict")
            };
            rpc::complete(send, request, reply);
            return;
        }
        if let (Some(activity_id), Some(rtsp_operation_id)) =
            (&config.history_activity_id, &config.rtsp_operation_id)
        {
            if owner != "amaterasu.rtsp" {
                rpc::complete(send, request, error("permission_denied"));
                return;
            }
            if !self
                .history_activities
                .matches(activity_id, rtsp_operation_id, now)
            {
                rpc::complete(send, request, error("history_activity_unavailable"));
                return;
            }
        }
        if self.records.len() >= 128
            || self
                .records
                .values()
                .filter(|record| !record.state.terminal())
                .count()
                >= 2
        {
            rpc::complete(send, request, error("capacity_exceeded"));
            return;
        }
        let id = match self.new_id() {
            Ok(id) => id,
            Err(code) => {
                rpc::complete(send, request, error(code));
                return;
            }
        };
        self.records.insert(
            id,
            Record::new(id, owner, config, self.settings.input_lease_ttl_ms),
        );
        self.job = Some(Job::new(
            id,
            Some(request),
            Step::Open(Resource::Session),
            now.saturating_add(remaining.min(self.settings.prepare_timeout_ms)),
        ));
    }

    fn start(
        &mut self,
        input: Incoming,
        session: u64,
        configuration: StartConfig,
        mode: StartMode,
        send: &mut Send<'_>,
    ) {
        let Incoming {
            request,
            owner,
            now,
            remaining,
        } = input;

        let Some(record) = self
            .records
            .get_mut(&session)
            .filter(|record| record.owner == owner)
        else {
            rpc::complete(send, request, error("session_not_found"));
            return;
        };
        if let Err(code) = configuration.validate(record.config.stream_audio) {
            rpc::complete(send, request, error(code));
            return;
        }
        if !configuration.matches_prepared_audio(&record.config) {
            rpc::complete(send, request, error("audio_configuration_mismatch"));
            return;
        }
        if !configuration.matches_prepared_codec(&record.config) {
            rpc::complete(send, request, error("video_codec_mismatch"));
            return;
        }
        if !configuration.matches_prepared_frame_limiter(&record.config) {
            rpc::complete(send, request, error("frame_limiter_configuration_mismatch"));
            return;
        }
        if !configuration.matches_prepared_encryption(&record.config) {
            rpc::complete(send, request, error("encryption_configuration_mismatch"));
            return;
        }
        if record
            .start
            .as_ref()
            .is_some_and(|existing| existing != &configuration)
        {
            rpc::complete(send, request, error("start_conflict"));
            return;
        }
        if record.state == State::Active
            || (mode == StartMode::WhenAuthenticated && record.armed_accepted)
        {
            rpc::complete(send, request, record.response());
            return;
        }
        if record.state != State::Prepared {
            rpc::complete(send, request, error("invalid_state"));
            return;
        }
        record.start = Some(configuration);
        if mode == StartMode::WhenAuthenticated {
            Self::arm(record, request, now, send);
            return;
        }
        record.state = State::Starting;
        self.job = Some(Job::new(
            session,
            Some(request),
            Step::Authenticate,
            now + remaining.min(15000),
        ));
    }

    fn arm(record: &mut Record, request: u64, now: u64, send: &mut Send<'_>) {
        if !rpc::active(send, request) {
            record.state = State::Stopping;
            record.outcome = State::Cancelled;
            record.error = Some("request_cancelled");
            return;
        }
        record.state = State::Armed;
        if record.config.display_prepare_timeout_ms.is_some() {
            record.prepared_until = now.saturating_add(u64::from(record.config.ping_timeout_ms));
        }
        if rpc::complete(send, request, record.response()) {
            record.armed_accepted = true;
            record.next_auth_poll = now;
        } else {
            record.state = State::Stopping;
            record.outcome = State::Cancelled;
            record.error = Some("completion_unavailable");
        }
    }

    fn stop(&mut self, input: Incoming, session: u64, send: &mut Send<'_>) {
        let Incoming {
            request,
            owner,
            now,
            ..
        } = input;

        let Some(record) = self
            .records
            .get_mut(&session)
            .filter(|record| record.owner == owner)
        else {
            rpc::complete(send, request, error("session_not_found"));
            return;
        };
        if record.state.terminal() {
            rpc::complete(send, request, record.response());
            return;
        }
        let mut job = Job::new(
            session,
            Some(request),
            if record.input_lookup_required
                || (record.input_attempted && record.input_lease.is_none())
            {
                Step::FindInput
            } else if record.input_lease.is_some() {
                Step::ReleaseInput
            } else {
                Step::DrainVideo
            },
            now + self.settings.drain_timeout_ms,
        );
        record.outcome = State::Stopped;
        record.history.stopping("stop_requested");
        record.state = State::Stopping;
        record.cleanup_error = None;
        input::queue_rumble_zeros(record, now);
        if !record.rumble_pending.is_empty() {
            job.step = Step::RumbleDrain;
        }
        if let Some(plane) = record.plane.as_mut() {
            plane.stop();
        } else {
            job.cleanup = true;
            job.deadline = now + 15000;
            if let Some(mut pending) = record.uncertain.take() {
                pending.retry();
                pending.cancel();
                job.pending = Some(pending);
                job.settling_allocation = true;
            }
            if let Some(step) = job.next_cleanup(record) {
                job.step = step;
            }
        }
        self.job = Some(job);
    }

    fn status(&mut self, input: Incoming, session: Option<u64>, send: &mut Send<'_>) {
        let Incoming { request, owner, .. } = input;

        let reply = if let Some(id) = session {
            self.records
                .get(&id)
                .filter(|record| record.owner == owner)
                .map(Record::response)
                .unwrap_or_else(|| error("session_not_found"))
        } else {
            let owned: Vec<_> = self
                .records
                .values()
                .filter(|record| record.owner == owner)
                .collect();
            let visible: Vec<_> = owned
                .iter()
                .filter(|record| !record.state.terminal())
                .chain(owned.iter().filter(|record| record.state.terminal()))
                .take(16)
                .map(|record| record.response())
                .collect();
            json!({"ok":true,"enabled":self.settings.enabled,"sessions":visible,"total":owned.len(),"truncated":owned.len()>16})
        };
        rpc::complete(send, request, reply);
    }

    fn lookup(&mut self, input: Incoming, operation_id: String, send: &mut Send<'_>) {
        let Incoming { request, owner, .. } = input;

        let reply = self
            .records
            .values()
            .find(|record| record.owner == owner && record.config.operation_id == operation_id)
            .map(Record::response)
            .unwrap_or_else(|| error("session_not_found"));
        rpc::complete(send, request, reply);
    }
}
