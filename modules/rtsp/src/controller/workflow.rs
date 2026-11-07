use super::*;

impl Controller {
    pub(super) fn advance_job(&mut self, runtime: &RuntimeClient, now: u64) {
        let Some(mut job) = self.job.take() else {
            return;
        };
        let Some(mut record) = self.records.remove(&job.ticket) else {
            return;
        };
        if job.phase == Phase::Prepare
            && record.state != State::Closing
            && job
                .caller
                .is_some_and(|request| !rpc::active(runtime, request))
        {
            record.close(Some("request_cancelled"));
            job.call.cancel();
            job.deadline = now.saturating_add(CLEANUP_BUDGET_MS);
        }
        if now >= job.deadline && record.state != State::Closing {
            record.close(Some("operation_deadline"));
            job.call.cancel();
            job.deadline = now.saturating_add(CLEANUP_BUDGET_MS);
        }
        let outcome = job.call.poll(runtime);
        match outcome {
            Outcome::Waiting if now >= job.deadline && record.state == State::Closing => {
                self.retain_uncertain(&mut record, job, "cleanup_deadline", runtime);
            }
            Outcome::Waiting => self.job = Some(job),
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
                record.close(Some("dependency_unavailable"));
                self.schedule_lookup(record, job.caller, now);
            }
            Phase::LifecyclePrepare | Phase::LifecycleLookup => {
                record.close(Some("dependency_unavailable"));
                if let Some((connection, action_ticket)) = job.network {
                    self.fail_network_action(
                        record,
                        connection,
                        action_ticket,
                        "dependency_unavailable",
                    );
                }
                if job.phase == Phase::LifecyclePrepare {
                    // Broker settlement cannot roll back a committed coordinator
                    // transition. Recover its exact admission before cleanup.
                    self.schedule_lifecycle_lookup(
                        record,
                        job.caller,
                        None,
                        now.saturating_add(CLEANUP_BUDGET_MS),
                    );
                } else {
                    record.state = State::CleanupFailed;
                    record.cleanup_error = Some("lifecycle_prepare_lookup_unavailable");
                    complete_caller(record, job.caller, runtime);
                }
            }
            Phase::Arm => {
                if let Some((connection, action_ticket)) = job.network {
                    self.fail_network_action(
                        record,
                        connection,
                        action_ticket,
                        "dependency_unavailable",
                    );
                } else {
                    record.close(Some("network_disconnected"));
                    self.schedule_stop(record, job.caller, now);
                }
            }
            Phase::Stop => {
                self.schedule_status_until(record, job.caller, job.network, job.deadline)
            }
            Phase::Find | Phase::Status => {
                record.state = State::CleanupFailed;
                record.cleanup_error = Some("dependency_unavailable");
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
            Phase::Status => self.status_result(record, job, &payload, now, runtime),
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
        let operation = format!("{}-lifecycle-prepare", record.operation());
        if payload["operation_id"] != operation {
            self.reject_lifecycle_preparation(record, network, job.caller, runtime);
            return;
        }
        if payload["status"] == "pending" {
            self.schedule_lifecycle_lookup(record, job.caller, network, job.deadline);
            return;
        }
        if payload["status"] == "rejected" {
            record.lifecycle_prepare_outstanding = false;
            if let Some((connection, action_ticket)) = network {
                self.fail_network_action(
                    record,
                    connection,
                    action_ticket,
                    "lifecycle_prepare_rejected",
                );
            } else {
                record.close(Some("lifecycle_prepare_rejected"));
            }
            self.schedule_stop(record, job.caller, now);
            return;
        }
        let target: Option<amaterasu_extension_api::LifecycleTarget> =
            serde_json::from_value(payload["target"].clone()).ok();
        let candidate: Option<amaterasu_extension_api::CaptureBinding> =
            serde_json::from_value(payload["candidate"].clone()).ok();
        let valid_target = match (&record.config.lifecycle_session_target, &target) {
            (
                Some(amaterasu_extension_api::LifecycleTarget::Session {
                    host_epoch: expected_epoch,
                    session_id: expected_session,
                    ..
                }),
                Some(amaterasu_extension_api::LifecycleTarget::Stream {
                    host_epoch,
                    session_id,
                    stream_id,
                    generation,
                }),
            ) => {
                host_epoch == expected_epoch
                    && session_id == expected_session
                    && !stream_id.is_empty()
                    && *generation > 0
            }
            _ => false,
        };
        let returned_mode = serde_json::from_value::<amaterasu_extension_api::CaptureBindingMode>(
            payload
                .get("capture_binding_mode")
                .cloned()
                .unwrap_or(json!("managed")),
        )
        .ok();
        let valid_candidate = if record.config.lifecycle_capture_mode.is_external() {
            payload["candidate"].is_null()
                && payload["required_consumers"] == json!(["capture"])
                && (record.config.lifecycle_intent.as_deref() != Some("join")
                    || target.as_ref().is_some_and(|target| {
                        super::lifecycle_binding::valid_external_join(target, payload)
                    }))
        } else if record.config.lifecycle_intent.as_deref() == Some("join") {
            candidate
                .as_ref()
                .zip(target.as_ref())
                .is_some_and(|(binding, target)| {
                    super::lifecycle_binding::valid_join_binding(binding, target, payload)
                })
        } else {
            candidate
            .as_ref()
            .zip(record.negotiated_video)
            .zip(record.negotiated_video_stream_format)
            .is_some_and(|((binding, video), format)| {
                matches!(&target, Some(amaterasu_extension_api::LifecycleTarget::Stream { generation, .. }) if binding.generation == *generation)
                    && binding.geometry_revision > 0
                    && binding.plan_revision > 0
                    && binding.mode.width == video.width
                    && binding.mode.height == video.height
                    && binding.mode.refresh_millihz == video.fps * 1000
                    && binding.mode.hdr
                        == (format.dynamic_range == amaterasu_module_sdk::DynamicRange::Hdr)
            })
        };
        if payload["status"] != "succeeded"
            || !valid_target
            || !valid_candidate
            || returned_mode != Some(record.config.lifecycle_capture_mode)
        {
            self.reject_lifecycle_preparation(record, network, job.caller, runtime);
            return;
        }
        let source =
            match preparation_source(record, &payload["target"], payload.get("shared_source")) {
                Some(source) => source,
                None => {
                    self.reject_lifecycle_preparation(record, network, job.caller, runtime);
                    return;
                }
            };
        record.lifecycle_target = Some(payload["target"].clone());
        record.lifecycle_candidate = Some(payload["candidate"].clone());
        record.lifecycle_source_target = Some(source);
        record.lifecycle_prepare_outstanding = false;
        if let Some((connection, action_ticket)) = network {
            let Some(start) = record.authenticated_lifecycle_start_payload() else {
                self.fail_network_action(record, connection, action_ticket, "negotiation_failed");
                return;
            };
            self.schedule_actual_arm(record, start, connection, action_ticket, job.deadline);
        } else {
            record.close(Some("network_disconnected"));
            self.schedule_stop(record, job.caller, now);
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
                "lifecycle_prepare_rejected",
            );
        } else {
            record.close(Some("lifecycle_prepare_rejected"));
            record.state = State::CleanupFailed;
            record.cleanup_error = Some("lifecycle_prepare_unconfirmed");
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
            let negotiation = Negotiation::new(
                TrustedLaunch {
                    epoch: record.id,
                    rtsp_target: record.url.clone(),
                    session_token: record.session_token.clone(),
                    encrypted_rtsp: true,
                    audio_port: audio,
                    video_port: video,
                    control_port: control,
                    control_connect_data: record.config.connect_data,
                    udp_discovery: record.udp_discovery.clone(),
                },
                record.capabilities(),
            )
            .map_err(|_| "invalid_prepare_response")?;
            self.transport.as_mut().ok_or("disabled")?.add_ticket(
                record.id,
                record.config.peer_ip,
                &key,
            )?;
            record.session = Some(session);
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
            record.close(Some(code));
            self.revoke_ticket(record.id);
            self.schedule_cleanup(record, job.caller, now);
            return;
        }
        let accepted = job.caller.is_some_and(|request| {
            rpc::active(runtime, request) && rpc::complete(runtime, request, record.response())
        });
        if !accepted {
            record.close(Some("completion_unavailable"));
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
            record.state = State::CleanupFailed;
            record.cleanup_error = Some("invalid_lookup_response");
            complete_caller(record, job.caller, runtime);
            return;
        };
        record.session = Some(session);
        record.needs_lookup = false;
        self.schedule_stop(record, job.caller, now);
    }

    pub(super) fn armed(&mut self, record: &mut Record, job: Job, payload: &Value, now: u64) {
        let state = stream_state(payload);
        let valid = payload["ok"] == true
            && response_session(payload) == record.session
            && matches!(state, Some("armed" | "starting" | "active"))
            && payload["cleanup_error"].is_null();
        if !valid {
            eprintln!(
                "amaterasu rtsp arm rejected state={} code={} error={}",
                safe_provider_code(&payload["state"]),
                safe_provider_code(&payload["code"]),
                safe_provider_code(&payload["error"]),
            );
        }
        if let Some(state) = state {
            record.stream_state = Some(state.into());
        }
        let Some((connection, action_ticket)) = job.network else {
            record.close(Some("network_disconnected"));
            self.schedule_stop(record, job.caller, now);
            return;
        };
        let reply = record
            .negotiation
            .as_mut()
            .and_then(|negotiation| negotiation.complete(record.id, action_ticket, valid).ok());
        let replied = if let Some(reply) = reply {
            self.reply_bytes(connection, &reply)
        } else {
            self.revoke_ticket(record.id);
            false
        };
        if valid && replied {
            record.state = State::Playing;
            record.expires = u64::MAX;
            record.next_status = now.saturating_add(STATUS_INTERVAL_MS);
        } else {
            record.close(Some("start_rejected"));
            record.revoke_after = replied.then_some(connection);
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
            record.state = State::CleanupFailed;
            record.cleanup_error = Some("invalid_stop_response");
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
        if response_session(payload) != record.session || payload["ok"] != true || state.is_none() {
            record.close(Some("stream_status_unavailable"));
            if job.network.is_some() || job.caller.is_some() {
                record.state = State::CleanupFailed;
                record.cleanup_error = Some("invalid_status_response");
                complete_caller(record, job.caller, runtime);
            } else {
                self.schedule_stop(record, None, now);
            }
            return;
        }
        record.stream_state = state.map(str::to_owned);
        if confirm_terminal(record, payload) {
            record.authenticated_disconnect_reason =
                authenticated_disconnect_reason(record, payload);
            if record.state != State::Closing {
                record.close(Some("stream_failed"));
            }
            self.finish_cleanup_reply(record, job, runtime);
        } else if matches!(state, Some("cleanup_failed" | "failed" | "cancelled"))
            || !payload["cleanup_error"].is_null()
        {
            if record.state == State::Closing {
                record.state = State::CleanupFailed;
                record.cleanup_error = Some("stream_cleanup_failed");
                complete_caller(record, job.caller, runtime);
            } else {
                record.close(Some("stream_failed"));
                self.revoke_ticket(record.id);
                self.schedule_stop(record, None, now);
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
                .and_then(|negotiation| negotiation.complete(record.id, action_ticket, true).ok());
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
        record.close(Some("dependency_unavailable"));
        record.state = State::CleanupFailed;
        record.cleanup_error = Some(code);
        record.uncertain_phase = Some(job.phase);
        record.uncertain = Some(job.call);
        self.revoke_ticket(record.id);
        complete_caller(record, job.caller, runtime);
    }

    pub(super) fn schedule_background(&mut self, now: u64) {
        if let Some(ticket) = self
            .records
            .values()
            .find(|record| {
                matches!(record.state, State::Ready | State::Arming) && now >= record.expires
            })
            .map(|record| record.id)
        {
            let record = self.records.get_mut(&ticket).expect("selected record");
            record.close(Some("prepare_expired"));
            self.revoke_ticket(ticket);
        }
        let retry_ticket = self.records.iter_mut().find_map(|(ticket, record)| {
            if record.state != State::CleanupFailed {
                return None;
            }
            if let Some(retry_at) = record.cleanup_retry_at {
                if now < retry_at {
                    return None;
                }
                record.cleanup_retry_at = None;
                record.cleanup_error = None;
                record.state = State::Closing;
                return Some(*ticket);
            }

            if record.cleanup_retry_count >= MAX_CLEANUP_RETRIES {
                return None;
            }

            let delay = CLEANUP_RETRY_DELAYS_MS[usize::from(record.cleanup_retry_count)];
            record.cleanup_retry_count += 1;
            record.cleanup_retry_at = Some(now.saturating_add(delay));
            None
        });
        if let Some(ticket) = retry_ticket {
            let mut record = self.records.remove(&ticket).expect("selected retry record");
            self.schedule_cleanup(&mut record, None, now);
            self.records.insert(ticket, record);
            return;
        }
        if let Some(ticket) = self
            .records
            .values()
            .find(|record| record.state == State::Closing)
            .map(|record| record.id)
        {
            let mut record = self.records.remove(&ticket).expect("selected record");
            self.schedule_cleanup(&mut record, None, now);
            self.records.insert(ticket, record);
            return;
        }
        let eligible =
            |record: &&Record| record.state == State::Playing && now >= record.next_status;
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
                deadline: now.saturating_add(CLEANUP_BUDGET_MS),
            });
        } else if record.session.is_some() && record.lifecycle_prepare_outstanding {
            self.schedule_lifecycle_lookup(
                record,
                caller,
                None,
                now.saturating_add(CLEANUP_BUDGET_MS),
            );
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
        let call = Call::for_service(
            "amaterasu.session-lifecycle/1",
            json!({"operation":"lookup", "operation_id":format!("{}-lifecycle-prepare", record.operation())}),
            self.operation_id(record, "lifecycle-lookup"),
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
        let call = Call::new(
            json!({"action":"lookup","operation_id":record.operation()}),
            self.operation_id(record, "lookup"),
            None,
        );
        self.job = Some(Job {
            ticket: record.id,
            phase: Phase::Find,
            call,
            caller,
            network: None,
            deadline: now.saturating_add(CLEANUP_BUDGET_MS),
        });
    }

    pub(super) fn schedule_stop(&mut self, record: &Record, caller: Option<u64>, now: u64) {
        self.job = Some(Job {
            ticket: record.id,
            phase: Phase::Stop,
            call: self.stop_call(record),
            caller,
            network: None,
            deadline: now.saturating_add(CLEANUP_BUDGET_MS),
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
    }

    pub(super) fn schedule_status_until(
        &mut self,
        record: &Record,
        caller: Option<u64>,
        network: Option<(u64, u64)>,
        deadline: u64,
    ) {
        let call = Call::new(
            json!({"action":"status","session":record.session}),
            self.operation_id(record, "status"),
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

    pub(super) fn stop_call(&mut self, record: &Record) -> Call {
        Call::new(
            record.stop_payload(),
            self.operation_id(record, "stop"),
            None,
        )
    }

    pub(super) fn operation_id(&mut self, record: &Record, phase: &str) -> String {
        self.sequence = self.sequence.saturating_add(1);
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
