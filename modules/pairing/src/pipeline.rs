use crate::engine::{Change, Engine};
use crate::identity::{Shared, Snapshot};
use crate::protocol::{Document, Operation, Settings, error};
use crate::{SERVICE, STATE_SERVICE};
use amaterasu_module_sdk::{Command, ModuleResult, Response};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const STATE_KEY: &str = "pairing/v1";

type Transport = ModuleResult<Response>;

struct Request {
    id: u64,
    caller: String,
    deadline: Instant,
}
struct Pending {
    request: Request,
    state_request: u64,
    stage: Stage,
}
enum Stage {
    Loading(Operation),
    Saving {
        response: Value,
        snapshot: Option<Arc<Snapshot>>,
    },
}

pub struct Pipeline {
    engine: Engine,
    pending: Option<Pending>,
    identity: Arc<Shared>,
}

impl Pipeline {
    #[cfg(test)]
    pub fn new(settings: Settings) -> Self {
        Self::with_identity(settings, Arc::new(Shared::default()))
    }

    pub fn with_identity(settings: Settings, identity: Arc<Shared>) -> Self {
        Self {
            engine: Engine::new(settings),
            pending: None,
            identity,
        }
    }

    pub fn invalidate_identity(&self) {
        self.identity.invalidate();
    }

    pub fn tick(&mut self, send: impl FnMut(Command) -> Transport) -> ModuleResult<()> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| "pairing clock failure")?
            .as_secs();
        self.tick_at(now, send)
    }

    pub fn tick_at(
        &mut self,
        now: u64,
        send: impl FnMut(Command) -> Transport,
    ) -> ModuleResult<()> {
        let result = self.drive(now, send);
        if result.is_err() {
            self.identity.invalidate();
        }
        result
    }

    fn drive(&mut self, now: u64, mut send: impl FnMut(Command) -> Transport) -> ModuleResult<()> {
        if let Some(pending) = self.pending.take() {
            return self.advance(pending, now, &mut send);
        }
        // One request at a time: state CAS operations cannot race internally and
        // identity writes and pairing commits are serialized without stale caches.
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
                if serde_json::to_vec(&payload).map_or(true, |bytes| bytes.len() > 12_000) {
                    return complete(&mut send, id, error("invalid_request"));
                }
                let Ok(operation) = serde_json::from_value::<Operation>(payload) else {
                    return complete(&mut send, id, error("invalid_request"));
                };
                if let Err(code) = self.engine.admit(&caller, &operation) {
                    return complete(&mut send, id, error(code));
                }
                if operation.may_write() {
                    self.identity.invalidate();
                }
                let request = Request {
                    id,
                    caller,
                    deadline: Instant::now() + Duration::from_millis(remaining_ms.min(30_000)),
                };
                self.submit(
                    request,
                    Stage::Loading(operation),
                    json!({"action":"get","key":STATE_KEY}),
                    &mut send,
                )
            }
            _ => Err("pairing poll failure".into()),
        }
    }

    fn submit(
        &mut self,
        request: Request,
        stage: Stage,
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
        let stage_name = match &stage {
            Stage::Loading(_) => "load",
            Stage::Saving { .. } => "save",
        };
        match send(Command::Request {
            service: STATE_SERVICE.into(),
            payload,
            timeout_ms: timeout_ms.min(30_000),
            operation_id: format!("pairing.{}.{}", request.id, stage_name),
            parent: Some(request.id),
        })? {
            Response::Accepted { id } => {
                self.pending = Some(Pending {
                    request,
                    state_request: id,
                    stage,
                });
                Ok(())
            }
            _ => {
                self.identity.invalidate();
                complete(send, request.id, error("state_unavailable"))
            }
        }
    }

    fn advance(
        &mut self,
        pending: Pending,
        now: u64,
        send: &mut impl FnMut(Command) -> Transport,
    ) -> ModuleResult<()> {
        match send(Command::TakeResult {
            request: pending.state_request,
        })? {
            Response::Pending => {
                self.pending = Some(pending);
                Ok(())
            }
            Response::Result { payload }
                if payload["ok"] == true && payload["revision"].as_u64().is_some() =>
            {
                match pending.stage {
                    Stage::Saving { response, snapshot } => {
                        self.identity
                            .publish(payload["revision"].as_u64().unwrap(), snapshot)?;
                        complete(send, pending.request.id, response)
                    }
                    Stage::Loading(operation) => {
                        if Instant::now() >= pending.request.deadline {
                            return complete(send, pending.request.id, error("deadline_exceeded"));
                        }
                        if !matches!(
                            send(Command::CheckRequest {
                                request: pending.request.id
                            })?,
                            Response::Ok
                        ) {
                            return complete(send, pending.request.id, error("request_ended"));
                        }
                        let document = match read_document(&payload, now) {
                            Ok(document) => document,
                            Err(code) => {
                                self.identity.invalidate();
                                return complete(send, pending.request.id, error(code));
                            }
                        };
                        self.apply_loaded(
                            pending.request,
                            operation,
                            document,
                            payload["revision"].as_u64().unwrap(),
                            now,
                            send,
                        )
                    }
                }
            }
            Response::Result { payload }
                if matches!(&pending.stage, Stage::Saving { .. })
                    && payload["ok"] == false
                    && payload["error"]["code"] == "revision_conflict"
                    && payload["error"]["current_revision"].as_u64().is_some() =>
            {
                self.identity.invalidate();
                complete(
                    send,
                    pending.request.id,
                    json!({
                        "ok": false,
                        "code": "revision_conflict",
                        "current_revision": payload["error"]["current_revision"]
                    }),
                )
            }
            // Never echo transport/state messages: they can contain input values.
            _ => {
                self.identity.invalidate();
                complete(send, pending.request.id, error("state_unavailable"))
            }
        }
    }
    fn apply_loaded(
        &mut self,
        request: Request,
        operation: Operation,
        document: Document,
        revision: u64,
        now: u64,
        send: &mut impl FnMut(Command) -> Transport,
    ) -> ModuleResult<()> {
        let loaded = Snapshot::prepare(&document, now).map_err(str::to_owned)?;
        if let Operation::UpdateClientConfigOverrides {
            expected_revision, ..
        } = &operation
            && *expected_revision != revision
        {
            self.identity.publish(revision, loaded)?;
            return complete(
                send,
                request.id,
                json!({"ok":false,"code":"revision_conflict","current_revision":revision}),
            );
        }
        let include_revision = matches!(&operation, Operation::ListClients {});
        match self.engine.apply(&request.caller, operation, document, now) {
            Ok(Change::Read(mut response)) => {
                if include_revision {
                    response["revision"] = revision.into();
                }
                self.identity.publish(revision, loaded)?;
                complete(send, request.id, response)
            }
            Ok(Change::Write { document, response }) => {
                self.identity.invalidate();
                let snapshot = Snapshot::prepare(&document, now).map_err(str::to_owned)?;
                self.submit(request,Stage::Saving {response,snapshot},json!({"action":"set","key":STATE_KEY,"expected_revision":revision,"value":document}),send)
            }
            Err(code) => {
                self.identity.publish(revision, loaded)?;
                complete(send, request.id, error(code))
            }
        }
    }
}

fn read_document(payload: &Value, now: u64) -> Result<Document, &'static str> {
    match payload["found"].as_bool() {
        Some(false) => Ok(Document::default()),
        Some(true) => {
            let document: Document =
                serde_json::from_value(payload["value"].clone()).map_err(|_| "invalid_state")?;
            document.validate(now)?;
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
        _ => Err("pairing completion failure".into()),
    }
}

#[cfg(test)]
mod tests;
