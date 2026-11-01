mod transaction;
use transaction::state_mutation_revision;

use crate::path::{effective_as_value, effective_settings};
use crate::protocol::{Operation, error, unavailable};
use crate::settings::{ConfigurationTransaction, ConfigurationTransactionState, Document};
use amaterasu_module_sdk::effect_authority::ConfigurationChange;
use amaterasu_module_sdk::{Command, ModuleResult, Response};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::time::{Duration, Instant};

const STATE_KEY: &str = "global/amaterasu.host-configuration";
const MAX_DOCUMENT_BYTES: usize = 12_000;
const MAX_RESPONSE_BYTES: usize = 16_384;
const MAX_REQUEST_MS: u64 = 30_000;
const DIAGNOSTICS_SERVICE: &str = "amaterasu.diagnostics/1";

type TransportResult = ModuleResult<Response>;

struct Incoming {
    request_id: u64,
    operation: Operation,
    deadline: Instant,
}

struct UpdateToSave {
    request_id: u64,
    deadline: Instant,
    state_revision: u64,
    document: Document,
    previous: Document,
    response: Value,
    allow_unready_bootstrap: bool,
}

struct HoldCompletion {
    request_id: u64,
    deadline: Instant,
    expected_state_revision: u64,
    document: Document,
    response: Value,
    current_authority_epoch: Option<String>,
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
    Preflighting(UpdateToSave),
    SelectingAuthority(UpdateToSave),
    PreparingHold {
        update: UpdateToSave,
        change: ConfigurationChange,
    },
    BeginningHold {
        update: UpdateToSave,
        change: ConfigurationChange,
    },
    ConfirmingHold {
        response: Value,
        document: Document,
        expected_state_revision: u64,
    },
    FinishingHold {
        response: Value,
        document: Document,
        expected_state_revision: u64,
        current_authority_epoch: Option<String>,
    },
    ClearingHold {
        response: Value,
        expected_state_revision: u64,
    },
    ReconcilingHold {
        document: Document,
        expected_state_revision: u64,
    },
    SelectingReconcile {
        document: Document,
        state_revision: u64,
    },
    Saving {
        expected_state_revision: u64,
        response: Value,
        previous: Document,
    },
    Applying {
        response: Value,
        previous: Document,
        saved_state_revision: u64,
    },
    RollingBack {
        saved_revision: u64,
        expected_state_revision: u64,
        original_deadline: Instant,
        restored_configuration: Option<Value>,
    },
    Compensating {
        saved_revision: u64,
        restored_revision: u64,
        parented: bool,
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
    concurrent_read: Option<Pending>,
    effect_authority_owner: Option<String>,
    detached_sequence: u64,
    transaction_epoch: Option<String>,
}

impl Pipeline {
    pub fn new(trusted_root: Result<PathBuf, String>) -> Self {
        let mut nonce = [0_u8; 16];
        let transaction_epoch = getrandom::fill(&mut nonce)
            .ok()
            .map(|()| nonce.iter().map(|byte| format!("{byte:02x}")).collect());
        Self {
            trusted_root,
            pending: None,
            concurrent_read: None,
            effect_authority_owner: None,
            detached_sequence: 0,
            transaction_epoch,
        }
    }

    pub fn enable_effect_authority(&mut self, owner: String) {
        self.effect_authority_owner = Some(owner);
    }

    pub fn tick(&mut self, mut send: impl FnMut(Command) -> TransportResult) -> ModuleResult<()> {
        if let Some(pending) = self.pending.take() {
            self.advance(pending, &mut send)?;
        }
        let primary = self.pending.take();
        if let Some(read) = self.concurrent_read.take() {
            let result = self.advance(read, &mut send);
            self.concurrent_read = self.pending.take();
            self.pending = primary;
            result?;
        } else {
            self.pending = primary;
        }
        if self.concurrent_read.is_none() {
            if self.pending.is_some() {
                let primary = self.pending.take();
                let result = self.receive(true, &mut send);
                self.concurrent_read = self.pending.take();
                self.pending = primary;
                result?;
            } else {
                self.receive(false, &mut send)?;
            }
        }
        Ok(())
    }

    fn receive(
        &mut self,
        read_only: bool,
        send: &mut impl FnMut(Command) -> TransportResult,
    ) -> ModuleResult<()> {
        match send(Command::Poll {})? {
            Response::Pending => Ok(()),
            Response::Request {
                id,
                caller,
                initiator,
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
                if matches!(operation, Operation::TransactionStatus {})
                    && ((caller != "amaterasu.management"
                        && self.effect_authority_owner.as_deref() != Some(caller.as_str()))
                        || initiator != caller)
                {
                    return complete(
                        send,
                        id,
                        error(
                            "permission_denied",
                            "transaction status is restricted to pairing",
                        ),
                    );
                }
                if matches!(operation, Operation::ReconcileTransaction { .. })
                    && (caller != "amaterasu.management" || initiator != caller)
                {
                    return complete(
                        send,
                        id,
                        error(
                            "permission_denied",
                            "only management may reconcile configuration transactions",
                        ),
                    );
                }
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
                if read_only && matches!(operation, Operation::Update { .. }) {
                    return complete(send, id, error("busy", "configuration update is pending"));
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
            let response = match pending.stage {
                Stage::Saving { response, .. } => {
                    save_unconfirmed(response["revision"].as_u64().unwrap_or_default())
                }
                Stage::Applying { response, .. } => {
                    application_unconfirmed(response["revision"].as_u64().unwrap_or_default())
                }
                Stage::RollingBack { saved_revision, .. } => rollback_unconfirmed(
                    saved_revision,
                    "compensating write timed out; inspect current configuration before retrying",
                ),
                Stage::Compensating {
                    saved_revision,
                    restored_revision,
                    ..
                } => rollback_application_unconfirmed(
                    saved_revision,
                    restored_revision,
                    "restored logging apply timed out",
                ),
                Stage::Loading(_) => {
                    error("deadline_exceeded", "host configuration request expired")
                }
                Stage::Preflighting(_) => unavailable(
                    "diagnostics status preflight expired before configuration was saved",
                ),
                Stage::SelectingAuthority(_)
                | Stage::PreparingHold { .. }
                | Stage::BeginningHold { .. }
                | Stage::ConfirmingHold { .. }
                | Stage::FinishingHold { .. }
                | Stage::ClearingHold { .. }
                | Stage::ReconcilingHold { .. }
                | Stage::SelectingReconcile { .. } => error(
                    "configuration_transaction_unconfirmed",
                    "configuration transaction requires reconciliation",
                ),
            };
            return complete(send, pending.request_id, response);
        }
        if matches!(&pending.phase, PendingPhase::Releasing(_)) {
            let outcome = match std::mem::replace(&mut pending.phase, PendingPhase::Waiting) {
                PendingPhase::Releasing(outcome) => outcome,
                PendingPhase::Waiting => unreachable!(),
            };
            return self.release_and_consume(pending, outcome, send);
        }

        let result = match send(Command::TakeResult {
            request: pending.state_request_id,
        }) {
            Ok(result) => result,
            Err(_) => {
                self.pending = Some(pending);
                return Ok(());
            }
        };
        match result {
            Response::Pending => {
                self.pending = Some(pending);
                Ok(())
            }
            Response::Result { payload } => {
                self.release_and_consume(pending, ChildOutcome::Payload(payload), send)
            }
            Response::Error { code, message } => {
                self.release_and_consume(pending, ChildOutcome::Failure { code, message }, send)
            }
            response => self.release_and_consume(
                pending,
                ChildOutcome::Failure {
                    code: "invalid_child_response".into(),
                    message: format!("unexpected child response: {response:?}"),
                },
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
            ChildOutcome::Failure { code, message } => match pending.stage {
                Stage::Applying {
                    previous,
                    saved_state_revision,
                    response,
                } if code == "permission_denied" => self.rollback(
                    pending.request_id,
                    pending.deadline,
                    saved_state_revision,
                    previous,
                    response["revision"].as_u64().unwrap_or_default(),
                    send,
                ),
                Stage::Applying { response, .. } => complete(
                    send,
                    pending.request_id,
                    application_unconfirmed(response["revision"].as_u64().unwrap_or_default()),
                ),
                Stage::Saving { response, .. } => complete(
                    send,
                    pending.request_id,
                    save_unconfirmed(response["revision"].as_u64().unwrap_or_default()),
                ),
                Stage::RollingBack { saved_revision, .. } => complete(
                    send,
                    pending.request_id,
                    rollback_unconfirmed(
                        saved_revision,
                        format!("state service failure ({code}): {message}"),
                    ),
                ),
                Stage::Compensating {
                    saved_revision,
                    restored_revision,
                    ..
                } => complete(
                    send,
                    pending.request_id,
                    rollback_application_unconfirmed(
                        saved_revision,
                        restored_revision,
                        format!("restored logging apply failed ({code}): {message}"),
                    ),
                ),
                Stage::Preflighting(_) => complete(
                    send,
                    pending.request_id,
                    unavailable(format!(
                        "diagnostics status preflight failed ({code}): {message}"
                    )),
                ),
                Stage::SelectingAuthority(_)
                | Stage::PreparingHold { .. }
                | Stage::BeginningHold { .. }
                | Stage::ConfirmingHold { .. }
                | Stage::FinishingHold { .. }
                | Stage::ClearingHold { .. }
                | Stage::ReconcilingHold { .. }
                | Stage::SelectingReconcile { .. } => complete(
                    send,
                    pending.request_id,
                    error(
                        "configuration_transaction_unconfirmed",
                        "configuration transaction requires reconciliation",
                    ),
                ),
                _ => self.finish_child_failure(pending.request_id, code, message, send),
            },
            ChildOutcome::Payload(payload) => {
                if payload.get("ok").and_then(Value::as_bool) != Some(true) {
                    let message = payload
                        .pointer("/error/message")
                        .and_then(Value::as_str)
                        .unwrap_or("state returned an unsuccessful response");
                    return match pending.stage {
                        Stage::Applying {
                            previous,
                            saved_state_revision,
                            response,
                        } => self.rollback(
                            pending.request_id,
                            pending.deadline,
                            saved_state_revision,
                            previous,
                            response["revision"].as_u64().unwrap_or_default(),
                            send,
                        ),
                        Stage::RollingBack { saved_revision, .. } => {
                            let response = if payload.pointer("/error/code").and_then(Value::as_str)
                                == Some("revision_conflict")
                            {
                                rollback_failed(saved_revision, message)
                            } else {
                                rollback_unconfirmed(saved_revision, message)
                            };
                            complete(send, pending.request_id, response)
                        }
                        Stage::Compensating {
                            saved_revision,
                            restored_revision,
                            ..
                        } => complete(
                            send,
                            pending.request_id,
                            rollback_application_unconfirmed(
                                saved_revision,
                                restored_revision,
                                message,
                            ),
                        ),
                        Stage::Saving { response, .. } => complete(
                            send,
                            pending.request_id,
                            save_unconfirmed(response["revision"].as_u64().unwrap_or_default()),
                        ),
                        Stage::Preflighting(_) => complete(
                            send,
                            pending.request_id,
                            unavailable("diagnostics status preflight did not confirm readiness"),
                        ),
                        Stage::SelectingAuthority(_)
                        | Stage::PreparingHold { .. }
                        | Stage::BeginningHold { .. }
                        | Stage::ConfirmingHold { .. }
                        | Stage::FinishingHold { .. }
                        | Stage::ClearingHold { .. }
                        | Stage::ReconcilingHold { .. }
                        | Stage::SelectingReconcile { .. } => complete(
                            send,
                            pending.request_id,
                            error(
                                "configuration_transaction_unconfirmed",
                                "configuration transaction requires reconciliation",
                            ),
                        ),
                        _ => complete(send, pending.request_id, unavailable(message)),
                    };
                }
                if let Stage::Preflighting(update) = pending.stage {
                    if !valid_diagnostics_status(&payload) {
                        return complete(
                            send,
                            pending.request_id,
                            unavailable("diagnostics status preflight was malformed"),
                        );
                    }
                    let ready = payload.get("ready").and_then(Value::as_bool) == Some(true);
                    let initial_unready = update.allow_unready_bootstrap
                        && payload.get("ready").and_then(Value::as_bool) == Some(false)
                        && payload
                            .get("logging_configuration_revision")
                            .is_some_and(Value::is_null)
                        && payload
                            .pointer("/logging_sink/state")
                            .and_then(Value::as_str)
                            == Some("disabled")
                        && payload
                            .pointer("/logging_sink/failure")
                            .is_some_and(Value::is_null);
                    if !ready && !initial_unready {
                        return complete(
                            send,
                            pending.request_id,
                            unavailable("diagnostics status preflight is not ready"),
                        );
                    }
                    if self.effect_authority_owner.is_some()
                        && identity_settings_changed(&update.previous, &update.document)
                    {
                        return self.select_authority(update, send);
                    }
                    return self.save(update, send);
                }
                if let Stage::SelectingAuthority(update) = pending.stage {
                    let Some(epoch) = payload["epoch"].as_str() else {
                        return complete(
                            send,
                            pending.request_id,
                            unavailable("effect authority omitted its epoch"),
                        );
                    };
                    let Some(trust_revision) = payload["revisions"]["trust"].as_u64() else {
                        return complete(
                            send,
                            pending.request_id,
                            unavailable("effect authority omitted its trust revision"),
                        );
                    };
                    if payload["revisions"]["configuration"].as_u64()
                        != Some(update.previous.revision)
                    {
                        return complete(
                            send,
                            pending.request_id,
                            error(
                                "revision_conflict",
                                "pairing identity has not observed the expected configuration",
                            ),
                        );
                    }
                    let change = ConfigurationChange {
                        version: 1,
                        authority_epoch: epoch.to_owned(),
                        operation_id: format!("host-configuration.{}", update.request_id),
                        expected_configuration_revision: update.previous.revision,
                        expected_trust_revision: trust_revision,
                    };
                    if !change.valid() {
                        return complete(
                            send,
                            pending.request_id,
                            unavailable("effect authority returned an invalid epoch"),
                        );
                    }
                    return self.prepare_hold(update, change, send);
                }
                if let Stage::PreparingHold { mut update, change } = pending.stage {
                    let expected = update.state_revision.checked_add(1);
                    if state_mutation_revision(&payload) != expected {
                        return complete(
                            send,
                            pending.request_id,
                            error(
                                "configuration_transaction_unconfirmed",
                                "transaction marker write was not confirmed",
                            ),
                        );
                    }
                    update.state_revision = expected.unwrap();
                    let marker = ConfigurationTransaction {
                        change: change.clone(),
                        state: ConfigurationTransactionState::Prepared {},
                        expected_state_revision: update.state_revision,
                        settled: false,
                    };
                    update.previous.transaction = Some(marker.clone());
                    update.document.transaction = Some(ConfigurationTransaction {
                        state: ConfigurationTransactionState::Committed {
                            configuration_revision: update.document.revision,
                        },
                        ..marker
                    });
                    return self.begin_hold(update, change, send);
                }
                if let Stage::BeginningHold { update, change } = pending.stage {
                    if payload["epoch"].as_str() != Some(change.authority_epoch.as_str())
                        || payload["operation_id"].as_str() != Some(change.operation_id.as_str())
                    {
                        return complete(
                            send,
                            pending.request_id,
                            error(
                                "configuration_transaction_unconfirmed",
                                "effect authority returned a mismatched hold",
                            ),
                        );
                    }
                    if payload["state"] == "pending" {
                        return self.begin_hold(update, change, send);
                    }
                    if payload["state"] != "drained" {
                        return complete(
                            send,
                            pending.request_id,
                            error(
                                "configuration_transaction_unconfirmed",
                                "effect authority did not confirm retirement",
                            ),
                        );
                    }
                    if !matches!(
                        send(Command::CheckRequest {
                            request: update.request_id
                        })?,
                        Response::Ok
                    ) {
                        return complete(
                            send,
                            update.request_id,
                            error(
                                "configuration_transaction_unconfirmed",
                                "request ended while the transaction was held",
                            ),
                        );
                    }
                    return self.save(update, send);
                }
                if let Stage::ConfirmingHold {
                    response,
                    document,
                    expected_state_revision,
                } = pending.stage
                {
                    if state_mutation_revision(&payload) != expected_state_revision.checked_add(1) {
                        return complete(
                            send,
                            pending.request_id,
                            error(
                                "configuration_transaction_unconfirmed",
                                "commit fence was not confirmed",
                            ),
                        );
                    }
                    return self.finish_hold(
                        HoldCompletion {
                            request_id: pending.request_id,
                            deadline: pending.deadline,
                            expected_state_revision: expected_state_revision + 1,
                            document,
                            response,
                            current_authority_epoch: None,
                        },
                        send,
                    );
                }
                if let Stage::FinishingHold {
                    response,
                    document,
                    expected_state_revision,
                    current_authority_epoch,
                } = pending.stage
                {
                    let Some(transaction) = document.transaction.as_ref() else {
                        return complete(
                            send,
                            pending.request_id,
                            error(
                                "configuration_transaction_unconfirmed",
                                "transaction marker is missing",
                            ),
                        );
                    };
                    if payload["state"] == "pending"
                        && payload["operation_id"].as_str()
                            == Some(transaction.change.operation_id.as_str())
                    {
                        return self.finish_hold(
                            HoldCompletion {
                                request_id: pending.request_id,
                                deadline: pending.deadline,
                                expected_state_revision,
                                document,
                                response,
                                current_authority_epoch,
                            },
                            send,
                        );
                    }
                    if payload["state"] != "released"
                        || payload["operation_id"].as_str()
                            != Some(transaction.change.operation_id.as_str())
                    {
                        return complete(
                            send,
                            pending.request_id,
                            error(
                                "configuration_transaction_unconfirmed",
                                "pairing did not confirm the committed identity",
                            ),
                        );
                    }
                    return self.clear_hold(
                        pending.request_id,
                        pending.deadline,
                        expected_state_revision,
                        document,
                        response,
                        send,
                    );
                }
                if let Stage::ClearingHold {
                    response,
                    expected_state_revision,
                } = pending.stage
                {
                    if state_mutation_revision(&payload) != expected_state_revision.checked_add(1) {
                        return complete(
                            send,
                            pending.request_id,
                            error(
                                "configuration_transaction_unconfirmed",
                                "transaction marker removal was not confirmed",
                            ),
                        );
                    }
                    return complete(send, pending.request_id, response);
                }
                if let Stage::ReconcilingHold {
                    document,
                    expected_state_revision,
                } = pending.stage
                {
                    if state_mutation_revision(&payload) != expected_state_revision.checked_add(1) {
                        return complete(
                            send,
                            pending.request_id,
                            error(
                                "configuration_transaction_unconfirmed",
                                "reconciliation CAS was not confirmed",
                            ),
                        );
                    }
                    return self.select_reconcile_authority(
                        pending.request_id,
                        pending.deadline,
                        expected_state_revision + 1,
                        document,
                        send,
                    );
                }
                if let Stage::SelectingReconcile {
                    document,
                    state_revision,
                } = pending.stage
                {
                    let Some(epoch) = payload["epoch"].as_str() else {
                        return complete(
                            send,
                            pending.request_id,
                            error(
                                "configuration_transaction_unconfirmed",
                                "current authority epoch is unavailable",
                            ),
                        );
                    };
                    let response = json!({"ok":true,"state":"reconciled","configuration_revision":document.revision,"logging":"unconfirmed"});
                    return self.finish_hold(
                        HoldCompletion {
                            request_id: pending.request_id,
                            deadline: pending.deadline,
                            expected_state_revision: state_revision,
                            document,
                            response,
                            current_authority_epoch: Some(epoch.to_owned()),
                        },
                        send,
                    );
                }
                if let Stage::Applying {
                    response,
                    previous,
                    saved_state_revision,
                } = pending.stage
                {
                    let saved_revision = response["revision"].as_u64().unwrap_or_default();
                    if payload.get("applied_revision").and_then(Value::as_u64)
                        == Some(saved_revision)
                    {
                        let mut response = response;
                        response["logging"] = json!({
                            "saved_revision":saved_revision,
                            "applied_revision":saved_revision,
                            "status":"applied"
                        });
                        if previous.transaction.is_some() {
                            return self.confirm_hold(
                                pending.request_id,
                                pending.deadline,
                                saved_state_revision,
                                previous,
                                response,
                                send,
                            );
                        }
                        return complete(send, pending.request_id, response);
                    }
                    let _ = (previous, saved_state_revision);
                    return complete(
                        send,
                        pending.request_id,
                        application_unconfirmed(saved_revision),
                    );
                }
                if let Stage::Compensating {
                    saved_revision,
                    restored_revision,
                    parented,
                } = pending.stage
                {
                    if payload.get("applied_revision").and_then(Value::as_u64)
                        == Some(restored_revision)
                        && parented
                    {
                        return complete(
                            send,
                            pending.request_id,
                            json!({
                                "ok":false,
                                "error":{"code":"logging_apply_failed","message":"logging policy could not be applied; previous configuration and logging policy were restored"},
                                "logging":{"saved_revision":saved_revision,"restored_revision":restored_revision,"applied_revision":restored_revision,"status":"rolled_back"}
                            }),
                        );
                    }
                    return complete(
                        send,
                        pending.request_id,
                        rollback_application_unconfirmed(
                            saved_revision,
                            restored_revision,
                            "restored logging application was not confirmed within the management request",
                        ),
                    );
                }
                let state_revision = match payload.get("revision").and_then(Value::as_u64) {
                    Some(revision) => revision,
                    None => {
                        let response = match pending.stage {
                            Stage::RollingBack { saved_revision, .. } => rollback_unconfirmed(
                                saved_revision,
                                "compensating write response omitted its revision",
                            ),
                            Stage::Saving { response, .. } => {
                                save_unconfirmed(response["revision"].as_u64().unwrap_or_default())
                            }
                            _ => unavailable("state response omitted its revision"),
                        };
                        return complete(send, pending.request_id, response);
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
                        previous,
                    } => {
                        let Some(written_revision) =
                            state_revision.checked_sub(expected_state_revision)
                        else {
                            return complete(
                                send,
                                pending.request_id,
                                save_unconfirmed(response["revision"].as_u64().unwrap_or_default()),
                            );
                        };
                        if written_revision != 1 {
                            return complete(
                                send,
                                pending.request_id,
                                save_unconfirmed(response["revision"].as_u64().unwrap_or_default()),
                            );
                        }
                        self.apply_logging(
                            pending.request_id,
                            pending.deadline,
                            written_revision + expected_state_revision,
                            previous,
                            response,
                            send,
                        )
                    }
                    Stage::RollingBack {
                        saved_revision,
                        expected_state_revision,
                        original_deadline,
                        restored_configuration,
                    } => {
                        if state_revision != expected_state_revision.saturating_add(1) {
                            return complete(
                                send,
                                pending.request_id,
                                rollback_unconfirmed(
                                    saved_revision,
                                    "state did not confirm the compensating write",
                                ),
                            );
                        }
                        self.compensate_logging(
                            pending.request_id,
                            original_deadline,
                            saved_revision,
                            restored_configuration,
                            send,
                        )
                    }
                    Stage::Applying { .. } => unreachable!(),
                    Stage::Preflighting(_) => unreachable!(),
                    Stage::SelectingAuthority(_)
                    | Stage::PreparingHold { .. }
                    | Stage::BeginningHold { .. }
                    | Stage::ConfirmingHold { .. }
                    | Stage::FinishingHold { .. }
                    | Stage::ClearingHold { .. }
                    | Stage::ReconcilingHold { .. }
                    | Stage::SelectingReconcile { .. } => unreachable!(),
                    Stage::Compensating { .. } => unreachable!(),
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
            Operation::TransactionStatus {} => {
                let status =
                    amaterasu_module_sdk::effect_authority::HostConfigurationTransactionStatus {
                        ok: true,
                        configuration_revision: document.revision,
                        transaction: document.transaction,
                    };
                let payload = serde_json::to_value(status)
                    .map_err(|_| "transaction status serialization failed")?;
                complete(send, request_id, payload)
            }
            Operation::ReconcileTransaction { change } => self.reconcile_transaction(
                request_id,
                deadline,
                state_revision,
                document,
                change,
                send,
            ),
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
                if document.transaction.is_some() {
                    return complete(
                        send,
                        request_id,
                        error(
                            "configuration_change_pending",
                            "a configuration transaction requires reconciliation",
                        ),
                    );
                }
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
                if revision.checked_add(1).is_none() {
                    return complete(
                        send,
                        request_id,
                        error(
                            "revision_exhausted",
                            "configuration revision cannot support rollback",
                        ),
                    );
                }
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
                let apply_request = json!({"action":"apply_host_logging","configuration":response});
                let success_response = json!({
                    "logging":{"saved_revision":revision,"applied_revision":revision,"status":"applied"},
                    "configuration":response
                });
                if !value_fits(&apply_request) || !value_fits(&success_response) {
                    return complete(
                        send,
                        request_id,
                        error(
                            "resource_exhausted",
                            "logging update exceeds the runtime payload limit",
                        ),
                    );
                }
                let allow_unready_bootstrap =
                    state_revision == 0 && document == Document::default();
                self.preflight(
                    UpdateToSave {
                        request_id,
                        deadline,
                        state_revision,
                        document: next,
                        previous: document,
                        response,
                        allow_unready_bootstrap,
                    },
                    send,
                )
            }
        }
    }

    fn preflight(
        &mut self,
        update: UpdateToSave,
        send: &mut impl FnMut(Command) -> TransportResult,
    ) -> ModuleResult<()> {
        let request = submit_child(
            send,
            update.request_id,
            update.deadline,
            "diagnostics-status",
            DIAGNOSTICS_SERVICE,
            json!({"action":"status"}),
        );
        let Some(child_id) = request.ok().flatten() else {
            return complete(
                send,
                update.request_id,
                unavailable(
                    "diagnostics status preflight was not accepted; configuration was not saved",
                ),
            );
        };
        self.pending = Some(Pending {
            request_id: update.request_id,
            deadline: update.deadline,
            state_request_id: child_id,
            stage: Stage::Preflighting(update),
            phase: PendingPhase::Waiting,
        });
        Ok(())
    }

    fn save(
        &mut self,
        update: UpdateToSave,
        send: &mut impl FnMut(Command) -> TransportResult,
    ) -> ModuleResult<()> {
        let state_request = submit_state(
            send,
            update.request_id,
            update.deadline,
            "save",
            json!({
                "action":"set","key":STATE_KEY,
                "expected_revision":update.state_revision,"value":update.document
            }),
        );
        let Some(state_request_id) = state_request.ok().flatten() else {
            return complete(
                send,
                update.request_id,
                save_unconfirmed(update.document.revision),
            );
        };
        self.pending = Some(Pending {
            request_id: update.request_id,
            deadline: update.deadline,
            state_request_id,
            stage: Stage::Saving {
                expected_state_revision: update.state_revision,
                response: update.response,
                previous: update.previous,
            },
            phase: PendingPhase::Waiting,
        });
        Ok(())
    }

    fn apply_logging(
        &mut self,
        request_id: u64,
        deadline: Instant,
        saved_state_revision: u64,
        previous: Document,
        response: Value,
        send: &mut impl FnMut(Command) -> TransportResult,
    ) -> ModuleResult<()> {
        let saved_revision = response["revision"].as_u64().unwrap_or_default();
        let remaining_ms = deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as u64;
        if remaining_ms == 0 {
            return complete(send, request_id, application_unconfirmed(saved_revision));
        }
        let result = send(Command::Request {
            service: DIAGNOSTICS_SERVICE.into(),
            payload: json!({"action":"apply_host_logging","configuration":response}),
            timeout_ms: remaining_ms.min(MAX_REQUEST_MS),
            operation_id: format!("host-configuration.{request_id}.apply-logging"),
            parent: Some(request_id),
        });
        let child_id = match result {
            Ok(Response::Accepted { id }) => id,
            Ok(Response::Error { code, .. }) if code == "permission_denied" => {
                return self.rollback(
                    request_id,
                    deadline,
                    saved_state_revision,
                    previous,
                    saved_revision,
                    send,
                );
            }
            _ => return complete(send, request_id, application_unconfirmed(saved_revision)),
        };
        self.pending = Some(Pending {
            request_id,
            deadline,
            state_request_id: child_id,
            stage: Stage::Applying {
                response,
                previous,
                saved_state_revision,
            },
            phase: PendingPhase::Waiting,
        });
        Ok(())
    }

    fn rollback(
        &mut self,
        request_id: u64,
        original_deadline: Instant,
        expected_state_revision: u64,
        mut previous: Document,
        saved_revision: u64,
        send: &mut impl FnMut(Command) -> TransportResult,
    ) -> ModuleResult<()> {
        previous.revision = saved_revision
            .checked_add(1)
            .ok_or("configuration revision exhausted during rollback")?;
        if let Some(transaction) = &mut previous.transaction {
            transaction.state = ConfigurationTransactionState::Aborted {
                configuration_revision: previous.revision,
            };
            transaction.settled = false;
        }
        let restored_configuration = self
            .trusted_root
            .as_ref()
            .ok()
            .and_then(|root| build_read_response(&previous, root).ok());
        let deadline = Instant::now() + Duration::from_millis(MAX_REQUEST_MS);
        let child_id = send(Command::Request {
            service: crate::STATE_SERVICE.into(),
            payload: json!({
                "action":"set","key":STATE_KEY,
                "expected_revision":expected_state_revision,"value":previous
            }),
            timeout_ms: MAX_REQUEST_MS,
            operation_id: format!("host-configuration.{request_id}.rollback-logging"),
            parent: None,
        });
        let Some(child_id) = (match child_id {
            Ok(Response::Accepted { id }) => Some(id),
            _ => None,
        }) else {
            return complete(
                send,
                request_id,
                rollback_unconfirmed(
                    saved_revision,
                    "state service did not confirm the compensating write",
                ),
            );
        };
        self.pending = Some(Pending {
            request_id,
            deadline,
            state_request_id: child_id,
            stage: Stage::RollingBack {
                saved_revision,
                expected_state_revision,
                original_deadline,
                restored_configuration,
            },
            phase: PendingPhase::Waiting,
        });
        Ok(())
    }

    fn compensate_logging(
        &mut self,
        request_id: u64,
        original_deadline: Instant,
        saved_revision: u64,
        restored_configuration: Option<Value>,
        send: &mut impl FnMut(Command) -> TransportResult,
    ) -> ModuleResult<()> {
        let restored_revision = saved_revision + 1;
        let Some(restored_configuration) = restored_configuration else {
            return complete(
                send,
                request_id,
                rollback_application_unconfirmed(
                    saved_revision,
                    restored_revision,
                    "restored configuration cannot be returned safely",
                ),
            );
        };
        let remaining_ms = original_deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as u64;
        let parented_result = (remaining_ms > 0).then(|| {
            send(Command::Request {
                service: DIAGNOSTICS_SERVICE.into(),
                payload: json!({"action":"apply_host_logging","configuration":restored_configuration}),
                timeout_ms: remaining_ms.min(MAX_REQUEST_MS),
                operation_id: format!("host-configuration.{request_id}.restore-logging"),
                parent: Some(request_id),
            })
        });
        let use_detached = match parented_result {
            None => true,
            Some(Ok(Response::Error { code, .. }))
                if code == "permission_denied"
                    || code == "request_ended"
                    || code == "not_found" =>
            {
                true
            }
            Some(Ok(Response::Accepted { id })) => {
                self.pending = Some(Pending {
                    request_id,
                    deadline: original_deadline,
                    state_request_id: id,
                    stage: Stage::Compensating {
                        saved_revision,
                        restored_revision,
                        parented: true,
                    },
                    phase: PendingPhase::Waiting,
                });
                return Ok(());
            }
            _ => false,
        };
        if !use_detached {
            return complete(
                send,
                request_id,
                rollback_application_unconfirmed(
                    saved_revision,
                    restored_revision,
                    "diagnostics did not accept restored logging apply",
                ),
            );
        }
        let detached_result = send(Command::Request {
            service: DIAGNOSTICS_SERVICE.into(),
            payload: json!({"action":"apply_host_logging","configuration":restored_configuration}),
            timeout_ms: MAX_REQUEST_MS,
            operation_id: format!("host-configuration.{request_id}.restore-logging-detached"),
            parent: None,
        });
        let Ok(Response::Accepted { id }) = detached_result else {
            return complete(
                send,
                request_id,
                rollback_application_unconfirmed(
                    saved_revision,
                    restored_revision,
                    "diagnostics did not accept detached restored logging apply",
                ),
            );
        };
        self.pending = Some(Pending {
            request_id,
            deadline: Instant::now() + Duration::from_millis(MAX_REQUEST_MS),
            state_request_id: id,
            stage: Stage::Compensating {
                saved_revision,
                restored_revision,
                parented: false,
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

fn identity_settings_changed(previous: &Document, next: &Document) -> bool {
    previous.file_state != next.file_state
        || previous.vibeshine_file_state != next.vibeshine_file_state
        || previous.credentials_file != next.credentials_file
        || previous.pkey != next.pkey
        || previous.cert != next.cert
}

fn submit_state(
    send: &mut impl FnMut(Command) -> TransportResult,
    parent: u64,
    deadline: Instant,
    stage: &str,
    payload: Value,
) -> ModuleResult<Option<u64>> {
    submit_child(send, parent, deadline, stage, crate::STATE_SERVICE, payload)
}

fn submit_child(
    send: &mut impl FnMut(Command) -> TransportResult,
    parent: u64,
    deadline: Instant,
    stage: &str,
    service: &str,
    payload: Value,
) -> ModuleResult<Option<u64>> {
    let remaining_ms = deadline
        .saturating_duration_since(Instant::now())
        .as_millis() as u64;
    if remaining_ms == 0 {
        return Ok(None);
    }
    match send(Command::Request {
        service: service.into(),
        payload,
        timeout_ms: remaining_ms.min(MAX_REQUEST_MS),
        operation_id: format!("host-configuration.{parent}.{stage}"),
        parent: Some(parent),
    })? {
        Response::Accepted { id } => Ok(Some(id)),
        _ => Ok(None),
    }
}

fn rollback_failed(saved_revision: u64, message: impl Into<String>) -> Value {
    json!({
        "ok":false,
        "error":{"code":"logging_rollback_failed","message":format!("{}; GET configuration and diagnostics status before retrying", message.into())},
        "logging":{"saved_revision":saved_revision,"applied_revision":null,"status":"rollback_failed"}
    })
}

fn rollback_unconfirmed(saved_revision: u64, message: impl Into<String>) -> Value {
    json!({
        "ok":false,
        "error":{"code":"logging_rollback_unconfirmed","message":format!("{}; GET configuration and diagnostics status before retrying", message.into())},
        "logging":{"saved_revision":saved_revision,"applied_revision":null,"status":"rollback_unconfirmed"}
    })
}

fn rollback_application_unconfirmed(
    saved_revision: u64,
    restored_revision: u64,
    message: impl Into<String>,
) -> Value {
    json!({
        "ok":false,
        "error":{"code":"logging_rollback_unconfirmed","message":format!("{}; GET configuration and diagnostics status before retrying", message.into())},
        "logging":{"saved_revision":saved_revision,"restored_revision":restored_revision,"applied_revision":null,"status":"rollback_application_unconfirmed"}
    })
}

fn valid_diagnostics_status(payload: &Value) -> bool {
    if payload.get("ok").and_then(Value::as_bool) != Some(true)
        || payload.get("ready").and_then(Value::as_bool).is_none()
    {
        return false;
    }
    match payload.get("logging_configuration_revision") {
        Some(Value::Null) => {}
        Some(Value::Number(revision)) if revision.as_u64().is_some_and(|value| value > 0) => {}
        _ => return false,
    }
    let state = payload
        .pointer("/logging_sink/state")
        .and_then(Value::as_str);
    let failure = payload.pointer("/logging_sink/failure");
    match (state, failure) {
        (Some("disabled" | "healthy"), Some(Value::Null)) => true,
        (Some("failed"), Some(Value::String(kind))) if kind == "open" || kind == "write" => true,
        _ => false,
    }
}

fn save_unconfirmed(attempted_revision: u64) -> Value {
    json!({
        "ok":false,
        "error":{"code":"save_unconfirmed","message":"configuration write outcome is unknown; GET configuration before retrying"},
        "logging":{"attempted_revision":attempted_revision,"saved_revision":null,"applied_revision":null,"status":"save_unconfirmed"}
    })
}

fn application_unconfirmed(saved_revision: u64) -> Value {
    json!({
        "ok":false,
        "error":{"code":"logging_application_unconfirmed","message":"configuration was saved but logging application is unconfirmed; GET configuration and diagnostics status before retrying"},
        "logging":{"saved_revision":saved_revision,"applied_revision":null,"status":"saved_application_unconfirmed"}
    })
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
            if let Some(transaction) = &document.transaction {
                let Some(state_revision) = payload.get("revision").and_then(Value::as_u64) else {
                    return Err(unavailable("state lookup omitted the transaction revision"));
                };
                if state_revision < transaction.expected_state_revision
                    || (transaction.settled
                        && state_revision == transaction.expected_state_revision)
                {
                    return Err(unavailable(
                        "configuration transaction revision is inconsistent",
                    ));
                }
            }
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

fn value_fits(value: &Value) -> bool {
    serde_json::to_vec(value).is_ok_and(|bytes| bytes.len() <= MAX_RESPONSE_BYTES)
}

#[cfg(test)]
mod tests;
