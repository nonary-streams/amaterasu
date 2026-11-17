use super::lifecycle_issues::Cause;
use super::*;

impl Controller {
    pub(super) fn advance_job(&mut self, runtime: &RuntimeClient, now: u64) {
        let Some(mut job) = self.job.take() else {
            return;
        };
        let Some(mut record) = self.records.remove(&job.ticket) else {
            return;
        };
        if job.phase == Phase::Observe
            && record.state == State::Closing
            && self.shutdown_settlement.len() < MAX_RECORDS
        {
            // Status is read-only: preserve its exact cancellation/release
            // ownership while allowing native cleanup to proceed independently.
            job.call.cancel_read_only();
            self.shutdown_settlement.push(job.call);
            self.records.insert(record.id, record);
            return;
        }
        if job.phase == Phase::Prepare
            && record.state != State::Closing
            && job
                .caller
                .is_some_and(|request| !rpc::active(runtime, request))
        {
            record.cancel();
            job.call.cancel();
            job.deadline = self.cleanup_deadline(now);
        }
        if job.phase == Phase::Observe && record.state == State::Playing && now >= job.deadline {
            job.call.cancel_read_only();
            self.late_observations.insert(record.id, job.call);
            record.next_status = now.saturating_add(STATUS_INTERVAL_MS);
            self.records.insert(record.id, record);
            return;
        }
        if now >= job.deadline
            && record.state != State::Closing
            && record.close(Some(
                amaterasu_module_sdk::stream_lifecycle::StreamFailure::requirement_failed(
                    "operation_deadline",
                ),
            ))
        {
            job.call.cancel();
            job.deadline = self.cleanup_deadline(now);
        }
        if self
            .shutdown_deadline
            .is_some_and(|deadline| now >= deadline)
            && (matches!(record.state, State::Closing | State::CleanupFailed)
                || matches!(job.phase, Phase::Stop | Phase::Find))
        {
            job.call.cancel();
            self.retain_uncertain(&mut record, job, "cleanup_deadline", runtime);
            self.records.insert(record.id, record);
            return;
        }
        let outcome = job.call.poll(runtime);
        match outcome {
            Outcome::Waiting if now >= job.deadline && record.state == State::Closing => {
                self.retain_uncertain(&mut record, job, "cleanup_deadline", runtime);
            }
            Outcome::Waiting => self.job = Some(job),
            Outcome::Uncertain if job.phase == Phase::Observe && record.state == State::Playing => {
                job.call.cancel_read_only();
                job.call.retry_settlement();
                self.late_observations.insert(record.id, job.call);
                record.next_status = now.saturating_add(STATUS_INTERVAL_MS);
            }
            Outcome::Uncertain => {
                self.retain_uncertain(
                    &mut record,
                    job,
                    "request_cancellation_unconfirmed",
                    runtime,
                );
            }
            Outcome::SettledFailure => self.consume_settled_failure(&mut record, job, now, runtime),
            Outcome::Result(payload) => {
                self.consume_result(&mut record, job, payload, now, runtime)
            }
        }
        self.records.insert(record.id, record);
    }

    pub(super) fn consume_settled_failure(
        &mut self,
        record: &mut Record,
        job: Job,
        now: u64,
        runtime: &RuntimeClient,
    ) {
        match job.phase {
            Phase::Prepare => {
                record.close(Some(
                    amaterasu_module_sdk::stream_lifecycle::StreamFailure::requirement_failed(
                        "dependency_unavailable",
                    ),
                ));
                if job.call.proven_not_dispatched() {
                    record.needs_lookup = false;
                    self.schedule_cleanup(record, job.caller, now);
                    complete_caller(record, job.caller, runtime);
                } else {
                    self.schedule_lookup(record, job.caller, now);
                }
            }
            Phase::LifecyclePrepare | Phase::LifecycleLookup => {
                let failure =
                    amaterasu_module_sdk::stream_lifecycle::StreamFailure::requirement_failed(
                        "dependency_unavailable",
                    );
                let cause = Some(Cause::of_phase(job.phase));
                if record.admission() == Admission::Open {
                    if let Some((connection, action_ticket)) = job.network {
                        self.finish_network_result(
                            record,
                            connection,
                            action_ticket,
                            Err(failure),
                            cause,
                        );
                    } else {
                        record.close_with_cause(Some(failure), cause);
                    }
                }
                if job.phase == Phase::LifecyclePrepare && job.call.proven_not_dispatched() {
                    record.lifecycle_prepare_outstanding = false;
                    self.schedule_stop(record, job.caller, now);
                } else if job.phase == Phase::LifecyclePrepare {
                    // Broker settlement cannot roll back a committed coordinator
                    // transition. Recover its exact admission before cleanup.
                    self.schedule_lifecycle_lookup(
                        record,
                        job.caller,
                        None,
                        self.cleanup_deadline(now),
                    );
                } else {
                    record.fail_cleanup(
                        amaterasu_module_sdk::stream_lifecycle::StreamFailure::requirement_failed(
                            "lifecycle_prepare_lookup_unavailable",
                        ),
                    );
                    complete_caller(record, job.caller, runtime);
                }
            }
            Phase::Arm => {
                if record.admission() == Admission::Ended {
                    self.schedule_cleanup(record, job.caller, now);
                } else if let Some((connection, action_ticket)) = job.network {
                    self.fail_network_action(
                        record,
                        connection,
                        action_ticket,
                        amaterasu_module_sdk::stream_lifecycle::StreamFailure::requirement_failed(
                            "dependency_unavailable",
                        ),
                    );
                } else {
                    record.close(Some(
                        amaterasu_module_sdk::stream_lifecycle::StreamFailure::requirement_failed(
                            "network_disconnected",
                        ),
                    ));
                    self.schedule_stop(record, job.caller, now);
                }
            }
            Phase::Stop => {
                self.schedule_status_until(record, job.caller, job.network, job.deadline)
            }
            Phase::Observe if record.state == State::Playing => {
                record.next_status = now.saturating_add(STATUS_INTERVAL_MS);
            }
            Phase::Find | Phase::Observe | Phase::Status => {
                record.fail_cleanup(
                    amaterasu_module_sdk::stream_lifecycle::StreamFailure::requirement_failed(
                        "dependency_unavailable",
                    ),
                );
                complete_caller(record, job.caller, runtime);
            }
        }
    }

    pub(super) fn consume_result(
        &mut self,
        record: &mut Record,
        job: Job,
        payload: Value,
        now: u64,
        runtime: &RuntimeClient,
    ) {
        match job.phase {
            Phase::Prepare => self.prepared(record, job, &payload, now, runtime),
            Phase::Find => self.found(record, job, &payload, now, runtime),
            Phase::LifecyclePrepare | Phase::LifecycleLookup => {
                self.lifecycle_prepared(record, job, &payload, now, runtime)
            }
            Phase::Arm => self.armed(record, job, &payload, now),
            Phase::Stop => self.stopped(record, job, &payload, runtime),
            Phase::Observe | Phase::Status => {
                self.status_result(record, job, &payload, now, runtime)
            }
        }
    }

    fn lifecycle_prepared(
        &mut self,
        record: &mut Record,
        job: Job,
        payload: &Value,
        now: u64,
        runtime: &RuntimeClient,
    ) {
        let network = job.network.filter(|_| record.state == State::Arming);
        match super::preparation::apply_response(record, payload) {
            super::preparation::PreparationResolution::Pending => {
                self.schedule_lifecycle_lookup(record, job.caller, network, job.deadline);
            }
            super::preparation::PreparationResolution::Rejected(fact) => {
                use amaterasu_module_sdk::stream_lifecycle::{StreamFailure, StreamFailureEffect};
                const CODE: &str = "lifecycle_prepare_rejected";
                if let Some((connection, action_ticket)) = network {
                    if let Some(fact) = fact {
                        self.finish_provider_failure(
                            record,
                            connection,
                            action_ticket,
                            (fact, StreamFailureEffect::RequirementFailed),
                            CODE,
                        );
                    } else {
                        self.fail_network_action(
                            record,
                            connection,
                            action_ticket,
                            StreamFailure::requirement_failed(CODE),
                        );
                    }
                } else if !matches!(record.state, State::Closing | State::CleanupFailed) {
                    record.close(Some(StreamFailure::requirement_failed(CODE)));
                }
                if record.state == State::Closing {
                    self.schedule_stop(record, job.caller, now);
                }
            }
            super::preparation::PreparationResolution::Admitted => {
                if let Some((connection, action_ticket)) = network {
                    let Some(start) = record.authenticated_lifecycle_start_payload() else {
                        self.fail_network_action(
                            record,
                            connection,
                            action_ticket,
                            amaterasu_module_sdk::stream_lifecycle::StreamFailure::requirement_failed(
                                "negotiation_failed",
                            ),
                        );
                        return;
                    };
                    self.schedule_actual_arm(
                        record,
                        start,
                        connection,
                        action_ticket,
                        job.deadline,
                    );
                } else {
                    record.close(Some(
                        amaterasu_module_sdk::stream_lifecycle::StreamFailure::requirement_failed(
                            "network_disconnected",
                        ),
                    ));
                    self.schedule_stop(record, job.caller, now);
                }
            }
            super::preparation::PreparationResolution::Unconfirmed => {
                self.reject_lifecycle_preparation(record, network, job.caller, runtime);
            }
        }
    }

    fn reject_lifecycle_preparation(
        &mut self,
        record: &mut Record,
        network: Option<(u64, u64)>,
        caller: Option<u64>,
        runtime: &RuntimeClient,
    ) {
        if let Some((connection, action_ticket)) = network {
            self.fail_network_action(
                record,
                connection,
                action_ticket,
                amaterasu_module_sdk::stream_lifecycle::StreamFailure::requirement_failed(
                    "lifecycle_prepare_rejected",
                ),
            );
        } else {
            record.close(Some(
                amaterasu_module_sdk::stream_lifecycle::StreamFailure::requirement_failed(
                    "lifecycle_prepare_rejected",
                ),
            ));
            record.fail_cleanup(
                amaterasu_module_sdk::stream_lifecycle::StreamFailure::requirement_failed(
                    "lifecycle_prepare_unconfirmed",
                ),
            );
            complete_caller(record, caller, runtime);
        }
    }

    pub(super) fn prepared(
        &mut self,
        record: &mut Record,
        job: Job,
        payload: &Value,
        now: u64,
        runtime: &RuntimeClient,
    ) {
        record.session = response_session(payload).or(record.session);
        if matches!(record.state, State::Closing | State::CleanupFailed) {
            // A late successful prepare still owns its session, but shutdown
            // or cancellation has already closed admission to this ticket.
            if record.session.is_some() {
                record.needs_lookup = false;
            }
            self.schedule_cleanup(record, job.caller, now);
            return;
        }
        let prepared = (|| {
            if payload["ok"] != true || payload["state"] != "prepared" {
                eprintln!(
                    "amaterasu rtsp prepare not ready state={} code={} error={}",
                    safe_provider_code(&payload["state"]),
                    safe_provider_code(&payload["code"]),
                    safe_provider_code(&payload["error"]),
                );
                return Err("prepare_rejected");
            }
            if record
                .display_preflight_deadline
                .is_some_and(|expiry| now >= expiry)
            {
                return Err("display_admission_expired");
            }
            if let Some(expected) = &record.config.display_request
                && (payload["display_request"] != *expected
                    || payload["display_applied"] != true
                    || payload["display_state"] != "applied")
            {
                return Err("display_lease_unverified");
            }
            if record
                .config
                .virtual_display_intent
                .as_ref()
                .is_some_and(amaterasu_contracts::virtual_display::VirtualDisplayIntent::active)
                && (payload["virtual_display_ready"] != true
                    || payload["display_applied"] != true
                    || payload["display_state"] != "applied")
            {
                return Err("display_lease_unverified");
            }
            match record.config.history_activity_id.as_deref() {
                Some(activity_id)
                    if payload["history_activity_id"].as_str() == Some(activity_id)
                        && payload["rtsp_operation_id"].as_str()
                            == Some(record.config.operation_id.as_str()) => {}
                None if payload["history_activity_id"].is_null()
                    && if record.config.lifecycle_capture_mode.is_external() {
                        payload["rtsp_operation_id"] == record.config.operation_id
                    } else {
                        payload["rtsp_operation_id"].is_null()
                    } => {}
                _ => return Err("history_activity_unverified"),
            }
            if payload["lifecycle_session_target"] != json!(record.config.lifecycle_session_target)
            {
                return Err("lifecycle_identity_unverified");
            }
            if payload["borrowed_capture"] != json!(record.config.borrowed_capture) {
                return Err("borrowed_capture_unverified");
            }
            if payload["source_capture_context"] != json!(record.config.source_capture_context) {
                return Err("source_capture_context_unverified");
            }
            if serde_json::from_value::<amaterasu_extension_api::CaptureBindingMode>(
                payload
                    .get("lifecycle_capture_mode")
                    .cloned()
                    .unwrap_or(json!("managed")),
            )
            .ok()
                != Some(record.config.lifecycle_capture_mode)
            {
                return Err("lifecycle_mode_unverified");
            }
            if record.config.lifecycle_session_target.is_some() {
                record.lifecycle_input_required = Some(
                    payload["lifecycle_input_required"]
                        .as_bool()
                        .ok_or("lifecycle_input_policy_unverified")?,
                );
            }
            if record.display_prepare_timeout_ms.is_some_and(|expected| {
                payload["display_prepare_timeout_ms"].as_u64() != Some(u64::from(expected))
            }) {
                return Err("display_lease_unverified");
            }
            let session = response_session(payload).ok_or("invalid_prepare_response")?;
            let audio = local_port(payload, "audio_local")?;
            let video = local_port(payload, "video_local")?;
            let control = local_port(payload, "control_local")?;
            let key = record
                .config
                .validate(self.settings.allow_remote)
                .map_err(|_| "invalid_prepare_response")?;
            let launch = TrustedLaunch {
                epoch: record.id,
                operation: record.config.operation_id.clone(),
                rtsp_target: record.url.clone(),
                session_token: record.session_token.clone(),
                encrypted_rtsp: true,
                audio_port: audio,
                video_port: video,
                control_port: control,
                control_connect_data: record.config.connect_data,
                udp_discovery: record.udp_discovery.clone(),
            };
            let profile = if self.settings.extension_enabled
                && record.config.min_threads == 2
                && record.config.qp == 28
            {
                serde_json::from_value::<amaterasu_contracts::media_profile::VideoProfile>(
                    payload["extension_profile"].clone(),
                )
                .ok()
                .filter(|profile| profile.is_valid())
            } else {
                None
            };
            let negotiation = if let Some(profile) = &profile {
                let snapshot = amaterasu_contracts::media_profile::VideoProfileSnapshot {
                    profiles: vec![profile.clone()],
                };
                Negotiation::with_video_profiles(
                    launch,
                    record.capabilities(),
                    snapshot.clone(),
                    snapshot,
                )
            } else {
                Negotiation::new(launch, record.capabilities())
            }
            .and_then(|negotiation| negotiation.with_failure_owner(&record.failure_scope))
            .map_err(|_| "invalid_prepare_response")?;
            self.transport.as_mut().ok_or("disabled")?.add_ticket(
                record.id,
                record.config.peer_ip,
                &key,
            )?;
            record.session = Some(session);
            record.extension_offer = profile;
            record.negotiation = Some(negotiation);
            record.needs_lookup = false;
            record.stream_state = Some("prepared".into());
            let expires = record
                .display_lease_ready_until
                .unwrap_or_else(|| now.saturating_add(u64::from(record.config.ping_timeout_ms)))
                .min(record.display_preflight_deadline.unwrap_or(u64::MAX));
            if record.config.display_admission_required() && expires <= now {
                return Err("display_lease_expired");
            }
            record.display_applied = record.config.display_request.is_some()
                || record.config.virtual_display_intent.as_ref().is_some_and(
                    amaterasu_contracts::virtual_display::VirtualDisplayIntent::active,
                );
            record.state = State::Ready;
            record.expires = expires;
            Ok(())
        })();
        if let Err(code) = prepared {
            eprintln!("amaterasu rtsp prepare failed code={code}");
            let cause = if code == "prepare_rejected" {
                Some(Cause::stream_session(provider_code(payload)))
            } else {
                Cause::of_close(code)
            };
            if record.close_with_cause(
                Some(
                    amaterasu_module_sdk::stream_lifecycle::StreamFailure::requirement_failed(code),
                ),
                cause,
            ) {
                self.revoke_ticket(record.id);
                self.schedule_cleanup(record, job.caller, now);
            }
            return;
        }
        let accepted = job.caller.is_some_and(|request| {
            rpc::active(runtime, request) && rpc::complete(runtime, request, record.response())
        });
        if !accepted
            && record.close(Some(
                amaterasu_module_sdk::stream_lifecycle::StreamFailure::requirement_failed(
                    "completion_unavailable",
                ),
            ))
        {
            self.revoke_ticket(record.id);
            self.schedule_stop(record, None, now);
        }
    }

    pub(super) fn found(
        &mut self,
        record: &mut Record,
        job: Job,
        payload: &Value,
        now: u64,
        runtime: &RuntimeClient,
    ) {
        if missing(payload) {
            record.needs_lookup = false;
            record.cleanup_confirmed =
                record.lifecycle_target.is_none() && !record.lifecycle_prepare_outstanding;
            finish_record(record);
            complete_caller(record, job.caller, runtime);
            return;
        }
        let Some(session) = response_session(payload) else {
            record.fail_cleanup(
                amaterasu_module_sdk::stream_lifecycle::StreamFailure::requirement_failed(
                    "invalid_lookup_response",
                ),
            );
            complete_caller(record, job.caller, runtime);
            return;
        };
        record.session = Some(session);
        record.needs_lookup = false;
        self.schedule_stop(record, job.caller, now);
    }

    pub(super) fn armed(&mut self, record: &mut Record, job: Job, payload: &Value, now: u64) {
        if matches!(record.state, State::Closing | State::CleanupFailed) {
            // Do not send PLAY success or revive a ticket that was closed while
            // its provider was completing the authenticated start.
            self.schedule_cleanup(record, job.caller, now);
            return;
        }
        let state = stream_state(payload);
        let same_session = record.session.is_some() && response_session(payload) == record.session;
        let valid = payload["ok"] == true
            && same_session
            && matches!(state, Some("armed" | "starting" | "active"))
            && payload["video_startup_ready"] == true
            && payload["cleanup_error"].is_null();
        if !valid {
            eprintln!(
                "amaterasu rtsp arm rejected state={} code={} error={}",
                safe_provider_code(&payload["state"]),
                safe_provider_code(&payload["code"]),
                safe_provider_code(&payload["error"]),
            );
        }
        let Some((connection, action_ticket)) = job.network else {
            record.close(Some(
                amaterasu_module_sdk::stream_lifecycle::StreamFailure::requirement_failed(
                    "network_disconnected",
                ),
            ));
            self.schedule_stop(record, job.caller, now);
            return;
        };
        if record
            .negotiation
            .as_ref()
            .is_some_and(|negotiation| negotiation.failure_scope(record.id, action_ticket).is_err())
        {
            return;
        }
        if same_session && let Some(state) = state {
            record.stream_state = Some(state.into());
        }
        let cause = (!valid).then(|| Cause::stream_session(provider_code(payload)));
        use amaterasu_module_sdk::stream_lifecycle::{
            LifecycleLayer, StreamFailure, StreamFailureEffect, StreamFailureFact, StreamIssue,
        };
        if !valid {
            let owns_report = |fact: &StreamFailureFact| {
                let local_scope = record.config.history_activity_id.is_none()
                    && record.config.lifecycle_capture_mode.is_managed()
                    && record
                        .session
                        .is_some_and(|session| fact.operation == format!("ss-{session:x}"));
                same_session
                    && (fact.operation == record.config.operation_id || local_scope)
                    && fact.layer == LifecycleLayer::StreamSession
                    && fact.phase == "start"
            };
            let unchanged = payload["ok"] == false
                && record.state == State::Arming
                && state == Some("prepared")
                && payload["cleanup_error"].is_null();
            let report = match payload.get("stream_failure_fact") {
                Some(fact) => serde_json::from_value::<StreamFailureFact>(fact.clone())
                    .ok()
                    .filter(owns_report)
                    .map(|fact| (fact, unchanged)),
                None => payload
                    .get("stream_failure")
                    .and_then(|report| serde_json::from_value::<StreamIssue>(report.clone()).ok())
                    .filter(|report| report.valid() && !report.reason.is_empty())
                    .and_then(|report| {
                        let fact = StreamFailureFact::from(&report);
                        owns_report(&fact).then_some((fact, unchanged && !report.is_critical()))
                    }),
            };
            if let Some((fact, preparation_unchanged)) = report {
                let effect = if preparation_unchanged {
                    StreamFailureEffect::PreparationUnchanged
                } else {
                    StreamFailureEffect::RequiredStartUnverified
                };
                if self.finish_provider_failure(
                    record,
                    connection,
                    action_ticket,
                    (fact, effect),
                    "start_rejected",
                ) && record.state == State::Arming
                {
                    record.restore_admission();
                }
            } else {
                self.finish_network_result(
                    record,
                    connection,
                    action_ticket,
                    Err(StreamFailure::requirement_failed("start_rejected")),
                    cause,
                );
            }
            return;
        }
        if self.finish_network_result(
            record,
            connection,
            action_ticket,
            Ok(super::network::NetworkCompletion::NativeStart),
            cause,
        ) {
            record.state = State::Playing;
            record.expires = u64::MAX;
            record.observation_stale_at = now.saturating_add(OBSERVATION_STALE_MS);
            record.next_status = now.saturating_add(STATUS_INTERVAL_MS);
        }
    }

    pub(super) fn stopped(
        &mut self,
        record: &mut Record,
        job: Job,
        payload: &Value,
        runtime: &RuntimeClient,
    ) {
        if confirm_terminal(record, payload) {
            record.authenticated_disconnect_reason =
                authenticated_disconnect_reason(record, payload);
            record.stream_state = stream_state(payload).map(str::to_owned);
            record.cleanup_error = None;
            self.finish_cleanup_reply(record, job, runtime);
        } else if response_session(payload) == record.session {
            self.schedule_status_until(record, job.caller, job.network, job.deadline);
        } else {
            record.fail_cleanup(
                amaterasu_module_sdk::stream_lifecycle::StreamFailure::requirement_failed(
                    "invalid_stop_response",
                ),
            );
            complete_caller(record, job.caller, runtime);
        }
    }

    pub(super) fn status_result(
        &mut self,
        record: &mut Record,
        job: Job,
        payload: &Value,
        now: u64,
        runtime: &RuntimeClient,
    ) {
        let state = stream_state(payload);
        let observing = job.phase == Phase::Observe && record.state == State::Playing;
        let matches_target = record.lifecycle_target.as_ref()
            == payload
                .get("lifecycle_target")
                .filter(|target| !target.is_null());
        if response_session(payload) != record.session
            || payload["ok"] != true
            || state.is_none()
            || (observing && !matches_target)
        {
            if observing {
                // A bad read cannot change the last authenticated observation
                // or extend the original deadline.
                record.next_status = now.saturating_add(STATUS_INTERVAL_MS);
            } else {
                record.fail_cleanup(
                    amaterasu_module_sdk::stream_lifecycle::StreamFailure::requirement_failed(
                        "invalid_status_response",
                    ),
                );
                complete_caller(record, job.caller, runtime);
            }
            return;
        }
        let terminal = confirm_terminal(record, payload);
        if !terminal
            && observing
            && (state != Some("active") || !payload["cleanup_error"].is_null())
        {
            if state == Some("stopping") {
                // Expose the verified fence to reconnect/join without treating
                // an intermediate status as fresh or complete retirement.
                record.stream_state = Some("stopping".into());
            }
            record.next_status = now.saturating_add(STATUS_INTERVAL_MS);
            return;
        }
        record.stream_state = state.map(str::to_owned);
        record.observation_stale_at = now.saturating_add(OBSERVATION_STALE_MS);
        if terminal {
            record.authenticated_disconnect_reason =
                authenticated_disconnect_reason(record, payload);
            // Confirmed retirement is an observation of a completed owner
            // transition. Project its outcome without making a second decision.
            if record.state != State::Closing && state == Some("failed") && record.failure.is_none()
            {
                use amaterasu_module_sdk::stream_lifecycle::{LifecycleLayer, StreamIssue};
                record.failure = Some("stream_failed");
                record.issues.push(
                    StreamIssue::critical(
                        &record.config.operation_id,
                        LifecycleLayer::Rtsp,
                        record.state.phase(),
                        "stream_failed",
                    )
                    .caused_by(LifecycleLayer::StreamSession),
                );
            }
            self.finish_cleanup_reply(record, job, runtime);
        } else if matches!(state, Some("cleanup_failed" | "failed" | "cancelled"))
            || !payload["cleanup_error"].is_null()
        {
            if record.state == State::Closing {
                record.fail_cleanup(
                    amaterasu_module_sdk::stream_lifecycle::StreamFailure::requirement_failed(
                        "stream_cleanup_failed",
                    ),
                );
                complete_caller(record, job.caller, runtime);
            } else {
                if record.close(Some(
                    amaterasu_module_sdk::stream_lifecycle::StreamFailure::requirement_failed(
                        "stream_failed",
                    ),
                )) {
                    self.revoke_ticket(record.id);
                    self.schedule_stop(record, None, now);
                }
            }
        } else if record.state == State::Closing {
            self.schedule_status_until(record, job.caller, job.network, job.deadline);
        } else {
            record.next_status = now.saturating_add(STATUS_INTERVAL_MS);
        }
    }

    pub(super) fn finish_cleanup_reply(
        &mut self,
        record: &mut Record,
        job: Job,
        runtime: &RuntimeClient,
    ) {
        // The stream-session Stop/Status has confirmed cleanup even if RTSP is
        // still waiting for the TEARDOWN reply to leave the network transport.
        record.display_applied = false;
        record.cleanup_retry_count = 0;
        record.cleanup_retry_at = None;
        if let Some((connection, action_ticket)) = job.network {
            let reply = record
                .negotiation
                .as_mut()
                .and_then(|negotiation| negotiation.complete(record.id, action_ticket).ok());
            if let Some(reply) = reply {
                if self.reply_bytes(connection, &reply) {
                    record.revoke_after = Some(connection);
                } else {
                    self.revoke_ticket(record.id);
                    finish_record(record);
                }
            } else {
                self.revoke_ticket(record.id);
                finish_record(record);
            }
        } else {
            if record.revoke_after.is_none() {
                self.revoke_ticket(record.id);
                finish_record(record);
            }
        }
        complete_caller(record, job.caller, runtime);
    }

    pub(super) fn retain_uncertain(
        &mut self,
        record: &mut Record,
        job: Job,
        code: &'static str,
        runtime: &RuntimeClient,
    ) {
        if !matches!(record.state, State::Closing | State::CleanupFailed) {
            record.close_with_cause(
                Some(
                    amaterasu_module_sdk::stream_lifecycle::StreamFailure::requirement_failed(
                        "dependency_unavailable",
                    ),
                ),
                Some(Cause::of_phase(job.phase)),
            );
        }
        record.fail_cleanup(
            amaterasu_module_sdk::stream_lifecycle::StreamFailure::requirement_failed(code),
        );
        record.uncertain_phase = Some(job.phase);
        record.uncertain = Some(job.call);
        if matches!(record.state, State::Closing | State::CleanupFailed) {
            self.revoke_ticket(record.id);
        }
        complete_caller(record, job.caller, runtime);
    }

    pub(super) fn advance_late_observations(&mut self, runtime: &RuntimeClient) {
        self.late_observations
            .retain(|_, call| match call.poll(runtime) {
                Outcome::Result(_) | Outcome::SettledFailure => false,
                Outcome::Waiting => true,
                Outcome::Uncertain => {
                    call.retry_settlement();
                    true
                }
            });
    }

    pub(super) fn schedule_background(&mut self, now: u64) {
        for record in self.records.values_mut() {
            if record.state == State::Playing && now >= record.observation_stale_at {
                record.close(Some(
                    amaterasu_module_sdk::stream_lifecycle::StreamFailure::requirement_failed(
                        "stream_observation_lost",
                    ),
                ));
            }
        }
        if let Some(ticket) = self
            .records
            .values()
            .find(|record| {
                matches!(record.state, State::Ready | State::Arming) && now >= record.expires
            })
            .map(|record| record.id)
        {
            let record = self.records.get_mut(&ticket).expect("selected record");
            if record.close(Some(
                amaterasu_module_sdk::stream_lifecycle::StreamFailure::requirement_failed(
                    "prepare_expired",
                ),
            )) {
                self.revoke_ticket(ticket);
            }
        }
        // Cleanup of ended tickets is scheduled by `schedule_retiring`; the
        // job slot is left for open tickets.
        // Timed-out reads settle independently, at most one per ticket.
        // Each shutdown observation belongs to a distinct record that has
        // left Playing, and RTSP records remain resident for this runtime.
        // Thus a full ledger cannot coexist with another Playing record.
        // Preserve this reservation invariant if record reclamation is added.
        if self.shutdown_settlement.len() >= MAX_RECORDS {
            return;
        }
        let eligible = |record: &&Record| {
            record.state == State::Playing
                && now >= record.next_status
                && !self.late_observations.contains_key(&record.id)
                && self.late_observations.len() < MAX_RECORDS
        };
        let next = self
            .records
            .values()
            .filter(eligible)
            .find(|record| record.id > self.status_after)
            .or_else(|| self.records.values().find(eligible))
            .map(|record| record.id);
        if let Some(ticket) = next {
            self.status_after = ticket;
            let record = self.records.remove(&ticket).expect("selected record");
            self.schedule_status(&record, None, None, now);
            self.records.insert(ticket, record);
        }
    }

    pub(super) fn schedule_cleanup(&mut self, record: &mut Record, caller: Option<u64>, now: u64) {
        if let Some(mut call) = record.uncertain.take() {
            call.retry_settlement();
            let phase = record.uncertain_phase.take().unwrap_or(Phase::Prepare);
            self.job = Some(Job {
                ticket: record.id,
                phase,
                call,
                caller,
                network: None,
                deadline: self.cleanup_deadline(now),
            });
        } else if record.session.is_some() && record.lifecycle_prepare_outstanding {
            self.schedule_lifecycle_lookup(record, caller, None, self.cleanup_deadline(now));
        } else if record.session.is_some() {
            self.schedule_stop(record, caller, now);
        } else if record.needs_lookup {
            self.schedule_lookup(record, caller, now);
        } else {
            record.cleanup_confirmed =
                record.lifecycle_target.is_none() && !record.lifecycle_prepare_outstanding;
            finish_record(record);
        }
    }

    fn schedule_lifecycle_lookup(
        &mut self,
        record: &Record,
        caller: Option<u64>,
        network: Option<(u64, u64)>,
        deadline: u64,
    ) {
        let deadline = if matches!(record.state, State::Closing | State::CleanupFailed) {
            self.cap_cleanup_deadline(deadline)
        } else {
            deadline
        };
        let operation = self.operation_id(record, "lifecycle-lookup");
        let call = Call::for_service(
            self.settings.lifecycle_service.as_deref().unwrap_or(""),
            json!({"operation":"lookup", "operation_id":format!("{}-lifecycle-prepare", record.operation())}),
            operation,
            None,
        );
        self.job = Some(Job {
            ticket: record.id,
            phase: Phase::LifecycleLookup,
            call,
            caller,
            network,
            deadline,
        });
    }

    pub(super) fn schedule_lookup(&mut self, record: &Record, caller: Option<u64>, now: u64) {
        let operation = self.operation_id(record, "lookup");
        let call = Call::for_service(
            &self.settings.stream_service,
            json!({"action":"lookup","operation_id":record.operation()}),
            operation,
            None,
        );
        self.job = Some(Job {
            ticket: record.id,
            phase: Phase::Find,
            call,
            caller,
            network: None,
            deadline: self.cleanup_deadline(now),
        });
    }

    pub(super) fn schedule_stop(&mut self, record: &Record, caller: Option<u64>, now: u64) {
        self.job = Some(Job {
            ticket: record.id,
            phase: Phase::Stop,
            call: self.stop_call(record),
            caller,
            network: None,
            deadline: self.cleanup_deadline(now),
        });
    }

    pub(super) fn schedule_status(
        &mut self,
        record: &Record,
        caller: Option<u64>,
        network: Option<(u64, u64)>,
        now: u64,
    ) {
        let deadline = now.saturating_add(if record.state == State::Closing {
            CLEANUP_BUDGET_MS
        } else {
            PREPARE_BUDGET_MS
        });
        self.schedule_status_until(record, caller, network, deadline);
        if let Some(job) = self.job.as_mut() {
            job.phase = Phase::Observe;
        }
    }

    pub(super) fn schedule_status_until(
        &mut self,
        record: &Record,
        caller: Option<u64>,
        network: Option<(u64, u64)>,
        deadline: u64,
    ) {
        let deadline = if matches!(record.state, State::Closing | State::CleanupFailed) {
            self.cap_cleanup_deadline(deadline)
        } else {
            deadline
        };
        let operation = self.operation_id(record, "status");
        let call = Call::for_service(
            &self.settings.stream_service,
            json!({"action":"status","session":record.session}),
            operation,
            None,
        );
        self.job = Some(Job {
            ticket: record.id,
            phase: Phase::Status,
            call,
            caller,
            network,
            deadline,
        });
    }

    pub(super) fn cleanup_deadline(&self, now: u64) -> u64 {
        self.cap_cleanup_deadline(now.saturating_add(CLEANUP_BUDGET_MS))
    }

    fn cap_cleanup_deadline(&self, deadline: u64) -> u64 {
        self.shutdown_deadline
            .map_or(deadline, |announced| deadline.min(announced))
    }

    pub(super) fn stop_call(&mut self, record: &Record) -> Call {
        let operation = self.operation_id(record, "stop");
        Call::for_service(
            &self.settings.stream_service,
            record.stop_payload(),
            operation,
            None,
        )
    }

    pub(super) fn operation_id(&mut self, record: &Record, phase: &str) -> String {
        let Some(next) = self.sequence.checked_add(1) else {
            // Empty IDs are rejected locally by Call before any broker request.
            return String::new();
        };
        self.sequence = next;
        format!("{}-{phase}-{:016x}", record.operation(), self.sequence)
    }
}

fn safe_provider_code(value: &Value) -> &str {
    value
        .as_str()
        .filter(|code| {
            !code.is_empty()
                && code.len() <= 64
                && code
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        })
        .unwrap_or("unavailable")
}

/// A provider's own failure code, when it gave a well-formed one.
fn provider_code(payload: &Value) -> Option<&str> {
    Some(safe_provider_code(&payload["code"])).filter(|code| *code != "unavailable")
}
