use super::admission::{MAX_RECORDS, session_admitted};
use super::*;
use crate::model::{PrepareConfig, StartConfig, valid_name};
use amaterasu_module_sdk::lifecycle_ledger::Live;

#[derive(Clone, Copy, PartialEq, Eq)]
enum StartMode {
    Immediate,
    WhenAuthenticated,
    BeforePlay,
}

pub(super) struct RequestCaller {
    pub(super) principal: String,
    pub(super) approved_api: bool,
}

struct Incoming {
    request: u64,
    owner: String,
    approved_api: bool,
    now: u64,
    remaining: u64,
}

impl Pipeline {
    pub(super) fn receive(
        &mut self,
        request: u64,
        caller: RequestCaller,
        operation: Operation,
        now: u64,
        remaining: u64,
        send: &mut Send<'_>,
    ) {
        let RequestCaller {
            principal: owner,
            approved_api,
        } = caller;
        let opens_session = matches!(
            &operation,
            Operation::Prepare(_)
                | Operation::Start { .. }
                | Operation::StartWhenAuthenticated { .. }
        );
        if opens_session && self.shutdown_admission_closed() {
            self.report_open_rejection(&owner, &operation, "host_stopping");
            rpc::complete(send, request, error("host_stopping"));
            return;
        }
        if opens_session && !self.admission_ready() {
            self.report_open_rejection(&owner, &operation, "restart_recovery_unconfirmed");
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
            self.report_open_rejection(&owner, &operation, code);
            rpc::complete(send, request, error(code));
            return;
        }
        let input = Incoming {
            request,
            owner,
            approved_api,
            now,
            remaining,
        };
        match operation {
            Operation::ReserveHistoryActivity {
                activity_id,
                rtsp_operation_id,
            } => {
                if !self.settings.history_owners.contains(&input.owner) || !self.settings.enabled {
                    rpc::complete(send, input.request, error("permission_denied"));
                } else if self.shutdown_admission_closed()
                    && !self.history_activities.contains(&activity_id)
                {
                    rpc::complete(send, input.request, error("host_stopping"));
                } else if self.package_barrier_pending
                    && !self.history_activities.contains(&activity_id)
                {
                    rpc::complete(send, input.request, error("package_activation_pending"));
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
                if !self.settings.history_owners.contains(&input.owner) || !self.settings.enabled {
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
            Operation::RecordHistoryWarnings {
                activity_id,
                rtsp_operation_id,
                warnings,
            } => {
                let warning_request =
                    amaterasu_module_sdk::stream_warnings::RecordHistoryWarnings {
                        activity_id,
                        rtsp_operation_id,
                        warnings,
                    };
                if !self.settings.history_owners.contains(&input.owner) || !self.settings.enabled {
                    rpc::complete(send, input.request, error("permission_denied"));
                } else if !warning_request.valid() {
                    rpc::complete(send, input.request, error("invalid_history_warnings"));
                } else if let Err(code) = self.history_activities.record_warnings(
                    &warning_request.activity_id,
                    &warning_request.rtsp_operation_id,
                    &warning_request.warnings,
                ) {
                    rpc::complete(send, input.request, error(code));
                } else {
                    if let Some(record) = self.records.values_mut().find(|record| {
                        record.state == State::Active
                            && record.config.history_activity_id.as_deref()
                                == Some(warning_request.activity_id.as_str())
                    }) {
                        let warnings = self
                            .history_activities
                            .take_warnings(&warning_request.activity_id);
                        record.history.record_warnings(&warnings);
                    }
                    rpc::complete(send, input.request, json!({"ok":true}));
                }
            }
            Operation::ReleasePausedDisplay { application } => self.release_paused_display(
                input.request,
                &input.owner,
                application,
                input.now,
                send,
            ),
            Operation::Prepare(config) => self.prepare(input, *config, send),
            Operation::Start {
                session,
                configuration,
            } => self.start(input, session, configuration, StartMode::Immediate, send),
            Operation::StartWhenAuthenticated {
                session,
                configuration,
                prepare_video_before_play,
            } => self.start(
                input,
                session,
                configuration,
                if prepare_video_before_play {
                    StartMode::BeforePlay
                } else {
                    StartMode::WhenAuthenticated
                },
                send,
            ),
            Operation::ReconfigureAudio { session, audio } => {
                self.reconfigure_audio(input, session, *audio, send)
            }
            Operation::Stop {
                session,
                lifecycle_configuration,
            } => self.stop(input, session, lifecycle_configuration, send),
            Operation::Close { session } => self.stop(input, session, None, send),
            Operation::Status { session } => self.status(input, session, send),
            Operation::ManagementStopStream { session_id } => {
                self.management_stop_stream(input, &session_id, send)
            }
            Operation::ManagementLiveSessions {} => self.management_live_sessions(input, send),
            Operation::Lookup { operation_id } => self.lookup(input, operation_id, send),
        }
    }

    fn report_open_rejection(&mut self, owner: &str, operation: &Operation, code: &str) {
        match operation {
            Operation::Prepare(config) => {
                self.report_prepare_refusal(owner, config.rtsp_operation_id.as_deref(), code);
            }
            Operation::Start { session, .. }
            | Operation::StartWhenAuthenticated { session, .. } => {
                if let Some(record) = self
                    .records
                    .get_mut(session)
                    .filter(|record| record.owner == owner)
                {
                    record.report_request_warning("start", code);
                }
            }
            _ => {}
        }
    }

    fn report_owned_start_rejection(&mut self, owner: &str, session: u64, code: &str) {
        if let Some(record) = self
            .records
            .get_mut(&session)
            .filter(|record| record.owner == owner)
        {
            record.report_request_warning("start", code);
        }
    }

    fn reconfigure_audio(
        &mut self,
        input: Incoming,
        session: u64,
        audio: crate::model::AudioWire,
        send: &mut Send<'_>,
    ) {
        let outstanding_reads: usize = self
            .records
            .values()
            .map(|record| record.shutdown_observations.len())
            .sum();
        let Some(record) = self
            .records
            .get_mut(&session)
            .filter(|record| record.owner == input.owner)
        else {
            rpc::complete(send, input.request, error("session_not_found"));
            return;
        };
        if record.state != State::Prepared || !record.config.stream_audio {
            record.report_request_warning("reconfigure_audio", "invalid_state");
            rpc::complete(send, input.request, error("invalid_state"));
            return;
        }
        if outstanding_reads >= MAX_DEFERRED_READ_SETTLEMENTS {
            record.report_request_warning("reconfigure_audio", "audio_probe_settlement_pending");
            rpc::complete(send, input.request, error("audio_probe_settlement_pending"));
            return;
        }
        let (Some(old), Some(new)) = (&record.config.audio.custom, &audio.custom) else {
            record.report_request_warning("reconfigure_audio", "unsupported_audio_reconfiguration");
            rpc::complete(
                send,
                input.request,
                error("unsupported_audio_reconfiguration"),
            );
            return;
        };
        if audio.validate().is_err()
            || new.source != old.source
            || new.processing.is_some() != old.processing.is_some()
            || new.stream.generation <= old.stream.generation
        {
            record.report_request_warning("reconfigure_audio", "invalid_audio_reconfiguration");
            rpc::complete(send, input.request, error("invalid_audio_reconfiguration"));
            return;
        }
        record.pending_audio = Some(audio);
        self.job = Some(Job::new(
            session,
            Some(input.request),
            Step::ProbeAudioReconfiguration(Resource::AudioCapture),
            input.now.saturating_add(input.remaining.min(15000)),
        ));
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
            ..
        } = input;

        if let Err(code) = config.validate(self.settings.allow_remote) {
            self.report_prepare_refusal(&owner, config.rtsp_operation_id.as_deref(), code);
            rpc::complete(send, request, error(code));
            return;
        }
        if config.lifecycle_session_target.is_some() && self.settings.providers.lifecycle.is_none()
        {
            self.report_prepare_refusal(
                &owner,
                config.rtsp_operation_id.as_deref(),
                "lifecycle_provider_not_selected",
            );
            rpc::complete(send, request, error("lifecycle_provider_not_selected"));
            return;
        }
        if config
            .audio
            .custom
            .as_ref()
            .is_some_and(|custom| custom.processing.is_some())
            && self.settings.providers.audio_processor.is_none()
        {
            self.report_prepare_refusal(
                &owner,
                config.rtsp_operation_id.as_deref(),
                "audio_processor_not_selected",
            );
            rpc::complete(send, request, error("audio_processor_not_selected"));
            return;
        }
        let lifecycle_admitter = self.settings.lifecycle_admitters.contains(&owner);
        if config.lifecycle_session_target.is_some() && !lifecycle_admitter {
            rpc::complete(send, request, error("permission_denied"));
            return;
        }
        if config.rtsp_operation_id.is_some() && !lifecycle_admitter {
            rpc::complete(send, request, error("permission_denied"));
            return;
        }
        if (config.borrowed_capture.is_some() || config.extension_probe.is_some())
            && !lifecycle_admitter
        {
            rpc::complete(send, request, error("permission_denied"));
            return;
        }
        if let Some(record) = self.records.values_mut().find(|record| {
            record.owner == owner && record.config.operation_id == config.operation_id
        }) {
            let reply = if record.config == config {
                record.response()
            } else {
                record.report_request_warning("prepare", "operation_conflict");
                error("operation_conflict")
            };
            rpc::complete(send, request, reply);
            return;
        }
        // Existing operations may finish or retry while no new stream is admitted.
        if self.package_barrier_pending {
            self.report_prepare_refusal(
                &owner,
                config.rtsp_operation_id.as_deref(),
                "package_activation_pending",
            );
            rpc::complete(send, request, error("package_activation_pending"));
            return;
        }
        if let (Some(activity_id), Some(rtsp_operation_id)) =
            (&config.history_activity_id, &config.rtsp_operation_id)
        {
            if !lifecycle_admitter {
                rpc::complete(send, request, error("permission_denied"));
                return;
            }
            if !self
                .history_activities
                .matches(activity_id, rtsp_operation_id, now)
            {
                self.report_prepare_refusal(
                    &owner,
                    config.rtsp_operation_id.as_deref(),
                    "history_activity_unavailable",
                );
                rpc::complete(send, request, error("history_activity_unavailable"));
                return;
            }
        }
        self.keep_session_capacity();
        if self.records.len() >= MAX_RECORDS
            || !session_admitted(
                &Live::of(self.records.values()),
                self.settings.max_live_sessions,
            )
        {
            self.report_prepare_refusal(
                &owner,
                config.rtsp_operation_id.as_deref(),
                "capacity_exceeded",
            );
            rpc::complete(send, request, error("capacity_exceeded"));
            return;
        }
        let id = match self.new_id() {
            Ok(id) => id,
            Err(code) => {
                self.report_prepare_refusal(&owner, config.rtsp_operation_id.as_deref(), code);
                rpc::complete(send, request, error(code));
                return;
            }
        };
        let lifecycle_input_required = config
            .lifecycle_session_target
            .as_ref()
            .map(|_| self.settings.enable_input);
        let shared_capture = match super::shared_capture::borrow_source(
            &self.records,
            &owner,
            lifecycle_admitter,
            &config,
        ) {
            Ok(source) => source,
            Err(code) => {
                self.report_prepare_refusal(&owner, config.rtsp_operation_id.as_deref(), code);
                rpc::complete(send, request, error(code));
                return;
            }
        };
        let mut record = Record::new(id, owner, config, self.settings.input_lease_ttl_ms);
        record.shared_capture_attached = shared_capture.is_some();
        record.virtual_display_attempted = shared_capture.is_some();
        record.shared_capture = shared_capture;
        record.providers = self.settings.providers.clone();
        record.processing_plan_provider = self.settings.processing_plan.clone();
        record.source_preparation = self.settings.source_preparation.clone();
        if record.config.extension_probe.is_some()
            && (self
                .settings
                .extension_route
                .as_ref()
                .is_some_and(|route| route.submitted)
                || (record.config.display_request.is_none()
                    && record.config.virtual_display_intent.is_none()
                    && record.config.lifecycle_session_target.is_none()))
        {
            record.extension_route = self.settings.extension_route.as_ref().and_then(|route| {
                let probe = record.config.extension_probe.as_ref()?;
                route.for_format(&probe.video_stream_format)
            });
        }
        record.lifecycle_input_required = lifecycle_input_required;
        self.records.insert(id, record);
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
        mut configuration: StartConfig,
        mode: StartMode,
        send: &mut Send<'_>,
    ) {
        let Incoming {
            request,
            owner,
            now,
            remaining,
            ..
        } = input;

        if mode == StartMode::BeforePlay
            && (owner != "amaterasu.rtsp" || !self.settings.lifecycle_admitters.contains(&owner))
        {
            self.report_owned_start_rejection(&owner, session, "permission_denied");
            rpc::complete(send, request, error("permission_denied"));
            return;
        }
        let lifecycle_admitter = self.settings.lifecycle_admitters.contains(&owner);
        if (configuration.client_feature_flags.is_some()
            || !configuration.stream_adjustments.is_empty())
            && !lifecycle_admitter
        {
            self.report_owned_start_rejection(&owner, session, "permission_denied");
            rpc::complete(send, request, error("permission_denied"));
            return;
        }
        if let Some(agreement) = &configuration.outbound_agreement
            && self
                .settings
                .outbound_policy
                .as_ref()
                .is_none_or(|policy| policy.authority != owner || !policy.approves(agreement))
        {
            self.report_owned_start_rejection(&owner, session, "permission_denied");
            rpc::complete(send, request, error("permission_denied"));
            return;
        }

        let Some(record) = self
            .records
            .get_mut(&session)
            .filter(|record| record.owner == owner)
        else {
            rpc::complete(send, request, error("session_not_found"));
            return;
        };
        if configuration.configuration.extension_profile.is_none()
            && configuration.extension_wire_bitrate_kbps.is_some()
        {
            rpc::complete(send, request, error("invalid_extension_wire_budget"));
            return;
        }
        if let Err(code) = resolve_extension_start(record, &mut configuration) {
            record.report_request_warning("start", code);
            rpc::complete(send, request, error(code));
            return;
        }
        if let Err(code) =
            validate_start_configuration(lifecycle_admitter, record, &mut configuration)
        {
            record.report_request_warning("start", code);
            rpc::complete(send, request, error(code));
            return;
        }
        if mode != StartMode::Immediate
            && record.start.is_some()
            && record.video_warmup_requested != (mode == StartMode::BeforePlay)
        {
            record.report_request_warning("start", "start_configuration_conflict");
            rpc::complete(send, request, error("start_configuration_conflict"));
            return;
        }
        if record.state == State::Active || (mode != StartMode::Immediate && record.armed_accepted)
        {
            rpc::complete(send, request, record.response());
            return;
        }
        if record.state != State::Prepared {
            record.report_request_warning("start", "invalid_state");
            rpc::complete(send, request, error("invalid_state"));
            return;
        }
        record.start = Some(configuration);
        if mode == StartMode::BeforePlay {
            // RTSP has authenticated the client and supplied its negotiated
            // configuration. Warm only source output; no input lease, native
            // media plane or remote destination is activated before PLAY.
            record.video_warmup_requested = true;
            record.video_warmup_started_at = now;
            eprintln!("amaterasu source warmup started session={session}");
            record.state = State::Starting;
            record.video_readiness_next_poll_at = now;
            self.job = Some(Job::new(
                session,
                Some(request),
                if record
                    .start
                    .as_ref()
                    .is_some_and(|start| start.configuration.extension_profile.is_some())
                {
                    Step::AcquireOutput
                } else {
                    Step::ConfigureMediaVideoFormat
                },
                now.saturating_add(remaining.min(10_000)),
            ));
            return;
        }
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

    pub(super) fn arm(record: &mut Record, request: u64, now: u64, send: &mut Send<'_>) {
        if !rpc::active(send, request) {
            record.state = State::Stopping;
            record.outcome = State::Cancelled;
            record.error = Some("request_cancelled");
            return;
        }
        record.state = State::Armed;
        if record.config.display_prepare_timeout_ms.is_some() || record.video_warmup_requested {
            record.prepared_until = now.saturating_add(u64::from(record.config.ping_timeout_ms));
        }
        if rpc::complete(send, request, record.response()) {
            record.armed_accepted = true;
            record.next_auth_poll = now;
        } else {
            record.report_failure(
                amaterasu_module_sdk::stream_lifecycle::LifecycleLayer::StreamSession,
                "arm",
                "completion_unavailable",
            );
            record.state = State::Stopping;
            record.outcome = State::Cancelled;
            record.error = Some("completion_unavailable");
        }
    }

    fn stop(
        &mut self,
        input: Incoming,
        session: u64,
        lifecycle_configuration: Option<Box<StartConfig>>,
        send: &mut Send<'_>,
    ) {
        let Incoming {
            request,
            owner,
            now,
            ..
        } = input;

        let lifecycle_admitter = self.settings.lifecycle_admitters.contains(&owner);
        let startup_reconciled = self.startup_reconciled();
        let Some(record) = self
            .records
            .get_mut(&session)
            .filter(|record| record.owner == owner)
        else {
            let mut response = error("session_not_found");
            response["startup_reconciled"] = json!(startup_reconciled);
            rpc::complete(send, request, response);
            return;
        };
        let mut adopted_lifecycle = false;
        if let Some(mut configuration) = lifecycle_configuration {
            if !lifecycle_admitter || configuration.lifecycle_target.is_none() {
                rpc::complete(send, request, error("permission_denied"));
                return;
            }
            if let Err(code) =
                validate_stop_configuration(lifecycle_admitter, record, &mut configuration)
            {
                rpc::complete(send, request, error(code));
                return;
            }
            if record.start.is_none() {
                // Accept cleanup ownership without arming or opening capture.
                // The journal must commit before any lifecycle mutation below.
                record.start = Some(*configuration);
                adopted_lifecycle = true;
            }
        }
        if record.state.terminal() && !adopted_lifecycle {
            rpc::complete(send, request, record.response());
            return;
        }
        let persist_lifecycle_identity = record
            .start
            .as_ref()
            .is_some_and(|start| start.lifecycle_target.is_some())
            && !self.journal.matches_lifecycle_identity(record);
        if persist_lifecycle_identity && self.journal.failed {
            // A failed or unconfirmed write requires authoritative journal
            // reconciliation. A retry must not fence a target held only in RAM,
            // or race a late write using a new operation identity.
            let code = record
                .cleanup_error
                .unwrap_or("lifecycle_journal_write_unconfirmed");
            record.fail_cleanup(
                amaterasu_module_sdk::stream_lifecycle::LifecycleLayer::StreamSession,
                "persist_journal",
                code,
            );
            self.journal_waiting_stops.insert(record.id);
            rpc::complete(send, request, record.response());
            return;
        }
        if let Some(wait) = record.video_warmup_wait.take() {
            rpc::complete(send, wait.request, error("request_cancelled"));
        }
        let job = match prepare_stop_job(
            record,
            &self.journal,
            self.sequence,
            Some(request),
            now,
            self.settings.drain_timeout_ms,
        ) {
            Ok(job) => job,
            Err(_) => {
                self.journal.failed = true;
                record.fail_cleanup(
                    amaterasu_module_sdk::stream_lifecycle::LifecycleLayer::StreamSession,
                    "persist_journal",
                    "lifecycle_journal_write_failed",
                );
                rpc::complete(send, request, record.response());
                return;
            }
        };
        self.job = Some(job);
    }

    fn status(&mut self, input: Incoming, session: Option<u64>, send: &mut Send<'_>) {
        let Incoming { request, owner, .. } = input;

        let mut reply = if let Some(id) = session {
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
                .take(32)
                .map(|record| record.response())
                .collect();
            json!({"ok":true,"enabled":self.settings.enabled,"sessions":visible,"total":owned.len(),"truncated":owned.len()>32})
        };
        reply["startup_reconciled"] = json!(self.startup_reconciled());
        rpc::complete(send, request, reply);
    }

    fn management_stop_stream(
        &mut self,
        mut input: Incoming,
        session_id: &str,
        send: &mut Send<'_>,
    ) {
        if input.owner != "amaterasu.management" && !input.approved_api {
            rpc::complete(send, input.request, error("permission_denied"));
            return;
        }
        if session_id.is_empty()
            || session_id.len() > 128
            || session_id.chars().any(char::is_control)
        {
            rpc::complete(send, input.request, error("invalid_session_id"));
            return;
        }
        let selected = self.records.values().find(|record| {
            matches!(record.state, State::Active | State::Stopping)
                && record.history.live_identity().0 == session_id
        });
        let Some(record) = selected else {
            rpc::complete(send, input.request, error("session_not_found"));
            return;
        };
        let id = record.id;
        // This explicit management operation authorizes stopping a particular
        // visible stream. Cleanup retains that stream's original transport owner.
        input.owner = record.owner.clone();
        self.stop(input, id, None, send);
    }

    fn management_live_sessions(&mut self, input: Incoming, send: &mut Send<'_>) {
        if input.owner != "amaterasu.management" && !input.approved_api {
            rpc::complete(send, input.request, error("permission_denied"));
            return;
        }
        if self.live_status_epoch.is_none() {
            let mut bytes = [0_u8; 16];
            if getrandom::fill(&mut bytes).is_err() {
                rpc::complete(send, input.request, error("live_status_unavailable"));
                return;
            }
            self.live_status_epoch = Some(bytes.iter().map(|byte| format!("{byte:02x}")).collect());
        }
        let sessions: Result<Vec<_>, _> = self
            .records
            .values()
            .filter(|record| record.state == State::Active)
            .map(|record| {
                let start = record.start.as_ref().ok_or(())?;
                let (session_id, started_at_unix) = record.history.live_identity();
                let (frames, bytes, gaps) = record.history.observed_counters();
                Ok(json!({
                    "session_id": session_id,
                    "started_at_unix": started_at_unix.ok_or(())?,
                    "device_name": start.device_name,
                    "width": start.configuration.width,
                    "height": start.configuration.height,
                    "target_fps": start.configuration.fps,
                    "target_bitrate_kbps": start.configuration.bitrate / 1000,
                    "codec": start.configuration.codec,
                    "frames_sent": frames.to_string(),
                    "bytes_sent": bytes.to_string(),
                    "sequence_gaps": gaps.to_string(),
                    "encode_timing": record.history.live_encode_timing(),
                }))
            })
            .collect();
        match sessions {
            Ok(sessions) => rpc::complete(
                send,
                input.request,
                json!({"ok":true,"epoch":self.live_status_epoch,"sampled_at_ms":input.now.to_string(),
                    "sampled_at_unix_ms":unix_time_millis().to_string(),"sessions":sessions}),
            ),
            Err(()) => rpc::complete(send, input.request, error("live_status_unavailable")),
        };
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

fn unix_time_millis() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => i64::try_from(duration.as_millis()).unwrap_or(i64::MAX),
        Err(error) => -i64::try_from(error.duration().as_millis()).unwrap_or(i64::MAX),
    }
}

/// Translate an RTSP-selected extension into the approved native encoder route.
fn resolve_extension_start(
    record: &Record,
    configuration: &mut StartConfig,
) -> Result<(), &'static str> {
    let Some(profile) = configuration.configuration.extension_profile.as_ref() else {
        return Ok(());
    };
    let route = record
        .extension_route
        .as_ref()
        .filter(|route| {
            record.owner == "amaterasu.rtsp"
                && configuration.client_feature_flags.is_some()
                && record.extension_limits.is_some()
                && &route.profile == profile
        })
        .ok_or("extension_profile_not_offered")?;
    let mut expected = record
        .config
        .extension_probe
        .clone()
        .expect("offered profile has probe configuration");
    expected.extension_profile = Some(profile.clone());
    // The client selects its color mode in ANNOUNCE, after the offer
    // probe. Video checks the selected encoder with the final config.
    expected.color_mode = configuration.configuration.color_mode;
    let pyrowave = amaterasu_gamestream::sdp::pyrowave::is_profile(&route.profile);
    if pyrowave && (100_000..=expected.bitrate).contains(&configuration.configuration.bitrate) {
        expected.bitrate = configuration.configuration.bitrate;
    }
    if configuration.configuration != expected
        || configuration.source_id.is_some()
        || configuration.encoder_provider.is_some()
        || configuration.encoder_options.is_some()
    {
        return Err("extension_configuration_mismatch");
    }
    configuration.source_id = Some(route.source_id);
    configuration.encoder_provider = Some(route.encoder_provider.clone());
    configuration.encoder = route.encoder.clone();
    if let Some(wire_kbps) = configuration.extension_wire_bitrate_kbps.take() {
        let limits = record
            .extension_limits
            .as_ref()
            .expect("offered transport limits");
        let audio_bps = if record.config.stream_audio {
            record.config.audio.bitrate
        } else {
            0
        };
        let Some(bitrate) = (wire_kbps <= 1_000_000 && pyrowave)
            .then(|| {
                amaterasu_gamestream::sdp::pyrowave::encoder_bitrate_bps(
                    wire_kbps,
                    configuration.configuration.fps,
                    audio_bps,
                    limits,
                )
            })
            .flatten()
        else {
            return Err("invalid_extension_wire_budget");
        };
        configuration.configuration.bitrate = bitrate;
    }
    Ok(())
}

/// Match cleanup to the accepted start even after its capture lease has detached.
fn validate_stop_configuration(
    lifecycle_admitter: bool,
    record: &Record,
    configuration: &mut StartConfig,
) -> Result<(), &'static str> {
    let Some(start) = &record.start else {
        // Cleanup-only adoption has no stored configuration to compare.
        return validate_start_configuration(lifecycle_admitter, record, configuration);
    };
    // RTSP repeats the negotiated fields; stream-session owns the native
    // encoder fields added to a start that actually used the extension.
    if start.source_id.is_some() && start.configuration.extension_profile.is_some() {
        resolve_extension_start(record, configuration)?;
    }
    configuration.configuration.normalize_codec()?;
    if start != configuration {
        return Err("start_conflict");
    }
    Ok(())
}

/// Startup and cleanup adoption share the same owner, frozen configuration and
/// coordinator binding checks. Validation alone never opens native resources.
fn validate_start_configuration(
    lifecycle_admitter: bool,
    record: &Record,
    configuration: &mut StartConfig,
) -> Result<(), &'static str> {
    if configuration.lifecycle_target.is_some() && record.providers.lifecycle.is_none() {
        return Err("lifecycle_provider_not_selected");
    }
    if (configuration.client_feature_flags.is_some()
        || !configuration.stream_adjustments.is_empty())
        && !lifecycle_admitter
    {
        return Err("permission_denied");
    }
    if record.processing_plan_provider.is_some()
        && (configuration.lifecycle_target.is_none()
            || (configuration.lifecycle_capture_mode.is_managed()
                && configuration.lifecycle_candidate.is_none()))
    {
        return Err("processing_lifecycle_required");
    }
    match (
        &record.config.lifecycle_session_target,
        &configuration.lifecycle_target,
        &configuration.lifecycle_candidate,
    ) {
        (None, None, None)
            if configuration.lifecycle_capture_mode.is_managed()
                && record.config.lifecycle_capture_mode.is_managed() => {}
        (
            Some(amaterasu_extension_api::LifecycleTarget::Session {
                host_epoch,
                session_id,
                ..
            }),
            Some(amaterasu_extension_api::LifecycleTarget::Stream {
                host_epoch: stream_epoch,
                session_id: stream_session,
                stream_id,
                generation,
            }),
            None,
        ) if lifecycle_admitter
            && configuration.lifecycle_capture_mode.is_external()
            && record.config.lifecycle_capture_mode.is_external()
            && host_epoch == stream_epoch
            && session_id == stream_session
            && valid_name(stream_id)
            && *generation > 0 => {}
        (
            Some(amaterasu_extension_api::LifecycleTarget::Session {
                host_epoch,
                session_id,
                ..
            }),
            Some(amaterasu_extension_api::LifecycleTarget::Stream {
                host_epoch: stream_epoch,
                session_id: stream_session,
                stream_id,
                generation,
            }),
            Some(candidate),
        ) if lifecycle_admitter
            && configuration.lifecycle_capture_mode.is_managed()
            && record.config.lifecycle_capture_mode.is_managed()
            && host_epoch == stream_epoch
            && session_id == stream_session
            && valid_name(stream_id)
            && *generation > 0
            && candidate.generation == *generation
            && amaterasu_extension_api::validate_capture_binding(candidate).is_ok()
            && candidate.capture_provider == "amaterasu.capture-pipewire/1"
            && candidate.selector_schema == "amaterasu.kwin-wl-output/1"
            && candidate
                .selector
                .as_object()
                .is_some_and(|object| object.len() == 1)
            && candidate.selector["output"].as_str().is_some_and(|output| {
                output.starts_with("kwin:Virtual-") && valid_name(output)
            })
            && super::shared_capture::uses_capture_frames(record, configuration)
            && (record
                .extension_route
                .as_ref()
                .is_some_and(|route| route.submitted)
                || !candidate.mode.hdr)
            && configuration.capture_provider == crate::model::CaptureProvider::PipeWire => {}
        _ => {
            return Err("invalid_lifecycle_identity");
        }
    }
    configuration.configuration.normalize_codec()?;
    if record.config.audio.custom.is_some() {
        configuration.validate_audio_profile(record.config.stream_audio, true)?;
    } else {
        configuration.validate(record.config.stream_audio)?;
    }
    if configuration.source_capture_context != record.config.source_capture_context {
        return Err("source_capture_context_changed");
    }
    if let Some(source) = &record.config.borrowed_capture
        && (configuration.output != source.output
            || !super::shared_capture::uses_capture_frames(record, configuration))
    {
        return Err("borrowed_capture_changed");
    }
    super::shared_capture::validate_start(record, configuration)?;
    if configuration.source_id.is_some()
        && !record
            .extension_route
            .as_ref()
            .is_some_and(|route| route.submitted)
        && (record.config.display_request.is_some()
            || record.config.virtual_display_intent.is_some())
    {
        return Err("source_mode_display_incompatible");
    }
    if !configuration.matches_prepared_audio(&record.config) {
        return Err("audio_configuration_mismatch");
    }
    if configuration.configuration.extension_profile.is_none()
        && !configuration.matches_prepared_codec(&record.config)
    {
        return Err("video_codec_mismatch");
    }
    if !configuration.matches_prepared_encryption(&record.config) {
        return Err("encryption_configuration_mismatch");
    }
    if record
        .start
        .as_ref()
        .is_some_and(|existing| existing != configuration)
    {
        return Err("start_conflict");
    }
    Ok(())
}

/// Persist an adopted lifecycle identity before any provider cleanup is sent.
pub(super) fn prepare_stop_job(
    record: &mut Record,
    journal: &journal::LifecycleJournal,
    sequence: u64,
    request: Option<u64>,
    now: u64,
    drain_timeout_ms: u64,
) -> Result<Job, &'static str> {
    let persist = record
        .start
        .as_ref()
        .is_some_and(|start| start.lifecycle_target.is_some())
        && !journal.matches_lifecycle_identity(record);
    // Construct before moving any uncertain provider operation into the job.
    let document = persist.then(|| journal.with_record(record)).transpose()?;
    let mut job = stop_job(record, request, now, drain_timeout_ms, persist);
    if let Some(document) = document {
        job.journal_resume = Some(job.step);
        job.journal_payload = Some(journal.set_payload(&document));
        job.journal_document = Some(document);
        job.journal_operation_id = Some(journal.op_id(sequence.saturating_add(1)));
        job.step = Step::PersistJournal;
    }
    Ok(job)
}

/// Build the ordinary owned stop workflow without dispatching provider work.
pub(super) fn stop_job(
    record: &mut Record,
    request: Option<u64>,
    now: u64,
    drain_timeout_ms: u64,
    persist_lifecycle_identity: bool,
) -> Job {
    let request = request.or(record.video_warmup_wait.take().map(|wait| wait.request));
    let retrying_cleanup = record.state == State::CleanupFailed;
    let mut job = Job::new(
        record.id,
        request,
        if record.input_lookup_required || (record.input_attempted && record.input_lease.is_none())
        {
            Step::FindInput
        } else if record.input_lease.is_some() {
            Step::ReleaseInput
        } else {
            Step::DrainVideo
        },
        now + drain_timeout_ms,
    );
    record.outcome = State::Stopped;
    record.history.stopping("stop_requested");
    record.state = State::Stopping;
    record.cleanup_error = None;
    input::queue_rumble_zeros(record, now);
    if retrying_cleanup
        || record
            .start
            .as_ref()
            .is_some_and(|start| start.lifecycle_target.is_some())
    {
        if !record.rumble_pending.is_empty() {
            record.rumble_stop_unconfirmed = true;
            record.rumble_pending.clear();
            record.rumble_active_until.clear();
        }
        record.plane = None;
    }
    if !record.rumble_pending.is_empty() {
        job.step = Step::RumbleDrain;
    }
    if let Some(plane) = record.plane.as_mut() {
        plane.stop();
    } else {
        job.cleanup = true;
        job.deadline = now + 15000;
        if !persist_lifecycle_identity {
            job.settle_uncertain(record);
        }
        if job.pending.is_none() {
            if let Some(step) = job.next_cleanup(record) {
                job.step = step;
            } else {
                // A previous cleanup attempt may already have retired every
                // resource. Let the driver confirm journal retirement.
                job.step = Step::WaitDrain;
            }
        }
    }
    job
}
