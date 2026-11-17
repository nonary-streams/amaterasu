use super::*;
use amaterasu_module_sdk::stream_lifecycle::{
    LifecycleLayer, StreamFailure, StreamFailureDecision, StreamFailureEffect, StreamFailureFact,
    StreamFailureHandler,
};

#[derive(Debug)]
pub(super) enum NetworkCompletion {
    NegotiationOnly,
    NativeStart,
}

impl Controller {
    pub(super) fn mark_noncritical_connection(&mut self, connection: u64) {
        if self
            .transport
            .as_ref()
            .is_some_and(|t| t.has_connection(connection))
        {
            self.noncritical_connections.insert(connection);
        }
    }

    pub(super) fn poll_transport(&mut self, now: u64) {
        let request_budget = MAX_PENDING_NETWORK.saturating_sub(self.pending_network.len())
            + usize::from(self.job.is_none() && self.pending_network.is_empty());
        let events = self
            .transport
            .as_mut()
            .map(|transport| transport.poll_with_request_budget(now, request_budget))
            .unwrap_or_default();
        self.noncritical_connections.retain(|connection|
            self.transport.as_ref().is_some_and(|t| t.has_connection(*connection))
            || events.iter().any(|event| matches!(event, Event::Disconnected {connection: id, ..} if id == connection)));
        for event in events {
            match event {
                Event::Request {
                    connection,
                    ticket,
                    request,
                } => {
                    if self.job.is_none() && self.pending_network.is_empty() {
                        self.handle_network(connection, ticket, request, now);
                    } else {
                        debug_assert!(self.pending_network.len() < MAX_PENDING_NETWORK);
                        self.pending_network.push_back(PendingNetwork {
                            connection,
                            ticket,
                            request,
                        });
                    }
                }
                Event::Disconnected {
                    connection,
                    ticket,
                    reason,
                } => {
                    eprintln!(
                        "amaterasu rtsp connection closed reason={reason} authenticated={}",
                        ticket.is_some()
                    );
                    self.disconnected(connection, ticket, reason);
                }
            }
        }
    }

    pub(super) fn process_pending_network(&mut self, now: u64) {
        if let Some(pending) = self.pending_network.pop_front() {
            self.handle_network(pending.connection, pending.ticket, pending.request, now);
        }
    }

    pub(super) fn handle_network(
        &mut self,
        connection: u64,
        ticket: u64,
        request: Request,
        now: u64,
    ) {
        eprintln!("amaterasu rtsp request method={:?}", request.method);
        let Some(mut record) = self.records.remove(&ticket) else {
            self.reply_status(
                connection,
                request.cseq,
                454,
                amaterasu_module_sdk::stream_lifecycle::StreamFailure::requirement_failed(
                    "session_unavailable",
                ),
            );
            return;
        };
        if !matches!(record.state, State::Ready | State::Playing) {
            self.reply_status(
                connection,
                request.cseq,
                455,
                amaterasu_module_sdk::stream_lifecycle::StreamFailure::request_unchanged(
                    "request_wrong_state",
                ),
            );
            self.records.insert(ticket, record);
            return;
        }
        if record.state == State::Ready
            && self.shutdown_deadline.is_some()
            && request.method == amaterasu_gamestream::rtsp::Method::Play
        {
            // Existing prepared ownership remains available until DAG-ordered
            // quiescence. The shutdown announcement admits no new native start.
            self.mark_noncritical_connection(connection);
            self.reply_status(
                connection,
                request.cseq,
                503,
                amaterasu_module_sdk::stream_lifecycle::StreamFailure::requirement_failed(
                    "module_stopping",
                ),
            );
            self.records.insert(ticket, record);
            return;
        }
        if record.state == State::Ready && now >= record.expires {
            let issue = StreamFailure::requirement_failed("prepare_expired").at(
                record.config.operation_id.clone(),
                LifecycleLayer::Rtsp,
                record.state.phase(),
            );
            if let StreamFailureDecision::Terminate(permit) =
                StreamFailureHandler::adjudicate_report(
                    self.failure_policy.as_ref(),
                    issue,
                    &record.failure_scope,
                )
            {
                self.reject_status(
                    &mut record,
                    connection,
                    request.cseq,
                    454,
                    permit,
                    "prepare_expired",
                );
            }
            self.records.insert(ticket, record);
            return;
        }
        let result = record
            .negotiation
            .as_mut()
            .ok_or(amaterasu_gamestream::rtsp::negotiation::Error::InvalidLaunch)
            .and_then(|negotiation| negotiation.handle(ticket, &request));
        match result {
            Ok(NegotiationStep::Reply(reply)) => {
                self.deliver_replayable_reply(&mut record, connection, &reply);
            }
            Ok(NegotiationStep::Action {
                ticket: action_ticket,
                action: Action::Announce(announce),
            }) => {
                let success = record.accept_announce(&announce);
                let completed = self.finish_network_result(
                    &mut record,
                    connection,
                    action_ticket,
                    if success {
                        Ok(NetworkCompletion::NegotiationOnly)
                    } else {
                        Err(StreamFailure::requirement_failed("configuration_mismatch"))
                    },
                    None,
                );
                if completed && success {
                    for adjustment in &record.adjustments {
                        if self.pending_adjustment_logs.len() < 64 {
                            self.pending_adjustment_logs.push_back(
                                json!({
                                    "event":"stream_adjustment", "stream":record.config.history_activity_id.as_deref().unwrap_or(&record.config.operation_id),
                                    "code":adjustment.code,"requested":adjustment.requested,
                                    "effective":adjustment.effective
                                })
                                .to_string(),
                            );
                        }
                    }
                    self.pending_ignored_client_features |= announce.client_feature_flags
                        & !amaterasu_gamestream::sdp::KNOWN_CLIENT_FEATURE_FLAGS;
                }
                if !completed && record.failure.is_some() {
                    record.negotiated_encryption = None;
                    record.client_feature_flags = None;
                    record.negotiated_color = None;
                    record.negotiated_video = None;
                }
            }
            Ok(NegotiationStep::Action {
                ticket: action_ticket,
                action: Action::ExtensionAnnounce(announce),
            }) => {
                let success = record.accept_extension_announce(&announce);
                let completed = self.finish_network_result(
                    &mut record,
                    connection,
                    action_ticket,
                    if success {
                        Ok(NetworkCompletion::NegotiationOnly)
                    } else {
                        Err(StreamFailure::requirement_failed(
                            "unsupported_video_format",
                        ))
                    },
                    None,
                );
                if !completed && record.failure.is_some() {
                    record.extension_announce = None;
                }
            }
            Ok(NegotiationStep::Action {
                ticket: action_ticket,
                action: Action::Play,
            }) => {
                let Some(session) = record.session else {
                    self.fail_network_action(
                        &mut record,
                        connection,
                        action_ticket,
                        amaterasu_module_sdk::stream_lifecycle::StreamFailure::requirement_failed(
                            "session_unavailable",
                        ),
                    );
                    self.records.insert(ticket, record);
                    return;
                };
                let Some(payload) = record.authenticated_start_payload(session) else {
                    self.fail_network_action(
                        &mut record,
                        connection,
                        action_ticket,
                        amaterasu_module_sdk::stream_lifecycle::StreamFailure::requirement_failed(
                            "negotiation_failed",
                        ),
                    );
                    self.records.insert(ticket, record);
                    return;
                };
                self.schedule_arm(&mut record, payload, connection, action_ticket, now);
            }
            Ok(NegotiationStep::Action {
                ticket: action_ticket,
                action: Action::Teardown,
            }) => {
                record.close(None);
                let call = self.stop_call(&record);
                self.job = Some(Job {
                    ticket,
                    phase: Phase::Stop,
                    call,
                    caller: None,
                    network: Some((connection, action_ticket)),
                    deadline: self.cleanup_deadline(now),
                });
            }
            Err(error) => {
                eprintln!(
                    "amaterasu rtsp rejection method={:?} status={} reason={error:?}",
                    request.method,
                    error.status()
                );
                let failure = if error.leaves_request_unchanged() {
                    StreamFailure::request_unchanged("negotiation_request_rejected")
                        .because(error.code())
                } else {
                    StreamFailure::requirement_failed(error.code())
                };
                let report = failure.at(
                    record.failure_scope.operation(),
                    LifecycleLayer::Rtsp,
                    "negotiate",
                );
                match StreamFailureHandler::adjudicate_report(
                    self.failure_policy.as_ref(),
                    report,
                    &record.failure_scope,
                ) {
                    StreamFailureDecision::Terminate(permit) => {
                        self.reject_status(
                            &mut record,
                            connection,
                            request.cseq,
                            error.status(),
                            permit,
                            error.code(),
                        );
                    }
                    StreamFailureDecision::Continue(continuation) => {
                        if let Ok(reply) =
                            serialize_response(error.status(), request.cseq, &[], &[])
                        {
                            self.deliver_scoped_reply(&mut record, connection, &reply);
                        }
                        record.issues.push(continuation.into_issue());
                    }
                    StreamFailureDecision::Invalid(_) => {
                        // An invalid diagnostic cannot preserve authorization
                        // after a required or security-critical failure.
                        if record.close(Some(StreamFailure::requirement_failed(
                            "invalid_failure_classification",
                        ))) {
                            self.revoke_ticket(record.id);
                        }
                    }
                }
            }
        }
        self.records.insert(ticket, record);
    }

    pub(super) fn schedule_arm(
        &mut self,
        record: &mut Record,
        payload: Value,
        connection: u64,
        action_ticket: u64,
        now: u64,
    ) {
        let deadline = record.begin_arm(now);
        if let Some(target) = &record.config.lifecycle_session_target {
            let Some(input_required) = record.lifecycle_input_required else {
                self.fail_network_action(
                    record,
                    connection,
                    action_ticket,
                    amaterasu_module_sdk::stream_lifecycle::StreamFailure::requirement_failed(
                        "lifecycle_input_policy_unverified",
                    ),
                );
                return;
            };
            let Some(video) = record.negotiated_video_mode() else {
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
            let operation = format!("{}-lifecycle-prepare", record.operation());
            record.lifecycle_prepare_outstanding = true;
            let mut request = json!({"operation":"prepare_stream", "operation_id":operation,
                "target":target, "intent":record.config.lifecycle_intent,
                "client_context":record.config.lifecycle_client_context,
                "settings_revision":record.config.lifecycle_settings_revision,
                "requested_video":video,
                "consumer_requirements":if input_required && record.config.lifecycle_capture_mode.is_managed() { vec!["capture", "input"] } else { vec!["capture"] }});
            if let Some(source) = &record.config.borrowed_capture {
                request["borrowed_capture"] = json!(source);
            }
            let operation = self.operation_id(record, "lifecycle-prepare");
            let call = Call::for_service(
                self.settings.lifecycle_service.as_deref().unwrap_or(""),
                request,
                operation,
                None,
            );
            self.job = Some(Job {
                ticket: record.id,
                phase: Phase::LifecyclePrepare,
                call,
                caller: None,
                network: Some((connection, action_ticket)),
                deadline,
            });
            return;
        }
        self.schedule_actual_arm(record, payload, connection, action_ticket, deadline);
    }

    pub(super) fn schedule_actual_arm(
        &mut self,
        record: &mut Record,
        payload: Value,
        connection: u64,
        action_ticket: u64,
        deadline: u64,
    ) {
        let mut payload = payload;
        payload["prepare_video_before_play"] = json!(true);
        let operation = self.operation_id(record, "arm");
        self.job = Some(Job {
            ticket: record.id,
            phase: Phase::Arm,
            call: Call::for_service(&self.settings.stream_service, payload, operation, None),
            caller: None,
            network: Some((connection, action_ticket)),
            deadline,
        });
    }

    fn reject_status(
        &mut self,
        record: &mut Record,
        connection: u64,
        cseq: u32,
        status: u16,
        permit: amaterasu_module_sdk::stream_lifecycle::StreamTermination,
        code: &'static str,
    ) {
        let Some(replied) = permit.authorize(&record.failure_scope, |_| {
            self.write_status(connection, cseq, status)
        }) else {
            return;
        };
        self.retire_after_reply(record, connection, replied, permit, code);
    }

    fn retire_after_reply(
        &mut self,
        record: &mut Record,
        connection: u64,
        replied: bool,
        permit: amaterasu_module_sdk::stream_lifecycle::StreamTermination,
        code: &'static str,
    ) {
        if !record.close_reported(permit, code) {
            return;
        }
        if replied {
            record.revoke_after = Some(connection);
        } else {
            self.revoke_ticket(record.id);
        }
    }

    fn deliver_replayable_reply(
        &mut self,
        record: &mut Record,
        connection: u64,
        reply: &[u8],
    ) -> bool {
        if self.deliver_scoped_reply(record, connection, reply) {
            return true;
        }
        if record.failure.is_none() {
            record.close(Some(StreamFailure::fallback_applied(
                "negotiation_reply_unavailable",
                "retry_request",
            )));
        }
        false
    }

    fn deliver_scoped_reply(&mut self, record: &mut Record, connection: u64, reply: &[u8]) -> bool {
        self.mark_noncritical_connection(connection);
        match self.deliver_reply(connection, reply) {
            Ok(()) => true,
            Err("connection_not_found") => false,
            Err(code) => {
                // With an unusable authenticated response channel (including
                // nonce exhaustion), another socket cannot recover this ticket.
                if record.close(Some(StreamFailure::requirement_failed(code))) {
                    self.revoke_ticket(record.id);
                }
                false
            }
        }
    }

    #[track_caller]
    pub(super) fn network_failure_decision(
        policy: &dyn amaterasu_module_sdk::stream_lifecycle::StreamFailurePolicy,
        record: &Record,
        action_ticket: u64,
        phase: &str,
        failure: StreamFailure,
        cause: Option<super::lifecycle_issues::Cause<'_>>,
    ) -> StreamFailureDecision {
        let issue = super::lifecycle_issues::with_cause(
            failure.at(
                record.config.operation_id.clone(),
                LifecycleLayer::Rtsp,
                phase,
            ),
            cause,
        );
        match record
            .negotiation
            .as_ref()
            .and_then(|negotiation| negotiation.failure_scope(record.id, action_ticket).ok())
        {
            Some(scope) => StreamFailureHandler::adjudicate_report(policy, issue, scope),
            None => StreamFailureDecision::Invalid(issue.into_diagnostic()),
        }
    }

    /// The one completion boundary for lower-layer action results. Lower
    /// layers report failures; only the shared handler decides terminal vs
    /// operation-scoped refusal before protocol state or wire bytes change.
    #[track_caller]
    pub(super) fn finish_network_result(
        &mut self,
        record: &mut Record,
        connection: u64,
        action_ticket: u64,
        outcome: Result<NetworkCompletion, StreamFailure>,
        cause: Option<super::lifecycle_issues::Cause<'_>>,
    ) -> bool {
        if record.admission() != Admission::Open {
            return false;
        }
        let Some(_) = record.negotiation.as_ref() else {
            let failure = outcome
                .err()
                .unwrap_or_else(|| StreamFailure::requirement_failed("negotiation_failed"));
            if record.close(Some(failure)) {
                self.revoke_ticket(record.id);
            }
            return false;
        };
        if record
            .negotiation
            .as_ref()
            .is_none_or(|negotiation| negotiation.failure_scope(record.id, action_ticket).is_err())
        {
            // This completion belongs to an action already retired or replaced.
            return false;
        }
        if let Ok(completion) = outcome {
            let native_start = matches!(completion, NetworkCompletion::NativeStart);
            let reply = record
                .negotiation
                .as_mut()
                .expect("known negotiation")
                .complete(record.id, action_ticket);
            let reply = match reply {
                Ok(reply) => reply,
                Err(_) => {
                    if record.close(Some(StreamFailure::requirement_failed(
                        "negotiation_failed",
                    ))) {
                        self.revoke_ticket(record.id);
                    }
                    return false;
                }
            };
            if !native_start {
                return self.deliver_replayable_reply(record, connection, &reply);
            }
            if self.reply_bytes(connection, &reply) {
                return true;
            }
            if record.close(Some(StreamFailure::requirement_failed(
                "response_unavailable",
            ))) {
                self.revoke_ticket(record.id);
            }
            return false;
        }
        let failure = outcome.expect_err("success returned above");
        let code = failure.code();
        let decision = Self::network_failure_decision(
            self.failure_policy.as_ref(),
            record,
            action_ticket,
            record.state.phase(),
            failure,
            cause,
        );
        self.apply_network_failure(record, connection, action_ticket, decision, code);
        false
    }

    pub(super) fn finish_provider_failure(
        &mut self,
        record: &mut Record,
        connection: u64,
        action_ticket: u64,
        (mut fact, effect): (StreamFailureFact, StreamFailureEffect),
        code: &'static str,
    ) -> bool {
        if record.admission() != Admission::Open {
            return false;
        }
        let Some(scope) = record
            .negotiation
            .as_ref()
            .and_then(|negotiation| negotiation.failure_scope(record.id, action_ticket).ok())
        else {
            return false;
        };
        // The caller verified the provider operation. Bind this fact to
        // the exact pending RTSP action before applying its effects.
        fact.operation.clone_from(&record.config.operation_id);
        let decision = StreamFailureHandler::adjudicate(
            self.failure_policy.as_ref(),
            fact,
            effect,
            scope,
            LifecycleLayer::Rtsp,
            code,
        );
        self.apply_network_failure(record, connection, action_ticket, decision, code)
    }

    fn apply_network_failure(
        &mut self,
        record: &mut Record,
        connection: u64,
        action_ticket: u64,
        decision: StreamFailureDecision,
        code: &'static str,
    ) -> bool {
        let negotiation = record.negotiation.as_mut().expect("known negotiation");
        let continued = matches!(decision, StreamFailureDecision::Continue(_));
        match decision {
            StreamFailureDecision::Terminate(permit) => {
                let Ok((reply, permit)) = negotiation.reject(record.id, action_ticket, permit)
                else {
                    return false;
                };
                let replied = self.reply_bytes(connection, &reply);
                self.retire_after_reply(record, connection, replied, permit, code);
            }
            StreamFailureDecision::Continue(continuation) => {
                if negotiation
                    .failure_scope(record.id, action_ticket)
                    .map_or(true, |scope| !continuation.matches_scope(scope))
                {
                    return false;
                }
                let reply = negotiation.refuse(record.id, action_ticket, &continuation);
                record.issues.push(continuation.into_issue());
                match reply {
                    Ok(reply) => {
                        self.deliver_scoped_reply(record, connection, &reply);
                    }
                    Err(_) => {
                        if record.close(Some(StreamFailure::requirement_failed(
                            "negotiation_failed",
                        ))) {
                            self.revoke_ticket(record.id);
                        }
                    }
                }
            }
            StreamFailureDecision::Invalid(_) => {
                if record.close(Some(StreamFailure::requirement_failed(
                    "invalid_failure_classification",
                ))) {
                    self.revoke_ticket(record.id);
                }
            }
        }
        continued && record.failure.is_none()
    }

    #[track_caller]
    pub(super) fn fail_network_action(
        &mut self,
        record: &mut Record,
        connection: u64,
        action_ticket: u64,
        failure: StreamFailure,
    ) {
        self.finish_network_result(record, connection, action_ticket, Err(failure), None);
    }

    pub(super) fn disconnected(
        &mut self,
        connection: u64,
        ticket: Option<u64>,
        reason: &'static str,
    ) {
        let noncritical = self.noncritical_connections.remove(&connection);
        let queued_request = self
            .pending_network
            .iter()
            .any(|pending| pending.connection == connection);
        self.pending_network
            .retain(|pending| pending.connection != connection);
        let Some(ticket) = ticket else {
            return;
        };
        let Some(mut record) = self.records.remove(&ticket) else {
            return;
        };
        if record.revoke_after == Some(connection) {
            record.revoke_after = None;
            self.revoke_ticket(ticket);
            if record.cleanup_confirmed {
                finish_record(&mut record);
            }
            self.records.insert(ticket, record);
            return;
        }
        if reason == "response_complete"
            || matches!(
                record.state,
                State::Closing | State::CleanupFailed | State::Closed | State::Failed
            )
        {
            self.records.insert(ticket, record);
            return;
        }
        let pending_mutation = self
            .job
            .iter_mut()
            .chain(self.retiring_jobs.values_mut())
            .chain(self.parked_job.iter_mut())
            .find(|job| {
                job.ticket == ticket && job.network.is_some_and(|value| value.0 == connection)
            });
        let io_loss = matches!(
            reason,
            "peer_closed"
                | "peer_disconnected"
                | "tcp_read_failed"
                | "tcp_write_failed"
                | "connection_deadline"
        );
        if (noncritical || queued_request) && io_loss && pending_mutation.is_none() {
            if queued_request {
                let issue = StreamFailure::request_unchanged("request_connection_lost")
                    .because(reason)
                    .at(
                        record.config.operation_id.clone(),
                        LifecycleLayer::Rtsp,
                        "queued_request",
                    );
                record.issues.push(issue.into_diagnostic());
            }
            self.records.insert(ticket, record);
            return;
        }
        if record.close(Some(
            StreamFailure::requirement_failed("network_disconnected").because(reason),
        )) {
            if let Some(job) = pending_mutation {
                job.call.cancel();
                job.network = None;
            }
            self.revoke_ticket(ticket);
        }
        self.records.insert(ticket, record);
    }
}
