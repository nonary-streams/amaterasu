use crate::STATE_SERVICE;
use crate::protocol::{
    Change, Document, Operation, ProductChange, ProductDocument, apply, apply_product, error,
    product_success, success,
};
use amaterasu_configuration_schema::{defaults, validate_values};
use amaterasu_module_sdk::events::publish_with;
use amaterasu_module_sdk::events::topics::{
    CONFIGURATION_CHANGED, ConfigurationChanged, ConfigurationKind,
};
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

struct Submission {
    request_id: u64,
    key: String,
    deadline: Instant,
    retry_at: Instant,
    stage: Stage,
    command: Command,
    first_attempt: Instant,
    uncertain: bool,
}

enum PendingPhase {
    Waiting,
    Releasing(ChildOutcome),
}

enum ChildOutcome {
    Payload(Value),
    Failure { code: String, message: String },
    Reconcile(Value),
    RetryReconciliation,
}

enum IntendedWrite {
    Configuration(Document),
    Product(ProductDocument),
}

enum Stage {
    Loading(Operation),
    Saving(Document, u64),
    SavingProduct(ProductDocument, u64),
    ReconcilingWrite {
        intended: IntendedWrite,
        expected_revision: u64,
        failure: Value,
    },
}

#[derive(Default)]
pub struct Pipeline {
    queued: VecDeque<Incoming>,
    pending: Option<Pending>,
    submission: Option<Submission>,
    unconfirmed_write: bool,
    completion: Option<(u64, Value)>,
    quiescing: bool,
}

impl Pipeline {
    pub fn quiesce(
        &mut self,
        mut send: impl FnMut(Command) -> TransportResult,
    ) -> ModuleResult<bool> {
        self.quiescing = true;
        self.tick(&mut send)?;
        Ok(self.pending.is_none()
            && self.submission.is_none()
            && self.completion.is_none()
            && self.queued.is_empty()
            && !self.unconfirmed_write)
    }

    pub fn tick(&mut self, mut send: impl FnMut(Command) -> TransportResult) -> ModuleResult<()> {
        self.flush_completion(&mut send);
        if self.completion.is_some() {
            return Ok(());
        }
        if let Some(submission) = self.submission.take() {
            self.submit_state(submission, &mut send)?;
        }
        if let Some(pending) = self.pending.take() {
            self.advance(pending, &mut send)?;
        }
        if self.completion.is_some() || self.unconfirmed_write {
            return Ok(());
        }
        // Stop polling at capacity so the runtime's own bounded queue applies backpressure.
        if !self.quiescing && self.queued.len() < MAX_QUEUED {
            self.receive(&mut send)?;
        }
        if self.completion.is_some() {
            return Ok(());
        }
        if self.pending.is_none()
            && self.submission.is_none()
            && !self.unconfirmed_write
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
                        return self.complete(
                            send,
                            id,
                            error("invalid_request", failure.to_string()),
                        );
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
        self.begin_state(
            incoming.request_id,
            incoming.key,
            incoming.deadline,
            "load",
            payload,
            Stage::Loading(incoming.operation),
            send,
        )
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
        let is_saving = matches!(&pending.stage, Stage::Saving(..) | Stage::SavingProduct(..));
        let is_reconciling = matches!(&pending.stage, Stage::ReconcilingWrite { .. });
        if is_saving
            && matches!(&result, Response::Error { code, .. } if code != "permission_denied")
        {
            // A terminal broker result can race the state provider's synchronous
            // write. Release this exact child, then enqueue a read barrier behind
            // it before allowing another configuration mutation.
            self.unconfirmed_write = true;
            pending.phase =
                PendingPhase::Releasing(ChildOutcome::Reconcile(runtime_failure(result)));
            self.pending = Some(pending);
            return Ok(());
        }
        if is_reconciling && matches!(&result, Response::Error { .. }) {
            // A failed readback is not a retirement proof. Retire its broker
            // result and enqueue another read barrier without replaying a write.
            pending.phase = PendingPhase::Releasing(ChildOutcome::RetryReconciliation);
            self.pending = Some(pending);
            return Ok(());
        }
        if (is_saving || is_reconciling)
            && !matches!(result, Response::Pending | Response::Result { .. })
        {
            // Unexpected or unauthorized broker status cannot establish a
            // terminal child result. Preserve the exact child and keep new work
            // fenced until polling returns usable evidence.
            if is_saving || is_reconciling {
                self.unconfirmed_write = true;
            }
            self.pending = Some(pending);
            return Ok(());
        }
        match result {
            Response::Pending => {
                self.pending = Some(pending);
                Ok(())
            }
            Response::Result { payload } => {
                let expected_revision = match &pending.stage {
                    Stage::Saving(_, revision) | Stage::SavingProduct(_, revision) => {
                        Some(*revision)
                    }
                    Stage::Loading(_) | Stage::ReconcilingWrite { .. } => None,
                };
                if expected_revision.is_some()
                    && payload.get("ok").and_then(Value::as_bool) == Some(false)
                    && payload.pointer("/error/code").and_then(Value::as_str)
                        == Some("storage_error")
                {
                    // Rename may have committed before directory syncing failed.
                    // Only a read barrier can verify the committed value.
                    self.unconfirmed_write = true;
                    pending.phase = PendingPhase::Releasing(ChildOutcome::Reconcile(payload));
                    self.pending = Some(pending);
                    return Ok(());
                }
                if let Some(expected) = expected_revision
                    && payload.get("ok").and_then(Value::as_bool) != Some(false)
                    && (payload.get("ok").and_then(Value::as_bool) != Some(true)
                        || expected.checked_add(1)
                            != payload.get("revision").and_then(Value::as_u64))
                {
                    // A mismatched or malformed success needs a read barrier
                    // before subsequent state operations can be admitted.
                    self.unconfirmed_write = true;
                    pending.phase = PendingPhase::Releasing(ChildOutcome::Reconcile(error(
                        "invalid_state_response",
                        "state write revision was not confirmed",
                    )));
                    self.pending = Some(pending);
                    return Ok(());
                }
                pending.phase = PendingPhase::Releasing(ChildOutcome::Payload(payload));
                self.release_and_finish(pending, send)
            }
            Response::Error { code, message }
                if code != "not_found" && code != "permission_denied" =>
            {
                pending.phase = PendingPhase::Releasing(ChildOutcome::Failure { code, message });
                self.release_and_finish(pending, send)
            }
            failure @ Response::Error { .. } => {
                self.complete(send, pending.request_id, runtime_failure(failure))
            }
            _ => {
                // An unexpected transport response is not evidence that the
                // child read retired. Preserve its identity and retry later.
                self.pending = Some(pending);
                Ok(())
            }
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
            ChildOutcome::Failure { code, message } => self.complete(
                send,
                pending.request_id,
                runtime_failure(Response::Error { code, message }),
            ),
            ChildOutcome::Reconcile(failure) => {
                self.begin_write_reconciliation(pending, failure, send)
            }
            ChildOutcome::RetryReconciliation => self.retry_write_reconciliation(pending, send),
        }
    }

    fn consume_state_result(
        &mut self,
        pending: Pending,
        payload: Value,
        send: &mut impl FnMut(Command) -> TransportResult,
    ) -> ModuleResult<()> {
        if matches!(&pending.stage, Stage::ReconcilingWrite { .. }) {
            return self.reconcile_state_result(pending, payload, send);
        }
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
            return self.complete(send, pending.request_id, failure);
        }
        let Some(state_revision) = payload.get("revision").and_then(Value::as_u64) else {
            return self.complete(
                send,
                pending.request_id,
                error("invalid_state_response", "state revision is missing"),
            );
        };
        match pending.stage {
            Stage::Saving(document, _) => {
                publish_change(send, &pending.key, document.revision);
                self.complete(send, pending.request_id, success(document))
            }
            Stage::SavingProduct(document, _) => {
                publish_product_change(send, &pending.key, document.revision);
                self.complete(send, pending.request_id, product_success(Some(document)))
            }
            Stage::ReconcilingWrite { .. } => {
                unreachable!("write reconciliation is handled before ordinary state results")
            }
            Stage::Loading(operation) => {
                if matches!(
                    &operation,
                    Operation::ProductGet {} | Operation::ProductUpdate { .. }
                ) {
                    let existing = match read_product_document(&payload) {
                        Ok(document) => document,
                        Err(failure) => return self.complete(send, pending.request_id, failure),
                    };
                    return match apply_product(operation, existing) {
                        Ok(ProductChange::Read(document)) => {
                            self.complete(send, pending.request_id, product_success(document))
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
                        Err(failure) => self.complete(send, pending.request_id, failure),
                    };
                }
                let existing = match read_document(&payload) {
                    Ok(document) => document,
                    Err(failure) => return self.complete(send, pending.request_id, failure),
                };
                match apply(operation, existing) {
                    Ok(Change::Read(document)) => {
                        self.complete(send, pending.request_id, success(document))
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
                    Err(failure) => self.complete(send, pending.request_id, failure),
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
            return self.complete(
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
        self.begin_state(
            request_id,
            key,
            deadline,
            "save",
            payload,
            Stage::Saving(document, state_revision),
            send,
        )
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
            return self.complete(
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
        self.begin_state(
            request_id,
            key,
            deadline,
            "product_save",
            payload,
            Stage::SavingProduct(document, state_revision),
            send,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn begin_state(
        &mut self,
        request_id: u64,
        key: String,
        deadline: Instant,
        operation: &str,
        payload: Value,
        stage: Stage,
        send: &mut impl FnMut(Command) -> TransportResult,
    ) -> ModuleResult<()> {
        let remaining_ms = deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as u64;
        if remaining_ms == 0 {
            return self.complete(
                send,
                request_id,
                error(
                    "deadline_exceeded",
                    "configuration request deadline elapsed",
                ),
            );
        }
        self.submit_state(
            Submission {
                request_id,
                key,
                deadline,
                retry_at: Instant::now(),
                stage,
                command: Command::Request {
                    service: STATE_SERVICE.into(),
                    payload,
                    timeout_ms: remaining_ms.min(30_000),
                    operation_id: format!("configuration.{request_id}.{operation}"),
                    parent: Some(request_id),
                },
                first_attempt: Instant::now(),
                uncertain: false,
            },
            send,
        )
    }

    fn submit_state(
        &mut self,
        mut submission: Submission,
        send: &mut impl FnMut(Command) -> TransportResult,
    ) -> ModuleResult<()> {
        let now = Instant::now();
        let reconciliation = matches!(&submission.stage, Stage::ReconcilingWrite { .. });
        if now < submission.retry_at {
            self.submission = Some(submission);
            return Ok(());
        }
        if !submission.uncertain && now >= submission.deadline {
            if reconciliation {
                submission.deadline = now + Duration::from_secs(30);
                submission.retry_at = now;
                self.submission = Some(submission);
                return Ok(());
            }
            return self.complete(
                send,
                submission.request_id,
                error(
                    "deadline_exceeded",
                    "configuration request deadline elapsed",
                ),
            );
        }
        // A missing lookup is not proof of non-admission after broker retention
        // expires. Never replay a potentially delivered write as new work.
        if submission.uncertain && submission.first_attempt.elapsed() >= Duration::from_secs(300) {
            if matches!(
                &submission.stage,
                Stage::Saving(..) | Stage::SavingProduct(..)
            ) {
                return self.reconcile_uncertain_write_admission(submission);
            }
            if reconciliation {
                // Readback is safe to repeat after its broker record expires;
                // the original mutation is never replayed here.
                submission.uncertain = false;
                submission.first_attempt = now;
                submission.deadline = now + Duration::from_secs(30);
                submission.retry_at = now;
                self.submission = Some(submission);
                return Ok(());
            }
            return self.complete(
                send,
                submission.request_id,
                error(
                    "deadline_exceeded",
                    "configuration state read admission was not confirmed",
                ),
            );
        }
        let command = if submission.uncertain {
            let Command::Request {
                service,
                payload,
                operation_id,
                parent,
                ..
            } = &submission.command
            else {
                unreachable!("state submission always uses a request");
            };
            Command::LookupRequest {
                service: service.clone(),
                payload: payload.clone(),
                operation_id: operation_id.clone(),
                parent: *parent,
            }
        } else {
            submission.first_attempt = now;
            submission.command.clone()
        };
        match send(command) {
            Ok(Response::Accepted { id }) => {
                self.pending = Some(Pending {
                    request_id: submission.request_id,
                    key: submission.key,
                    deadline: submission.deadline,
                    state_request_id: id,
                    stage: submission.stage,
                    phase: PendingPhase::Waiting,
                });
                Ok(())
            }
            Ok(Response::Error { code, .. })
                if submission.uncertain
                    && code == "not_found"
                    && submission.first_attempt.elapsed() < Duration::from_secs(300) =>
            {
                // This synchronous caller has not released or re-submitted the
                // operation, so absence inside the retention window proves it
                // was never admitted. Retry only if the parent remains live.
                if !reconciliation && Instant::now() >= submission.deadline {
                    self.complete(
                        send,
                        submission.request_id,
                        error(
                            "deadline_exceeded",
                            "configuration request deadline elapsed",
                        ),
                    )
                } else {
                    submission.uncertain = false;
                    submission.retry_at = now;
                    if reconciliation {
                        submission.deadline = now + Duration::from_secs(30);
                    }
                    self.submission = Some(submission);
                    Ok(())
                }
            }
            Ok(Response::Error { .. }) if !submission.uncertain && reconciliation => {
                // A rejected readback cannot have reached the provider. Keep the
                // write fence and retry the read after a short backoff.
                submission.retry_at = now + Duration::from_millis(250);
                submission.deadline = now + Duration::from_secs(30);
                self.submission = Some(submission);
                Ok(())
            }
            Ok(response @ Response::Error { .. }) if !submission.uncertain => {
                self.complete(send, submission.request_id, runtime_failure(response))
            }
            _ => {
                // The broker may have admitted the exact operation before its
                // acknowledgement was lost. Resolve it with a lookup that
                // cannot create a second state mutation.
                submission.uncertain = true;
                submission.retry_at = now;
                self.submission = Some(submission);
                Ok(())
            }
        }
    }

    fn begin_write_reconciliation(
        &mut self,
        mut pending: Pending,
        failure: Value,
        send: &mut impl FnMut(Command) -> TransportResult,
    ) -> ModuleResult<()> {
        pending.stage = reconciliation_stage(pending.stage, failure)
            .expect("only a configuration save enters write reconciliation");
        self.unconfirmed_write = true;
        self.submit_reconciliation(pending, send)
    }

    fn retry_write_reconciliation(
        &mut self,
        pending: Pending,
        send: &mut impl FnMut(Command) -> TransportResult,
    ) -> ModuleResult<()> {
        if !matches!(&pending.stage, Stage::ReconcilingWrite { .. }) {
            return Err("configuration reconciliation lost its write identity".into());
        }
        self.submit_reconciliation(pending, send)
    }

    fn submit_reconciliation(
        &mut self,
        pending: Pending,
        send: &mut impl FnMut(Command) -> TransportResult,
    ) -> ModuleResult<()> {
        let operation_id = format!(
            "configuration.{}.reconcile.{}",
            pending.request_id, pending.state_request_id
        );
        let Pending {
            request_id,
            key,
            stage,
            ..
        } = pending;
        let now = Instant::now();
        self.submit_state(
            Submission {
                request_id,
                key: key.clone(),
                deadline: now + Duration::from_secs(30),
                retry_at: now,
                stage,
                command: Command::Request {
                    service: STATE_SERVICE.into(),
                    payload: json!({"action":"get","key":key}),
                    timeout_ms: 30_000,
                    operation_id,
                    // Readback remains possible after the original caller's
                    // parent request expires or is cancelled.
                    parent: None,
                },
                first_attempt: now,
                uncertain: false,
            },
            send,
        )
    }

    fn reconcile_uncertain_write_admission(&mut self, submission: Submission) -> ModuleResult<()> {
        let stage = reconciliation_stage(
            submission.stage,
            error(
                "state_outcome_unknown",
                "state write admission could not be confirmed",
            ),
        )
        .expect("only a state write can have uncertain mutation admission");
        let operation_id = match &submission.command {
            Command::Request { operation_id, .. } => format!("{operation_id}.readback"),
            _ => unreachable!("state submission always uses a request"),
        };
        let now = Instant::now();
        self.unconfirmed_write = true;
        self.submission = Some(Submission {
            request_id: submission.request_id,
            key: submission.key.clone(),
            deadline: now + Duration::from_secs(30),
            retry_at: now,
            stage,
            command: Command::Request {
                service: STATE_SERVICE.into(),
                payload: json!({"action":"get","key":submission.key}),
                timeout_ms: 30_000,
                operation_id,
                parent: None,
            },
            first_attempt: now,
            uncertain: false,
        });
        Ok(())
    }

    fn reconcile_state_result(
        &mut self,
        pending: Pending,
        payload: Value,
        send: &mut impl FnMut(Command) -> TransportResult,
    ) -> ModuleResult<()> {
        let Stage::ReconcilingWrite {
            intended,
            expected_revision,
            failure,
        } = pending.stage
        else {
            unreachable!("write reconciliation stage was checked")
        };
        let state_revision = payload.get("revision").and_then(Value::as_u64);
        let intended_value = match &intended {
            IntendedWrite::Configuration(document) => serde_json::to_value(document).ok(),
            IntendedWrite::Product(document) => serde_json::to_value(document).ok(),
        };
        let committed = payload.get("ok").and_then(Value::as_bool) == Some(true)
            && state_revision == expected_revision.checked_add(1)
            && payload.get("found").and_then(Value::as_bool) == Some(true)
            && intended_value
                .as_ref()
                .is_some_and(|value| payload.get("value") == Some(value));
        let unchanged = payload.get("ok").and_then(Value::as_bool) == Some(true)
            && state_revision == Some(expected_revision);

        // A terminal read was queued behind the previous State operation. It
        // proves that operation returned, even if its caller result was cancelled.
        self.unconfirmed_write = false;
        if committed {
            match intended {
                IntendedWrite::Configuration(document) => {
                    publish_change(send, &pending.key, document.revision);
                    self.complete(send, pending.request_id, success(document))
                }
                IntendedWrite::Product(document) => {
                    publish_product_change(send, &pending.key, document.revision);
                    self.complete(send, pending.request_id, product_success(Some(document)))
                }
            }
        } else if unchanged {
            self.complete(send, pending.request_id, failure)
        } else {
            self.complete(
                send,
                pending.request_id,
                error(
                    "state_outcome_unknown",
                    "state readback did not confirm the intended revision and value",
                ),
            )
        }
    }

    fn complete(
        &mut self,
        send: &mut impl FnMut(Command) -> TransportResult,
        request: u64,
        payload: Value,
    ) -> ModuleResult<()> {
        self.completion = Some((request, payload));
        self.flush_completion(send);
        Ok(())
    }

    fn flush_completion(&mut self, send: &mut impl FnMut(Command) -> TransportResult) {
        let Some((request, payload)) = self.completion.as_ref() else {
            return;
        };
        let response = send(Command::Complete {
            request: *request,
            payload: payload.clone(),
        });
        if matches!(&response, Ok(Response::Ok))
            || matches!(&response, Ok(Response::Error { code, .. }) if code == "request_ended" || code == "not_found")
        {
            self.completion = None;
        }
    }
}

fn reconciliation_stage(stage: Stage, failure: Value) -> Option<Stage> {
    let (intended, expected_revision) = match stage {
        Stage::Saving(document, revision) => (IntendedWrite::Configuration(document), revision),
        Stage::SavingProduct(document, revision) => (IntendedWrite::Product(document), revision),
        Stage::Loading(_) | Stage::ReconcilingWrite { .. } => return None,
    };
    Some(Stage::ReconcilingWrite {
        intended,
        expected_revision,
        failure,
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
    let change = ConfigurationChanged {
        owner: owner.to_owned(),
        revision,
        kind: None,
    };
    let _ = publish_with(send, &CONFIGURATION_CHANGED, &change);
}

fn publish_product_change(
    send: &mut impl FnMut(Command) -> TransportResult,
    key: &str,
    revision: u64,
) {
    let owner = key
        .strip_prefix("product/")
        .expect("product settings state key");
    let change = ConfigurationChanged {
        owner: owner.to_owned(),
        revision,
        kind: Some(ConfigurationKind::ProductSettings),
    };
    let _ = publish_with(send, &CONFIGURATION_CHANGED, &change);
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
