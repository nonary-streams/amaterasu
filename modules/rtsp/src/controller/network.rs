use super::*;

impl Controller {
    pub(super) fn poll_transport(&mut self, now: u64) {
        let events = self
            .transport
            .as_mut()
            .map(|transport| transport.poll(now))
            .unwrap_or_default();
        for event in events {
            match event {
                Event::Request {
                    connection,
                    ticket,
                    request,
                } => {
                    if self.job.is_none() && self.pending_network.is_empty() {
                        self.handle_network(connection, ticket, request, now);
                    } else if self.pending_network.len() < MAX_PENDING_NETWORK {
                        self.pending_network.push_back(PendingNetwork {
                            connection,
                            ticket,
                            request,
                        });
                    } else {
                        self.reply_status(connection, request.cseq, 503);
                    }
                }
                Event::Disconnected {
                    connection,
                    ticket,
                    reason,
                } => self.disconnected(connection, ticket, reason),
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
        let Some(mut record) = self.records.remove(&ticket) else {
            self.reply_status(connection, request.cseq, 454);
            return;
        };
        if !matches!(record.state, State::Ready | State::Playing) {
            self.reply_status(connection, request.cseq, 455);
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
                if !self.reply_bytes(connection, &reply) {
                    record.close(Some("response_unavailable"));
                    self.revoke_ticket(record.id);
                }
            }
            Ok(NegotiationStep::Action {
                ticket: action_ticket,
                action: Action::Announce(announce),
            }) => {
                let success = record.accept_announce(&announce);
                let completed = self.finish_immediate_action(
                    &mut record,
                    connection,
                    action_ticket,
                    success,
                    "configuration_mismatch",
                );
                if !completed {
                    record.negotiated_encryption = None;
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
                        "session_unavailable",
                    );
                    self.records.insert(ticket, record);
                    return;
                };
                let Some(payload) = record.authenticated_start_payload(session) else {
                    self.fail_network_action(
                        &mut record,
                        connection,
                        action_ticket,
                        "negotiation_failed",
                    );
                    self.records.insert(ticket, record);
                    return;
                };
                record.state = State::Arming;
                let call = Call::new(payload, self.operation_id(&record, "arm"), None);
                self.job = Some(Job {
                    ticket,
                    phase: Phase::Arm,
                    call,
                    caller: None,
                    network: Some((connection, action_ticket)),
                    deadline: record.expires,
                });
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
                    deadline: now.saturating_add(CLEANUP_BUDGET_MS),
                });
            }
            Err(error) => {
                let replied = self.reply_status(connection, request.cseq, error.status());
                record.close(Some("negotiation_failed"));
                if replied {
                    record.revoke_after = Some(connection);
                } else {
                    self.revoke_ticket(record.id);
                }
            }
        }
        self.records.insert(ticket, record);
    }

    pub(super) fn finish_immediate_action(
        &mut self,
        record: &mut Record,
        connection: u64,
        action_ticket: u64,
        success: bool,
        failure: &'static str,
    ) -> bool {
        let reply = record
            .negotiation
            .as_mut()
            .and_then(|negotiation| negotiation.complete(record.id, action_ticket, success).ok());
        if let Some(reply) = reply {
            if !self.reply_bytes(connection, &reply) {
                record.close(Some("response_unavailable"));
                self.revoke_ticket(record.id);
                return false;
            }
        } else {
            record.close(Some("negotiation_failed"));
            self.revoke_ticket(record.id);
            return false;
        }
        if !success {
            record.close(Some(failure));
            record.revoke_after = Some(connection);
            return false;
        }
        true
    }

    pub(super) fn fail_network_action(
        &mut self,
        record: &mut Record,
        connection: u64,
        action_ticket: u64,
        failure: &'static str,
    ) {
        let reply = record
            .negotiation
            .as_mut()
            .and_then(|negotiation| negotiation.complete(record.id, action_ticket, false).ok());
        if let Some(reply) = reply {
            if self.reply_bytes(connection, &reply) {
                record.revoke_after = Some(connection);
            } else {
                self.revoke_ticket(record.id);
            }
        } else {
            self.revoke_ticket(record.id);
        }
        record.close(Some(failure));
    }

    pub(super) fn disconnected(
        &mut self,
        connection: u64,
        ticket: Option<u64>,
        reason: &'static str,
    ) {
        self.pending_network
            .retain(|pending| pending.connection != connection);
        let Some(ticket) = ticket else {
            return;
        };
        let Some(mut record) = self.records.remove(&ticket) else {
            return;
        };
        if reason == "response_complete" {
            if record.revoke_after == Some(connection) {
                record.revoke_after = None;
                self.revoke_ticket(ticket);
                if record.cleanup_confirmed {
                    finish_record(&mut record);
                }
            }
            self.records.insert(ticket, record);
            return;
        }
        let pending_mutation = self.job.as_mut().filter(|job| {
            job.ticket == ticket && job.network.is_some_and(|value| value.0 == connection)
        });
        if let Some(job) = pending_mutation {
            job.call.cancel();
            job.network = None;
        }
        record.close(Some("network_disconnected"));
        self.revoke_ticket(ticket);
        self.records.insert(ticket, record);
    }
}
