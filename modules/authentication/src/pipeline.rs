use crate::engine::{Change, Engine};
use crate::product_settings::{CONFIGURATION_SERVICE, TokenLifetimes};
use crate::protocol::{Document, Operation, Settings, error};
use crate::{SERVICE, STATE_SERVICE};
use amaterasu_module_sdk::{Command, ModuleResult, Response};
use serde_json::{Value, json};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const STATE_KEY: &str = "authentication/v1";

type Transport = ModuleResult<Response>;

struct Request {
    id: u64,
    caller: String,
    deadline: Instant,
}
struct Pending {
    request: Request,
    child_request: u64,
    stage: Stage,
    phase: PendingPhase,
}
enum PendingPhase {
    Waiting,
    Releasing(ChildOutcome),
}
enum ChildOutcome {
    Payload(Value),
    Failure { code: String },
}
enum Stage {
    Loading(Operation),
    Saving {
        response: Value,
        invalidate_sessions: bool,
    },
    LoadingProductSettings(Operation),
    SavingProductSettings {
        lifetimes: TokenLifetimes,
        expected_revision: u64,
    },
}

pub struct Pipeline {
    engine: Engine,
    bootstrap_lifetimes: TokenLifetimes,
    pending: Option<Pending>,
}

impl Pipeline {
    pub fn new(settings: Settings) -> Self {
        let bootstrap_lifetimes = TokenLifetimes::from_bootstrap(&settings);
        Self {
            engine: Engine::new(settings),
            bootstrap_lifetimes,
            pending: None,
        }
    }

    pub fn tick(&mut self, send: impl FnMut(Command) -> Transport) -> ModuleResult<()> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| "authentication clock failure")?
            .as_secs();
        self.tick_at(now, send)
    }

    pub fn tick_at(
        &mut self,
        now: u64,
        mut send: impl FnMut(Command) -> Transport,
    ) -> ModuleResult<()> {
        if let Some(pending) = self.pending.take() {
            return self.advance(pending, now, &mut send);
        }
        // One request at a time: state CAS operations cannot race internally and
        // plaintext passwords are never accumulated in another application queue.
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
                if service != SERVICE {
                    return complete(&mut send, id, error("invalid_service"));
                }
                if serde_json::to_vec(&payload).map_or(true, |bytes| bytes.len() > 4096) {
                    return complete(&mut send, id, error("invalid_request"));
                }
                let Ok(operation) = serde_json::from_value::<Operation>(payload) else {
                    return complete(&mut send, id, error("invalid_request"));
                };
                if let Err(code) = self.engine.admit(&caller, &operation, now) {
                    return complete(&mut send, id, error(code));
                }
                let request = Request {
                    id,
                    caller,
                    deadline: Instant::now() + Duration::from_millis(remaining_ms.min(30_000)),
                };
                match operation {
                    Operation::SettingsGet {}
                    | Operation::Login { .. }
                    | Operation::Refresh { .. } => self.submit(
                        request,
                        Stage::LoadingProductSettings(operation),
                        CONFIGURATION_SERVICE,
                        "product-get",
                        json!({"action":"product_get"}),
                        &mut send,
                    ),
                    Operation::SettingsUpdate {
                        session_token_ttl_seconds,
                        remember_me_refresh_token_ttl_seconds,
                        expected_revision,
                    } => {
                        let lifetimes = TokenLifetimes::new(
                            session_token_ttl_seconds,
                            remember_me_refresh_token_ttl_seconds,
                        )
                        .expect("operation validation checked token lifetimes");
                        self.submit(
                            request,
                            Stage::SavingProductSettings {
                                lifetimes,
                                expected_revision,
                            },
                            CONFIGURATION_SERVICE,
                            "product-update",
                            lifetimes.update_payload(expected_revision),
                            &mut send,
                        )
                    }
                    operation => self.submit(
                        request,
                        Stage::Loading(operation),
                        STATE_SERVICE,
                        "load",
                        json!({"action":"get","key":STATE_KEY}),
                        &mut send,
                    ),
                }
            }
            _ => Err("authentication poll failure".into()),
        }
    }

    fn submit(
        &mut self,
        request: Request,
        stage: Stage,
        service: &str,
        operation_id_suffix: &str,
        payload: Value,
        send: &mut impl FnMut(Command) -> Transport,
    ) -> ModuleResult<()> {
        let timeout_ms = request
            .deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as u64;
        if timeout_ms == 0 {
            return complete(send, request.id, error("deadline_exceeded"));
        }
        match send(Command::Request {
            service: service.into(),
            payload,
            timeout_ms: timeout_ms.min(30_000),
            operation_id: format!("authentication.{}.{}", request.id, operation_id_suffix),
            parent: Some(request.id),
        })? {
            Response::Accepted { id } => {
                self.pending = Some(Pending {
                    request,
                    child_request: id,
                    stage,
                    phase: PendingPhase::Waiting,
                });
                Ok(())
            }
            Response::Error { code, .. } => {
                complete(send, request.id, failure_for_stage(&stage, &code))
            }
            _ => complete(
                send,
                request.id,
                failure_for_stage(&stage, "invalid_response"),
            ),
        }
    }

    fn advance(
        &mut self,
        mut pending: Pending,
        now: u64,
        send: &mut impl FnMut(Command) -> Transport,
    ) -> ModuleResult<()> {
        let outcome = match std::mem::replace(&mut pending.phase, PendingPhase::Waiting) {
            PendingPhase::Releasing(outcome) => outcome,
            PendingPhase::Waiting => match send(Command::TakeResult {
                request: pending.child_request,
            }) {
                Ok(Response::Pending) => {
                    self.pending = Some(pending);
                    return Ok(());
                }
                Ok(Response::Result { payload }) => ChildOutcome::Payload(payload),
                Ok(Response::Error { code, .. }) if code == "not_found" => {
                    return complete(
                        send,
                        pending.request.id,
                        failure_for_stage(&pending.stage, &code),
                    );
                }
                Ok(Response::Error { code, .. }) => ChildOutcome::Failure { code },
                Ok(_) => {
                    return complete(
                        send,
                        pending.request.id,
                        failure_for_stage(&pending.stage, "invalid_response"),
                    );
                }
                Err(failure) => {
                    self.pending = Some(pending);
                    return Err(failure);
                }
            },
        };

        match send(Command::ReleaseResult {
            request: pending.child_request,
        }) {
            Ok(Response::Ok) => self.consume_child_result(pending, outcome, now, send),
            Ok(Response::Error { code, .. }) if code == "not_found" => {
                self.consume_child_result(pending, outcome, now, send)
            }
            Ok(_) => {
                pending.phase = PendingPhase::Releasing(outcome);
                self.pending = Some(pending);
                Ok(())
            }
            Err(failure) => {
                pending.phase = PendingPhase::Releasing(outcome);
                self.pending = Some(pending);
                Err(failure)
            }
        }
    }

    fn consume_child_result(
        &mut self,
        pending: Pending,
        outcome: ChildOutcome,
        now: u64,
        send: &mut impl FnMut(Command) -> Transport,
    ) -> ModuleResult<()> {
        let payload = match outcome {
            ChildOutcome::Payload(payload) => payload,
            ChildOutcome::Failure { code } => {
                return complete(
                    send,
                    pending.request.id,
                    failure_for_stage(&pending.stage, &code),
                );
            }
        };

        match pending.stage {
            Stage::Saving {
                response,
                invalidate_sessions,
            } => {
                if payload["ok"] != true || payload["revision"].as_u64().is_none() {
                    return complete(send, pending.request.id, error("state_unavailable"));
                }
                if invalidate_sessions {
                    self.engine.clear_sessions();
                }
                complete(send, pending.request.id, response)
            }
            Stage::Loading(operation) => {
                if Instant::now() >= pending.request.deadline {
                    return complete(send, pending.request.id, error("deadline_exceeded"));
                }
                let document = match read_document(&payload) {
                    Ok(document) => document,
                    Err(code) => return complete(send, pending.request.id, error(code)),
                };
                match self.engine.apply(&pending.request.caller, operation, document, now) {
                    Ok(Change::Read(response)) => complete(send, pending.request.id, response),
                    Ok(Change::Write {
                        document,
                        response,
                        invalidate_sessions,
                    }) => self.submit(
                        pending.request,
                        Stage::Saving {
                            response,
                            invalidate_sessions,
                        },
                        STATE_SERVICE,
                        "save",
                        json!({"action":"set","key":STATE_KEY,"expected_revision":payload["revision"],"value":document}),
                        send,
                    ),
                    Err(code) => complete(send, pending.request.id, error(code)),
                }
            }
            Stage::LoadingProductSettings(operation) => {
                if let Some(failure) = normalized_product_error(&payload) {
                    return complete(send, pending.request.id, failure);
                }
                let (lifetimes, revision) =
                    match TokenLifetimes::from_product_response(&payload, self.bootstrap_lifetimes)
                    {
                        Ok(document) => document,
                        Err(_) => {
                            return complete(
                                send,
                                pending.request.id,
                                error("invalid_state_response"),
                            );
                        }
                    };
                self.engine.set_token_lifetimes(lifetimes);
                match operation {
                    Operation::SettingsGet {} => complete(
                        send,
                        pending.request.id,
                        lifetimes.settings_response(revision),
                    ),
                    operation @ (Operation::Login { .. } | Operation::Refresh { .. }) => self
                        .submit(
                            pending.request,
                            Stage::Loading(operation),
                            STATE_SERVICE,
                            "load",
                            json!({"action":"get","key":STATE_KEY}),
                            send,
                        ),
                    _ => complete(send, pending.request.id, error("invalid_request")),
                }
            }
            Stage::SavingProductSettings {
                lifetimes,
                expected_revision,
            } => {
                if let Some(failure) = normalized_product_error(&payload) {
                    return complete(send, pending.request.id, failure);
                }
                let revision = match TokenLifetimes::from_update_response(
                    &payload,
                    expected_revision,
                    lifetimes,
                ) {
                    Ok(revision) => revision,
                    Err(_) => {
                        return complete(send, pending.request.id, error("invalid_state_response"));
                    }
                };
                self.engine.set_token_lifetimes(lifetimes);
                complete(
                    send,
                    pending.request.id,
                    lifetimes.settings_response(revision),
                )
            }
        }
    }
}

fn normalized_product_error(payload: &Value) -> Option<Value> {
    if payload["ok"] != false {
        return None;
    }
    let outer = payload.as_object()?;
    let failure = payload["error"].as_object()?;
    if outer.len() != 2
        || outer
            .keys()
            .any(|key| !matches!(key.as_str(), "ok" | "error"))
        || failure
            .keys()
            .any(|key| !matches!(key.as_str(), "code" | "message" | "current_revision"))
    {
        return Some(error("configuration_unavailable"));
    }
    let Some(code) = failure.get("code").and_then(Value::as_str) else {
        return Some(error("configuration_unavailable"));
    };
    if code.is_empty()
        || code.len() > 64
        || !code
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
    {
        return Some(error("configuration_unavailable"));
    }
    if failure
        .get("current_revision")
        .is_some_and(|revision| revision.as_u64().is_none())
    {
        return Some(error("configuration_unavailable"));
    }
    let current_revision = failure
        .get("current_revision")
        .and_then(Value::as_u64)
        .filter(|_| code == "revision_conflict");
    Some(match current_revision {
        Some(current_revision) => {
            json!({"ok":false,"error":{"code":code,"current_revision":current_revision}})
        }
        None => json!({"ok":false,"error":{"code":code}}),
    })
}

fn failure_for_stage(stage: &Stage, code: &str) -> Value {
    match stage {
        Stage::LoadingProductSettings(_) | Stage::SavingProductSettings { .. } => error(code),
        Stage::Loading(_) | Stage::Saving { .. } => error("state_unavailable"),
    }
}

fn read_document(payload: &Value) -> Result<Document, &'static str> {
    match payload["found"].as_bool() {
        Some(false) => Ok(Document::default()),
        Some(true) => {
            let document: Document =
                serde_json::from_value(payload["value"].clone()).map_err(|_| "invalid_state")?;
            document.validate()?;
            Ok(document)
        }
        None => Err("invalid_state"),
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
        _ => Err("authentication completion failure".into()),
    }
}

#[cfg(test)]
mod tests;
