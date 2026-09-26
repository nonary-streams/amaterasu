use crate::path::{effective_as_value, effective_settings};
use crate::protocol::{Operation, error, unavailable};
use crate::settings::Document;
use amaterasu_module_sdk::{Command, ModuleResult, Response};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::time::{Duration, Instant};

const STATE_KEY: &str = "global/amaterasu.host-configuration";
const MAX_DOCUMENT_BYTES: usize = 12_000;
const MAX_RESPONSE_BYTES: usize = 16_384;
const MAX_REQUEST_MS: u64 = 30_000;

type TransportResult = ModuleResult<Response>;

struct Incoming {
    request_id: u64,
    operation: Operation,
    deadline: Instant,
}

struct Pending {
    request_id: u64,
    deadline: Instant,
    state_request_id: u64,
    stage: Stage,
    phase: PendingPhase,
}

enum Stage {
    Loading(Operation),
    Saving {
        expected_state_revision: u64,
        response: Value,
    },
}

enum PendingPhase {
    Waiting,
    Releasing(ChildOutcome),
}

enum ChildOutcome {
    Payload(Value),
    Failure { code: String, message: String },
}

pub struct Pipeline {
    trusted_root: Result<PathBuf, String>,
    pending: Option<Pending>,
}

impl Pipeline {
    pub fn new(trusted_root: Result<PathBuf, String>) -> Self {
        Self {
            trusted_root,
            pending: None,
        }
    }

    pub fn tick(&mut self, mut send: impl FnMut(Command) -> TransportResult) -> ModuleResult<()> {
        if let Some(pending) = self.pending.take() {
            self.advance(pending, &mut send)?;
        }
        if self.pending.is_none() {
            self.receive(&mut send)?;
        }
        Ok(())
    }

    fn receive(&mut self, send: &mut impl FnMut(Command) -> TransportResult) -> ModuleResult<()> {
        match send(Command::Poll {})? {
            Response::Pending => Ok(()),
            Response::Request {
                id,
                caller,
                service,
                payload,
                remaining_ms,
                ..
            } => {
                if service != crate::SERVICE {
                    return complete(
                        send,
                        id,
                        error("invalid_service", "unsupported host configuration service"),
                    );
                }
                let operation = match serde_json::from_value::<Operation>(payload) {
                    Ok(operation) => operation,
                    Err(failure) => {
                        return complete(send, id, error("invalid_request", failure.to_string()));
                    }
                };
                if matches!(operation, Operation::Update { .. }) && caller != "amaterasu.management"
                {
                    return complete(
                        send,
                        id,
                        error(
                            "permission_denied",
                            "only amaterasu.management may update host configuration",
                        ),
                    );
                }
                if let Some(settings) = operation.settings()
                    && let Err(message) = settings.validate()
                {
                    return complete(send, id, error("invalid_settings", message));
                }
                let deadline =
                    Instant::now() + Duration::from_millis(remaining_ms.min(MAX_REQUEST_MS));
                if let Err(message) = &self.trusted_root {
                    return complete(send, id, unavailable(message.clone()));
                }
                self.load(
                    Incoming {
                        request_id: id,
                        operation,
                        deadline,
                    },
                    send,
                )
            }
            response => Err(format!("host configuration poll failed: {response:?}")),
        }
    }

    fn load(
        &mut self,
        incoming: Incoming,
        send: &mut impl FnMut(Command) -> TransportResult,
    ) -> ModuleResult<()> {
        let Some(state_request_id) = submit_state(
            send,
            incoming.request_id,
            incoming.deadline,
            "load",
            json!({"action":"get","key":STATE_KEY}),
        )?
        else {
            return complete(
                send,
                incoming.request_id,
                unavailable("state service did not accept the authoritative read"),
            );
        };
        self.pending = Some(Pending {
            request_id: incoming.request_id,
            deadline: incoming.deadline,
            state_request_id,
            stage: Stage::Loading(incoming.operation),
            phase: PendingPhase::Waiting,
        });
        Ok(())
    }

    fn advance(
        &mut self,
        mut pending: Pending,
        send: &mut impl FnMut(Command) -> TransportResult,
    ) -> ModuleResult<()> {
        if Instant::now() >= pending.deadline {
            let _ = send(Command::Cancel {
                request: pending.state_request_id,
            });
            let _ = send(Command::ReleaseResult {
                request: pending.state_request_id,
            });
            return complete(
                send,
                pending.request_id,
                error("deadline_exceeded", "host configuration request expired"),
            );
        }
        if matches!(&pending.phase, PendingPhase::Releasing(_)) {
            let outcome = match std::mem::replace(&mut pending.phase, PendingPhase::Waiting) {
                PendingPhase::Releasing(outcome) => outcome,
                PendingPhase::Waiting => unreachable!(),
            };
            return self.release_and_consume(pending, outcome, send);
        }

        match send(Command::TakeResult {
            request: pending.state_request_id,
        })? {
            Response::Pending => {
                self.pending = Some(pending);
                Ok(())
            }
            Response::Result { payload } => {
                self.release_and_consume(pending, ChildOutcome::Payload(payload), send)
            }
            Response::Error { code, message }
                if code == "not_found" || code == "permission_denied" =>
            {
                self.finish_child_failure(pending.request_id, code, message, send)
            }
            Response::Error { code, message } => {
                self.release_and_consume(pending, ChildOutcome::Failure { code, message }, send)
            }
            response => self.finish_child_failure(
                pending.request_id,
                "invalid_state_response".into(),
                format!("unexpected state response: {response:?}"),
                send,
            ),
        }
    }

    fn release_and_consume(
        &mut self,
        mut pending: Pending,
        outcome: ChildOutcome,
        send: &mut impl FnMut(Command) -> TransportResult,
    ) -> ModuleResult<()> {
        match send(Command::ReleaseResult {
            request: pending.state_request_id,
        }) {
            Ok(Response::Ok) => self.consume(pending, outcome, send),
            Ok(Response::Error { code, .. }) if code == "not_found" => {
                self.consume(pending, outcome, send)
            }
            Ok(Response::Error { code, message }) => {
                pending.phase = PendingPhase::Releasing(outcome);
                self.pending = Some(pending);
                let _ = (code, message);
                Ok(())
            }
            Ok(response) => {
                pending.phase = PendingPhase::Releasing(outcome);
                self.pending = Some(pending);
                let _ = response;
                Ok(())
            }
            Err(failure) => {
                pending.phase = PendingPhase::Releasing(outcome);
                self.pending = Some(pending);
                Err(failure)
            }
        }
    }

    fn consume(
        &mut self,
        pending: Pending,
        outcome: ChildOutcome,
        send: &mut impl FnMut(Command) -> TransportResult,
    ) -> ModuleResult<()> {
        match outcome {
            ChildOutcome::Failure { code, message } => {
                self.finish_child_failure(pending.request_id, code, message, send)
            }
            ChildOutcome::Payload(payload) => {
                if payload.get("ok").and_then(Value::as_bool) != Some(true) {
                    let message = payload
                        .pointer("/error/message")
                        .and_then(Value::as_str)
                        .unwrap_or("state returned an unsuccessful response");
                    return complete(send, pending.request_id, unavailable(message));
                }
                let state_revision = match payload.get("revision").and_then(Value::as_u64) {
                    Some(revision) => revision,
                    None => {
                        return complete(
                            send,
                            pending.request_id,
                            unavailable("state response omitted its revision"),
                        );
                    }
                };
                match pending.stage {
                    Stage::Loading(operation) => {
                        let document = match read_document(&payload) {
                            Ok(document) => document,
                            Err(response) => return complete(send, pending.request_id, response),
                        };
                        self.apply_loaded(
                            pending.request_id,
                            pending.deadline,
                            operation,
                            document,
                            state_revision,
                            send,
                        )
                    }
                    Stage::Saving {
                        expected_state_revision,
                        response,
                    } => {
                        let Some(written_revision) =
                            state_revision.checked_sub(expected_state_revision)
                        else {
                            return complete(
                                send,
                                pending.request_id,
                                unavailable("state revision moved backwards after update"),
                            );
                        };
                        if written_revision != 1 {
                            return complete(
                                send,
                                pending.request_id,
                                unavailable("state did not confirm the expected durable update"),
                            );
                        }
                        complete(send, pending.request_id, response)
                    }
                }
            }
        }
    }

    fn apply_loaded(
        &mut self,
        request_id: u64,
        deadline: Instant,
        operation: Operation,
        document: Document,
        state_revision: u64,
        send: &mut impl FnMut(Command) -> TransportResult,
    ) -> ModuleResult<()> {
        let update_settings = operation.settings();
        let root = match &self.trusted_root {
            Ok(root) => root,
            Err(message) => {
                let response = if matches!(operation, Operation::Readiness {}) {
                    unavailable(message.clone())
                } else {
                    error("configuration_unavailable", message.clone())
                };
                return complete(send, request_id, response);
            }
        };
        match operation {
            Operation::Readiness {} => match build_read_response(&document, root) {
                Ok(_) => complete(
                    send,
                    request_id,
                    json!({
                        "ok":true,"ready":true,"version":document.version,
                        "revision":document.revision
                    }),
                ),
                Err(_) => complete(
                    send,
                    request_id,
                    unavailable("effective configuration cannot be returned safely"),
                ),
            },
            Operation::Get {} => match build_read_response(&document, root) {
                Ok(response) => complete(send, request_id, response),
                Err(response) => complete(send, request_id, response),
            },
            Operation::Update {
                expected_revision, ..
            } => {
                if document.revision != expected_revision {
                    return complete(send, request_id, revision_conflict(document.revision));
                }
                let revision = match document.revision.checked_add(1) {
                    Some(revision) => revision,
                    None => {
                        return complete(
                            send,
                            request_id,
                            error("revision_exhausted", "configuration revision is exhausted"),
                        );
                    }
                };
                let Some(settings) = update_settings else {
                    unreachable!("update contains settings")
                };
                let next = Document::from_settings(revision, settings);
                if let Err(message) = next.validate() {
                    return complete(send, request_id, error("invalid_settings", message));
                }
                if let Err(message) = effective_settings(&next, root) {
                    return complete(send, request_id, error("invalid_settings", message));
                }
                if !document_fits(&next) {
                    return complete(
                        send,
                        request_id,
                        error("resource_exhausted", "configuration document is too large"),
                    );
                }
                let response = match build_read_response(&next, root) {
                    Ok(response) => response,
                    Err(response) => return complete(send, request_id, response),
                };
                self.save(request_id, deadline, state_revision, next, response, send)
            }
        }
    }

    fn save(
        &mut self,
        request_id: u64,
        deadline: Instant,
        state_revision: u64,
        document: Document,
        response: Value,
        send: &mut impl FnMut(Command) -> TransportResult,
    ) -> ModuleResult<()> {
        let Some(state_request_id) = submit_state(
            send,
            request_id,
            deadline,
            "save",
            json!({
                "action":"set","key":STATE_KEY,
                "expected_revision":state_revision,"value":document
            }),
        )?
        else {
            return complete(
                send,
                request_id,
                unavailable("state service did not accept the configuration write"),
            );
        };
        self.pending = Some(Pending {
            request_id,
            deadline,
            state_request_id,
            stage: Stage::Saving {
                expected_state_revision: state_revision,
                response,
            },
            phase: PendingPhase::Waiting,
        });
        Ok(())
    }

    fn finish_child_failure(
        &mut self,
        request_id: u64,
        code: String,
        message: String,
        send: &mut impl FnMut(Command) -> TransportResult,
    ) -> ModuleResult<()> {
        complete(
            send,
            request_id,
            unavailable(format!("state service failure ({code}): {message}")),
        )
    }
}

fn submit_state(
    send: &mut impl FnMut(Command) -> TransportResult,
    parent: u64,
    deadline: Instant,
    stage: &str,
    payload: Value,
) -> ModuleResult<Option<u64>> {
    let remaining_ms = deadline
        .saturating_duration_since(Instant::now())
        .as_millis() as u64;
    if remaining_ms == 0 {
        return Ok(None);
    }
    match send(Command::Request {
        service: crate::STATE_SERVICE.into(),
        payload,
        timeout_ms: remaining_ms.min(MAX_REQUEST_MS),
        operation_id: format!("host-configuration.{parent}.{stage}"),
        parent: Some(parent),
    })? {
        Response::Accepted { id } => Ok(Some(id)),
        _ => Ok(None),
    }
}

fn complete(
    send: &mut impl FnMut(Command) -> TransportResult,
    request: u64,
    payload: Value,
) -> ModuleResult<()> {
    match send(Command::Complete { request, payload })? {
        Response::Ok => Ok(()),
        Response::Error { code, .. } if code == "request_ended" || code == "not_found" => Ok(()),
        response => Err(format!(
            "host configuration completion failed: {response:?}"
        )),
    }
}

fn read_document(payload: &Value) -> Result<Document, Value> {
    match payload.get("found").and_then(Value::as_bool) {
        Some(false) if payload.get("value").is_some_and(Value::is_null) => Ok(Document::default()),
        Some(false) => Err(unavailable(
            "state lookup returned a value for a missing document",
        )),
        Some(true) => {
            let document: Document =
                serde_json::from_value(payload.get("value").cloned().unwrap_or(Value::Null))
                    .map_err(|_| unavailable("persisted host configuration is malformed"))?;
            document.validate().map_err(|message| {
                unavailable(format!("persisted configuration is invalid: {message}"))
            })?;
            if !document_fits(&document) {
                return Err(unavailable(
                    "persisted configuration exceeds its size limit",
                ));
            }
            Ok(document)
        }
        None => Err(unavailable("state lookup omitted its found marker")),
    }
}

fn build_read_response(document: &Document, root: &std::path::Path) -> Result<Value, Value> {
    let effective = effective_settings(document, root).map_err(|message| {
        unavailable(format!("effective configuration is unavailable: {message}"))
    })?;
    let response = json!({
        "ok":true,"ready":true,"version":document.version,
        "revision":document.revision,"values":document.settings(),
        "effective":effective_as_value(&effective)
    });
    if serde_json::to_vec(&response).is_ok_and(|bytes| bytes.len() <= MAX_RESPONSE_BYTES) {
        Ok(response)
    } else {
        Err(error(
            "resource_exhausted",
            "effective configuration exceeds the runtime response limit",
        ))
    }
}

fn revision_conflict(current_revision: u64) -> Value {
    json!({
        "ok":false,
        "error":{
            "code":"revision_conflict",
            "message":"expected revision does not match current configuration",
            "current_revision":current_revision
        }
    })
}

fn document_fits(document: &Document) -> bool {
    serde_json::to_vec(document).is_ok_and(|bytes| bytes.len() <= MAX_DOCUMENT_BYTES)
}

#[cfg(test)]
mod tests;
