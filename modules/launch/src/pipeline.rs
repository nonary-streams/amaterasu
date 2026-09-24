use crate::{
    SERVICE,
    protocol::*,
    records::{CatalogSelection, Records},
    responses,
    workflow::{Step, Workflow, first_cleanup_step},
};
use amaterasu_module_sdk::{Command, ModuleResult, Response};
use serde_json::Value;
use std::{collections::BTreeMap, time::Instant};

type Transport = ModuleResult<Response>;
pub struct Pipeline {
    records: Records,
    job: Option<Workflow>,
    sequence: u64,
    monitor_after: u64,
    ingress_since_monitor: u8,
    environment: BTreeMap<String, String>,
}
impl Pipeline {
    pub fn new(environment: BTreeMap<String, String>) -> Result<Self, &'static str> {
        Ok(Self {
            records: Records::new()?,
            job: None,
            sequence: 0,
            monitor_after: 0,
            ingress_since_monitor: 0,
            environment,
        })
    }
    pub fn tick(&mut self, mut send: impl FnMut(Command) -> Transport) -> ModuleResult<()> {
        if let Some(mut job) = self.job.take() {
            if let Some(request) = job.pending.take() {
                match send(Command::TakeResult { request }) {
                    Ok(Response::Pending) => {
                        job.pending = Some(request);
                        self.job = Some(job);
                        return Ok(());
                    }
                    Ok(Response::Result { payload }) => {
                        self.consume(&mut job, payload, &mut send)?
                    }
                    _ => {
                        let record = self
                            .records
                            .values
                            .get_mut(&job.launch_id)
                            .expect("workflow record");
                        record.outstanding_request = Some(request);
                        if !job.cleanup {
                            record.cleanup_outcome = Outcome::Failed;
                        }
                        if self.settle_outstanding(job.launch_id, &mut send) {
                            self.dependency_failed(&mut job, "dependency_unavailable", &mut send)?;
                        } else {
                            self.cleanup_failed(
                                &job,
                                "request_cancellation_unconfirmed",
                                &mut send,
                            )?;
                        }
                    }
                }
            } else {
                self.dispatch(&mut job, &mut send)?;
            }
            return Ok(());
        }
        // Background exit cleanup gets a turn even under continuous ingress.
        if self.ingress_since_monitor >= 8 {
            self.ingress_since_monitor = 0;
            self.schedule_monitor();
            if self.job.is_some() {
                return Ok(());
            }
        }
        match send(Command::Poll {})? {
            Response::Request {
                id,
                caller,
                initiator,
                service,
                payload,
                remaining_ms,
            } => {
                self.ingress_since_monitor = self.ingress_since_monitor.saturating_add(1);
                if service != SERVICE {
                    return complete(&mut send, id, error("invalid_service"));
                }
                let operation = serde_json::from_value::<Operation>(payload)
                    .map_err(|_| "invalid_request")
                    .and_then(|operation| {
                        operation.validate()?;
                        Ok(operation)
                    });
                match operation {
                    Ok(operation) => {
                        self.receive(id, &caller, &initiator, remaining_ms, operation, &mut send)
                    }
                    Err(code) => complete(&mut send, id, error(code)),
                }
            }
            Response::Pending => {
                self.schedule_monitor();
                Ok(())
            }
            _ => Err("launch poll failed".into()),
        }
    }
    fn receive(
        &mut self,
        request: u64,
        caller: &str,
        initiator: &str,
        remaining_ms: u64,
        operation: Operation,
        send: &mut impl FnMut(Command) -> Transport,
    ) -> ModuleResult<()> {
        match operation {
            Operation::Launch {
                operation_id,
                app_id,
            } => match self.records.start(
                caller,
                initiator,
                operation_id,
                app_id,
                CatalogSelection::Initiator,
            ) {
                Ok((id, true)) => {
                    self.job = Some(Workflow::launch(id, request, remaining_ms));
                    Ok(())
                }
                Ok((id, false)) => complete(send, request, self.records.values[&id].response()),
                Err(code) => complete(send, request, error(code)),
            },
            Operation::LaunchExport {
                operation_id,
                app_id,
            } => match self.records.start(
                caller,
                initiator,
                operation_id,
                app_id,
                CatalogSelection::Exported,
            ) {
                Ok((id, true)) => {
                    self.job = Some(Workflow::launch(id, request, remaining_ms));
                    Ok(())
                }
                Ok((id, false)) => complete(send, request, self.records.values[&id].response()),
                Err(code) => complete(send, request, error(code)),
            },
            Operation::Lookup { operation_id } => {
                let response = self
                    .records
                    .owned_operation(caller, &operation_id)
                    .map(|record| record.response())
                    .unwrap_or_else(error);
                complete(send, request, response)
            }
            Operation::Stop { launch_id } => {
                let record = match self.records.owned(caller, launch_id) {
                    Ok(record) => record,
                    Err(code) => return complete(send, request, error(code)),
                };
                if record.state.terminal() {
                    return complete(send, request, record.response());
                }
                let outcome = if record.state == State::CleanupFailed {
                    record.cleanup_outcome
                } else {
                    Outcome::Completed
                };
                let mut job = Workflow::launch(launch_id, request, remaining_ms);
                job.begin_cleanup(
                    self.records
                        .values
                        .get_mut(&launch_id)
                        .expect("owned record"),
                    outcome,
                    None,
                );
                self.job = Some(job);
                Ok(())
            }
            Operation::Status { launch_id } => {
                let response = self
                    .records
                    .owned(caller, launch_id)
                    .map(|record| record.response())
                    .unwrap_or_else(error);
                complete(send, request, response)
            }
            Operation::List { offset, limit } => {
                complete(send, request, self.records.list(caller, offset, limit))
            }
        }
    }
    /// Cancel prevents a queued request from reaching its provider; release removes
    /// its retained broker entry. Already delivered mutations still need lookup.
    /// Keep the ID on failure so explicit stop can retry without freeing a lease.
    fn settle_outstanding(
        &mut self,
        launch_id: u64,
        send: &mut impl FnMut(Command) -> Transport,
    ) -> bool {
        let record = self
            .records
            .values
            .get_mut(&launch_id)
            .expect("workflow record");
        let Some(request) = record.outstanding_request else {
            return true;
        };
        for command in [
            Command::Cancel { request },
            Command::ReleaseResult { request },
        ] {
            match send(command) {
                Ok(Response::Ok) => {}
                // An ephemeral TakeResult may have released the entry before
                // its transport failed; broker expiration also removes entries.
                Ok(Response::Error { code, .. }) if code == "not_found" => {}
                _ => return false,
            }
        }
        record.outstanding_request = None;
        true
    }

    fn schedule_monitor(&mut self) {
        let next = self
            .records
            .values
            .values()
            .filter(|record| record.state == State::Active)
            .find(|record| record.launch_id > self.monitor_after)
            .or_else(|| {
                self.records
                    .values
                    .values()
                    .find(|record| record.state == State::Active)
            });
        if let Some(record) = next {
            self.ingress_since_monitor = 0;
            self.monitor_after = record.launch_id;
            self.job = Some(Workflow::monitor(record.launch_id));
        }
    }
    fn dispatch(
        &mut self,
        job: &mut Workflow,
        send: &mut impl FnMut(Command) -> Transport,
    ) -> ModuleResult<()> {
        if !self.settle_outstanding(job.launch_id, send) {
            return self.cleanup_failed(job, "request_cancellation_unconfirmed", send);
        }
        if job.cleanup
            && job.step == Step::Finish
            && self.records.values[&job.launch_id].session_id.is_none()
        {
            return self.finish(job, send);
        }
        if Instant::now() >= job.deadline {
            return self.dependency_failed(job, "deadline_exceeded", send);
        }
        if !job.cleanup
            && let Some(request) = job.request
            && !matches!(send(Command::CheckRequest { request }), Ok(Response::Ok))
        {
            job.begin_cleanup(
                self.records
                    .values
                    .get_mut(&job.launch_id)
                    .expect("workflow record"),
                Outcome::Cancelled,
                Some("request_cancelled"),
            );
            return self.retain(job);
        }
        let record = self
            .records
            .values
            .get_mut(&job.launch_id)
            .expect("workflow record");
        let (service, payload) = job
            .command(record, &self.environment)
            .map_err(str::to_owned)?;
        // Mark possibly committed mutations before submission. A transport or
        // cancellation error then reconciles by owned lookup, never by respawn.
        if job.step == Step::CreateSession {
            record.session_requested = true;
        }
        if job.step == Step::Spawn {
            record.process_requested = true;
        }
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or("launch sequence exhausted")?;
        let timeout_ms = job
            .deadline
            .saturating_duration_since(Instant::now())
            .as_millis()
            .clamp(1, 5_000) as u64;
        let response = send(Command::Request {
            service: service.into(),
            payload,
            timeout_ms,
            operation_id: format!("{}.{}", record.session_key, self.sequence),
            parent: job.parent(),
        });
        match response {
            Ok(Response::Accepted { id }) => {
                job.pending = Some(id);
                self.retain(job)
            }
            _ => self.dependency_failed(job, "dependency_unavailable", send),
        }
    }
    fn consume(
        &mut self,
        job: &mut Workflow,
        payload: Value,
        send: &mut impl FnMut(Command) -> Transport,
    ) -> ModuleResult<()> {
        if job.cleanup {
            return self.consume_cleanup(job, payload, send);
        }
        if payload["ok"] != true {
            return self.dependency_failed(job, "dependency_rejected", send);
        }
        let record = self
            .records
            .values
            .get_mut(&job.launch_id)
            .expect("workflow record");
        match job.step {
            Step::Catalog => {
                if payload["id"] != record.app_id {
                    return self.dependency_failed(job, "invalid_catalog_response", send);
                }
                match record.catalog_selection {
                    CatalogSelection::Initiator if payload.get("catalog_owner").is_some() => {
                        return self.dependency_failed(job, "invalid_catalog_response", send);
                    }
                    CatalogSelection::Exported => {
                        let Some(owner) = payload["catalog_owner"]
                            .as_str()
                            .filter(|owner| valid_catalog_principal(owner))
                        else {
                            return self.dependency_failed(job, "invalid_catalog_response", send);
                        };
                        if record.set_exported_catalog_owner(owner).is_err() {
                            return self.dependency_failed(job, "invalid_catalog_response", send);
                        }
                    }
                    CatalogSelection::Initiator => {}
                }
                match Executable::from_catalog(&payload["entry"]["launch"]) {
                    Ok(executable) => record.executable = Some(executable),
                    Err(code) => return self.dependency_failed(job, code, send),
                }
                let config_overrides = match payload["entry"].get("config_overrides") {
                    Some(value) => match serde_json::from_value::<
                        crate::protocol::ApplicationConfigOverrides,
                    >(value.clone())
                    {
                        Ok(overrides) if overrides.validate().is_ok() => overrides,
                        _ => return self.dependency_failed(job, "invalid_catalog_response", send),
                    },
                    None => crate::protocol::ApplicationConfigOverrides::default(),
                };
                record.config_overrides =
                    (!config_overrides.is_empty()).then_some(config_overrides);
                job.step = Step::CreateSession;
            }
            Step::CreateSession => {
                let session = match responses::session(&payload, record) {
                    Ok(session) if session["state"] == "preparing" => session,
                    _ => return self.dependency_failed(job, "invalid_session_response", send),
                };
                record.session_id = session["id"].as_u64();
                job.step = Step::Acquire;
            }
            Step::Acquire => {
                let valid = responses::session(&payload, record).is_ok_and(|session| {
                    session["state"] == "preparing"
                        && record
                            .resource
                            .as_deref()
                            .is_some_and(|resource| session["leases"][resource] == "exclusive")
                });
                if !valid {
                    return self.dependency_failed(job, "invalid_session_response", send);
                }
                job.step = Step::Spawn;
            }
            Step::Spawn | Step::AwaitApplication => {
                let (process, state) = match responses::process(&payload, record) {
                    Ok(process) => process,
                    Err(code) => return self.dependency_failed(job, code, send),
                };
                if job.step == Step::Spawn {
                    record.process_id = process["process_id"].as_u64();
                }
                match state {
                    ApplicationState::Preparing | ApplicationState::Stopping => {
                        job.step = Step::AwaitApplication;
                    }
                    ApplicationState::Running => job.step = Step::Activate,
                    ApplicationState::Exited => job.begin_cleanup(record, Outcome::Completed, None),
                    ApplicationState::LaunchFailed => job.begin_cleanup(
                        record,
                        Outcome::Failed,
                        Some("application_launch_failed"),
                    ),
                    ApplicationState::Cancelled => {
                        job.begin_cleanup(record, Outcome::Cancelled, Some("application_cancelled"))
                    }
                    ApplicationState::CleanupFailed(outcome) => {
                        apply_application_outcome(record, outcome);
                        return self.cleanup_failed(job, "application_cleanup_failed", send);
                    }
                }
            }
            Step::Activate => {
                if !responses::session(&payload, record)
                    .is_ok_and(|session| session["state"] == "active")
                {
                    return self.dependency_failed(job, "invalid_session_response", send);
                }
                record.state = State::Active;
                return self.deliver_activation(job, send);
            }
            Step::Monitor => {
                let (_, state) = match responses::process(&payload, record) {
                    Ok(process) => process,
                    Err(code) => return self.dependency_failed(job, code, send),
                };
                match state {
                    ApplicationState::Running | ApplicationState::Stopping => return Ok(()),
                    ApplicationState::Exited => job.begin_cleanup(record, Outcome::Completed, None),
                    ApplicationState::LaunchFailed => job.begin_cleanup(
                        record,
                        Outcome::Failed,
                        Some("application_launch_failed"),
                    ),
                    ApplicationState::Cancelled => {
                        job.begin_cleanup(record, Outcome::Cancelled, Some("application_cancelled"))
                    }
                    ApplicationState::CleanupFailed(outcome) => {
                        apply_application_outcome(record, outcome);
                        return self.cleanup_failed(job, "application_cleanup_failed", send);
                    }
                    ApplicationState::Preparing => {
                        return self.dependency_failed(job, "invalid_application_response", send);
                    }
                }
            }
            _ => return Err("invalid launch workflow".into()),
        }
        self.retain(job)
    }
    fn consume_cleanup(
        &mut self,
        job: &mut Workflow,
        payload: Value,
        send: &mut impl FnMut(Command) -> Transport,
    ) -> ModuleResult<()> {
        let record = self
            .records
            .values
            .get_mut(&job.launch_id)
            .expect("workflow record");
        match job.step {
            Step::FindSession => {
                if payload["ok"] == true {
                    let session = match responses::session(&payload, record) {
                        Ok(session) => session,
                        Err(code) => return self.cleanup_failed(job, code, send),
                    };
                    record.session_id = session["id"].as_u64();
                } else if payload["code"] == "session_not_found" {
                    record.session_requested = false;
                } else {
                    return self.cleanup_failed(job, "session_lookup_failed", send);
                }
                job.step = first_cleanup_step(record);
            }
            Step::FindProcess => {
                if payload["ok"] == true {
                    let (process, _) = match responses::process(&payload, record) {
                        Ok(process) => process,
                        Err(code) => return self.cleanup_failed(job, code, send),
                    };
                    record.process_id = process["process_id"].as_u64();
                } else if payload["code"] == "process_not_found" {
                    record.process_requested = false;
                } else {
                    return self.cleanup_failed(job, "process_lookup_failed", send);
                }
                job.step = first_cleanup_step(record);
            }
            Step::StopSession => {
                // Process termination is still attempted if the session provider
                // fails. Finish-stop must independently acknowledge lease release.
                if payload["ok"] != true {
                    record.cleanup_error = Some("session_stop_failed");
                }
                job.step = if record.process_id.is_some() {
                    Step::Terminate
                } else {
                    Step::Finish
                };
            }
            Step::Terminate | Step::AwaitTermination => {
                if payload["ok"] != true {
                    return self.cleanup_failed(job, "process_termination_failed", send);
                }
                let (_, state) = match responses::process(&payload, record) {
                    Ok(process) => process,
                    Err(code) => return self.cleanup_failed(job, code, send),
                };
                match state {
                    ApplicationState::Exited
                    | ApplicationState::LaunchFailed
                    | ApplicationState::Cancelled => job.step = Step::Finish,
                    ApplicationState::Stopping => job.step = Step::AwaitTermination,
                    ApplicationState::CleanupFailed(_) => {
                        return self.cleanup_failed(job, "application_cleanup_failed", send);
                    }
                    ApplicationState::Preparing | ApplicationState::Running => {
                        return self.cleanup_failed(job, "process_termination_failed", send);
                    }
                }
            }
            Step::Finish => {
                if payload["ok"] != true
                    || !responses::session(&payload, record).is_ok_and(|session| {
                        session["state"] == "stopped"
                            && session["leases"]
                                .as_object()
                                .is_some_and(|leases| leases.is_empty())
                    })
                {
                    return self.cleanup_failed(job, "session_finish_failed", send);
                }
                return self.finish(job, send);
            }
            _ => return Err("invalid launch cleanup workflow".into()),
        }
        self.retain(job)
    }
    fn deliver_activation(
        &mut self,
        job: &mut Workflow,
        send: &mut impl FnMut(Command) -> Transport,
    ) -> ModuleResult<()> {
        let request = job.request.ok_or("missing launch request")?;
        let active = matches!(send(Command::CheckRequest { request }), Ok(Response::Ok));
        let delivered = active
            && matches!(
                send(Command::Complete {
                    request,
                    payload: self.records.values[&job.launch_id].response(),
                }),
                Ok(Response::Ok)
            );
        if delivered {
            return Ok(());
        }
        job.begin_cleanup(
            self.records
                .values
                .get_mut(&job.launch_id)
                .expect("workflow record"),
            Outcome::Cancelled,
            Some("completion_unavailable"),
        );
        self.retain(job)
    }

    fn dependency_failed(
        &mut self,
        job: &mut Workflow,
        code: &'static str,
        send: &mut impl FnMut(Command) -> Transport,
    ) -> ModuleResult<()> {
        if job.cleanup {
            if job.step == Step::StopSession {
                let record = self
                    .records
                    .values
                    .get_mut(&job.launch_id)
                    .expect("workflow record");
                record.cleanup_error = Some(code);
                job.step = if record.process_id.is_some() {
                    Step::Terminate
                } else {
                    Step::Finish
                };
                return self.retain(job);
            }
            return self.cleanup_failed(job, code, send);
        }
        if job.step == Step::Monitor {
            // Loss of process visibility must not release its exclusive lease.
            return self.cleanup_failed(job, code, send);
        }
        let cancelled = job.request.is_some_and(|request| {
            !matches!(send(Command::CheckRequest { request }), Ok(Response::Ok))
        });
        job.begin_cleanup(
            self.records
                .values
                .get_mut(&job.launch_id)
                .expect("workflow record"),
            if cancelled {
                Outcome::Cancelled
            } else {
                Outcome::Failed
            },
            Some(if cancelled { "request_cancelled" } else { code }),
        );
        self.retain(job)
    }
    fn cleanup_failed(
        &mut self,
        job: &Workflow,
        code: &'static str,
        send: &mut impl FnMut(Command) -> Transport,
    ) -> ModuleResult<()> {
        let record = self
            .records
            .values
            .get_mut(&job.launch_id)
            .expect("workflow record");
        record.state = State::CleanupFailed;
        record.cleanup_error = Some(code);
        self.reply(job, send)
    }
    fn finish(
        &mut self,
        job: &Workflow,
        send: &mut impl FnMut(Command) -> Transport,
    ) -> ModuleResult<()> {
        let record = self
            .records
            .values
            .get_mut(&job.launch_id)
            .expect("workflow record");
        record.state = record.cleanup_outcome.state();
        record.cleanup_error = None;
        record.executable = None;
        self.reply(job, send)
    }
    fn reply(
        &self,
        job: &Workflow,
        send: &mut impl FnMut(Command) -> Transport,
    ) -> ModuleResult<()> {
        if let Some(request) = job.request {
            complete(
                send,
                request,
                self.records.values[&job.launch_id].response(),
            )
        } else {
            Ok(())
        }
    }
    fn retain(&mut self, job: &mut Workflow) -> ModuleResult<()> {
        self.job = Some(job.clone());
        Ok(())
    }
}
fn complete(
    send: &mut impl FnMut(Command) -> Transport,
    request: u64,
    payload: Value,
) -> ModuleResult<()> {
    match send(Command::Complete { request, payload })? {
        Response::Ok => Ok(()),
        Response::Error { code, .. } if code == "request_ended" || code == "not_found" => Ok(()),
        _ => Err("launch completion failed".into()),
    }
}

fn apply_application_outcome(record: &mut crate::records::Record, outcome: ApplicationOutcome) {
    record.cleanup_outcome = outcome.launch_outcome();
    match outcome {
        ApplicationOutcome::Exited => {}
        ApplicationOutcome::LaunchFailed => record.failure = Some("application_launch_failed"),
        ApplicationOutcome::Cancelled => record.failure = Some("application_cancelled"),
    }
}

#[cfg(test)]
mod tests;
