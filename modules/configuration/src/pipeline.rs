use crate::STATE_SERVICE;
use crate::protocol::{
    Change, Document, Operation, ProductChange, ProductDocument, apply, apply_product, error,
    product_success, success,
};
use amaterasu_configuration_schema::{defaults, validate_values};
use amaterasu_module_sdk::{Command, ModuleResult, Response};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::time::{Duration, Instant};

const MAX_QUEUED: usize = 32;
// Reserve space for the state key, operation wrapper, and response metadata.
const MAX_DOCUMENT_BYTES: usize = 12_000;

type TransportResult = ModuleResult<Response>;

struct Incoming {
    request_id: u64,
    key: String,
    operation: Operation,
    deadline: Instant,
}

struct RequestContext {
    request_id: u64,
    key: String,
    deadline: Instant,
}

struct Pending {
    request_id: u64,
    key: String,
    deadline: Instant,
    state_request_id: u64,
    stage: Stage,
    phase: PendingPhase,
}

enum PendingPhase {
    Waiting,
    Releasing(ChildOutcome),
}

enum ChildOutcome {
    Payload(Value),
    Failure { code: String, message: String },
}

enum Stage {
    Loading(Operation),
    Saving(Document),
    SavingProduct(ProductDocument),
}

#[derive(Default)]
pub struct Pipeline {
    queued: VecDeque<Incoming>,
    pending: Option<Pending>,
}

impl Pipeline {
    pub fn tick(&mut self, mut send: impl FnMut(Command) -> TransportResult) -> ModuleResult<()> {
        if let Some(pending) = self.pending.take() {
            self.advance(pending, &mut send)?;
        }
        // Stop polling at capacity so the runtime's own bounded queue applies backpressure.
        if self.queued.len() < MAX_QUEUED {
            self.receive(&mut send)?;
        }
        if self.pending.is_none()
            && let Some(incoming) = self.queued.pop_front()
        {
            self.load(incoming, &mut send)?;
        }
        Ok(())
    }

    fn receive(&mut self, send: &mut impl FnMut(Command) -> TransportResult) -> ModuleResult<()> {
        match send(Command::Poll {})? {
            Response::Pending => Ok(()),
            Response::Request {
                id,
                caller,
                payload,
                remaining_ms,
                ..
            } => {
                let operation = match serde_json::from_value::<Operation>(payload) {
                    Ok(operation) => operation,
                    Err(failure) => {
                        return complete(send, id, error("invalid_request", failure.to_string()));
                    }
                };
                let key = match &operation {
                    Operation::ProductGet {} | Operation::ProductUpdate { .. } => {
                        format!("product/{caller}")
                    }
                    _ => format!("principal/{caller}"),
                };
                self.queued.push_back(Incoming {
                    request_id: id,
                    // This is an opaque state key, never a filesystem path. Identity comes from Poll.
                    key,
                    operation,
                    deadline: Instant::now() + Duration::from_millis(remaining_ms.min(30_000)),
                });
                Ok(())
            }
            unexpected => Err(format!("configuration poll failed: {unexpected:?}")),
        }
    }

    fn load(
        &mut self,
        incoming: Incoming,
        send: &mut impl FnMut(Command) -> TransportResult,
    ) -> ModuleResult<()> {
        let payload = json!({"action":"get","key":incoming.key});
        match submit(
            send,
            incoming.request_id,
            incoming.deadline,
            "load",
            payload,
        )? {
            Response::Accepted { id } => {
                self.pending = Some(Pending {
                    request_id: incoming.request_id,
                    key: incoming.key,
                    deadline: incoming.deadline,
                    state_request_id: id,
                    stage: Stage::Loading(incoming.operation),
                    phase: PendingPhase::Waiting,
                });
                Ok(())
            }
            failure => complete(send, incoming.request_id, runtime_failure(failure)),
        }
    }

    fn advance(
        &mut self,
        mut pending: Pending,
        send: &mut impl FnMut(Command) -> TransportResult,
    ) -> ModuleResult<()> {
        if matches!(&pending.phase, PendingPhase::Releasing(_)) {
            return self.release_and_finish(pending, send);
        }

        let result = match send(Command::TakeResult {
            request: pending.state_request_id,
        }) {
            Ok(result) => result,
            Err(failure) => {
                self.pending = Some(pending);
                return Err(failure);
            }
        };
        match result {
            Response::Pending => {
                self.pending = Some(pending);
                Ok(())
            }
            Response::Result { payload } => {
                pending.phase = PendingPhase::Releasing(ChildOutcome::Payload(payload));
                self.release_and_finish(pending, send)
            }
            Response::Error { code, message }
                if code != "not_found" && code != "permission_denied" =>
            {
                pending.phase = PendingPhase::Releasing(ChildOutcome::Failure { code, message });
                self.release_and_finish(pending, send)
            }
            failure => complete(send, pending.request_id, runtime_failure(failure)),
        }
    }

    fn release_and_finish(
        &mut self,
        mut pending: Pending,
        send: &mut impl FnMut(Command) -> TransportResult,
    ) -> ModuleResult<()> {
        let PendingPhase::Releasing(outcome) =
            std::mem::replace(&mut pending.phase, PendingPhase::Waiting)
        else {
            unreachable!("release requires a preserved child result")
        };

        let released = match send(Command::ReleaseResult {
            request: pending.state_request_id,
        }) {
            Ok(Response::Ok) => true,
            // The broker may expire a retained result between TakeResult and release,
            // or a retry may follow a release whose acknowledgement was lost.
            Ok(Response::Error { code, .. }) if code == "not_found" => true,
            Ok(_) => false,
            Err(failure) => {
                pending.phase = PendingPhase::Releasing(outcome);
                self.pending = Some(pending);
                return Err(failure);
            }
        };

        if !released {
            pending.phase = PendingPhase::Releasing(outcome);
            self.pending = Some(pending);
            return Ok(());
        }

        match outcome {
            ChildOutcome::Payload(payload) => self.consume_state_result(pending, payload, send),
            ChildOutcome::Failure { code, message } => complete(
                send,
                pending.request_id,
                runtime_failure(Response::Error { code, message }),
            ),
        }
    }

    fn consume_state_result(
        &mut self,
        pending: Pending,
        payload: Value,
        send: &mut impl FnMut(Command) -> TransportResult,
    ) -> ModuleResult<()> {
        if payload.get("ok").and_then(Value::as_bool) != Some(true) {
            let failure = if payload.get("ok").and_then(Value::as_bool) == Some(false)
                && payload.get("error").is_some()
            {
                payload
            } else {
                error(
                    "invalid_state_response",
                    "state returned a malformed response",
                )
            };
            return complete(send, pending.request_id, failure);
        }
        let Some(state_revision) = payload.get("revision").and_then(Value::as_u64) else {
            return complete(
                send,
                pending.request_id,
                error("invalid_state_response", "state revision is missing"),
            );
        };
        match pending.stage {
            Stage::Saving(document) => {
                publish_change(send, &pending.key, document.revision);
                complete(send, pending.request_id, success(document))
            }
            Stage::SavingProduct(document) => {
                publish_product_change(send, &pending.key, document.revision);
                complete(send, pending.request_id, product_success(Some(document)))
            }
            Stage::Loading(operation) => {
                if matches!(
                    &operation,
                    Operation::ProductGet {} | Operation::ProductUpdate { .. }
                ) {
                    let existing = match read_product_document(&payload) {
                        Ok(document) => document,
                        Err(failure) => return complete(send, pending.request_id, failure),
                    };
                    return match apply_product(operation, existing) {
                        Ok(ProductChange::Read(document)) => {
                            complete(send, pending.request_id, product_success(document))
                        }
                        Ok(ProductChange::Write(document)) => self.save_product(
                            RequestContext {
                                request_id: pending.request_id,
                                key: pending.key,
                                deadline: pending.deadline,
                            },
                            state_revision,
                            document,
                            send,
                        ),
                        Err(failure) => complete(send, pending.request_id, failure),
                    };
                }
                let existing = match read_document(&payload) {
                    Ok(document) => document,
                    Err(failure) => return complete(send, pending.request_id, failure),
                };
                match apply(operation, existing) {
                    Ok(Change::Read(document)) => {
                        complete(send, pending.request_id, success(document))
                    }
                    Ok(Change::Write(document)) => self.save(
                        RequestContext {
                            request_id: pending.request_id,
                            key: pending.key,
                            deadline: pending.deadline,
                        },
                        state_revision,
                        document,
                        send,
                    ),
                    Err(failure) => complete(send, pending.request_id, failure),
                }
            }
        }
    }

    fn save(
        &mut self,
        context: RequestContext,
        state_revision: u64,
        document: Document,
        send: &mut impl FnMut(Command) -> TransportResult,
    ) -> ModuleResult<()> {
        let RequestContext {
            request_id,
            key,
            deadline,
        } = context;
        if !document_fits(&document) {
            return complete(
                send,
                request_id,
                error(
                    "resource_exhausted",
                    "configuration exceeds document size limit",
                ),
            );
        }
        let payload =
            json!({"action":"set","key":key,"value":document,"expected_revision":state_revision});
        match submit(send, request_id, deadline, "save", payload)? {
            Response::Accepted { id } => {
                self.pending = Some(Pending {
                    request_id,
                    key,
                    deadline,
                    state_request_id: id,
                    stage: Stage::Saving(document),
                    phase: PendingPhase::Waiting,
                });
                Ok(())
            }
            failure => complete(send, request_id, runtime_failure(failure)),
        }
    }

    fn save_product(
        &mut self,
        context: RequestContext,
        state_revision: u64,
        document: ProductDocument,
        send: &mut impl FnMut(Command) -> TransportResult,
    ) -> ModuleResult<()> {
        let RequestContext {
            request_id,
            key,
            deadline,
        } = context;
        if !product_document_fits(&document) {
            return complete(
                send,
                request_id,
                error(
                    "resource_exhausted",
                    "product settings exceed document size limit",
                ),
            );
        }
        let payload =
            json!({"action":"set","key":key,"value":document,"expected_revision":state_revision});
        match submit(send, request_id, deadline, "product_save", payload)? {
            Response::Accepted { id } => {
                self.pending = Some(Pending {
                    request_id,
                    key,
                    deadline,
                    state_request_id: id,
                    stage: Stage::SavingProduct(document),
                    phase: PendingPhase::Waiting,
                });
                Ok(())
            }
            failure => complete(send, request_id, runtime_failure(failure)),
        }
    }
}

fn submit(
    send: &mut impl FnMut(Command) -> TransportResult,
    parent: u64,
    deadline: Instant,
    stage: &str,
    payload: Value,
) -> TransportResult {
    let remaining_ms = deadline
        .saturating_duration_since(Instant::now())
        .as_millis() as u64;
    if remaining_ms == 0 {
        return Ok(Response::error(
            "deadline_exceeded",
            "configuration request deadline elapsed",
        ));
    }
    send(Command::Request {
        service: STATE_SERVICE.into(),
        payload,
        timeout_ms: remaining_ms.min(30_000),
        operation_id: format!("configuration.{parent}.{stage}"),
        parent: Some(parent),
    })
}

fn publish_change(send: &mut impl FnMut(Command) -> TransportResult, key: &str, revision: u64) {
    // Poll supplies the identity used to build this opaque key. Payloads cannot
    // choose it. Events intentionally omit schema names and configuration values.
    let owner = key
        .strip_prefix("principal/")
        .expect("caller-scoped state key");
    // Persistence is already durable: an unavailable event grant, full queue, or
    // transport error must not turn a committed mutation into an apparent failure.
    let _ = send(Command::Publish {
        topic: crate::CHANGED_TOPIC.into(),
        payload: json!({"owner": owner, "revision": revision}),
    });
}

fn publish_product_change(
    send: &mut impl FnMut(Command) -> TransportResult,
    key: &str,
    revision: u64,
) {
    let owner = key
        .strip_prefix("product/")
        .expect("product settings state key");
    let _ = send(Command::Publish {
        topic: crate::CHANGED_TOPIC.into(),
        payload: json!({"owner": owner, "revision": revision, "kind":"product_settings"}),
    });
}

fn complete(
    send: &mut impl FnMut(Command) -> TransportResult,
    request: u64,
    payload: Value,
) -> ModuleResult<()> {
    match send(Command::Complete { request, payload })? {
        Response::Ok => Ok(()),
        Response::Error { code, .. } if code == "request_ended" || code == "not_found" => Ok(()),
        unexpected => Err(format!("configuration completion failed: {unexpected:?}")),
    }
}

fn runtime_failure(response: Response) -> Value {
    match response {
        Response::Error { code, message } => error(&code, message),
        _ => error(
            "invalid_state_response",
            "unexpected state transport response",
        ),
    }
}

fn document_fits(document: &Document) -> bool {
    serde_json::to_vec(document).is_ok_and(|bytes| bytes.len() <= MAX_DOCUMENT_BYTES)
}

fn product_document_fits(document: &ProductDocument) -> bool {
    serde_json::to_vec(document).is_ok_and(|bytes| bytes.len() <= MAX_DOCUMENT_BYTES)
}

fn read_document(payload: &Value) -> Result<Option<Document>, Value> {
    match payload.get("found").and_then(Value::as_bool) {
        Some(false) => Ok(None),
        Some(true) => {
            let document: Document =
                serde_json::from_value(payload.get("value").cloned().unwrap_or(Value::Null))
                    .map_err(|_| {
                        error(
                            "invalid_state_response",
                            "persisted configuration is malformed",
                        )
                    })?;
            if defaults(&document.fields).is_err()
                || validate_values(&document.fields, &document.values).is_err()
                || document.fields.len() != document.values.len()
                || !document_fits(&document)
            {
                return Err(error(
                    "invalid_state_response",
                    "persisted configuration violates its schema",
                ));
            }
            Ok(Some(document))
        }
        None => Err(error(
            "invalid_state_response",
            "state lookup omitted found",
        )),
    }
}

fn read_product_document(payload: &Value) -> Result<Option<ProductDocument>, Value> {
    match payload.get("found").and_then(Value::as_bool) {
        Some(false) => Ok(None),
        Some(true) => {
            let document: ProductDocument =
                serde_json::from_value(payload.get("value").cloned().unwrap_or(Value::Null))
                    .map_err(|_| {
                        error(
                            "invalid_state_response",
                            "persisted product settings are malformed",
                        )
                    })?;
            if document.version == 0 || document.revision == 0 || !product_document_fits(&document)
            {
                return Err(error(
                    "invalid_state_response",
                    "persisted product settings violate their document bounds",
                ));
            }
            Ok(Some(document))
        }
        None => Err(error(
            "invalid_state_response",
            "state lookup omitted found",
        )),
    }
}

#[cfg(test)]
mod tests;
