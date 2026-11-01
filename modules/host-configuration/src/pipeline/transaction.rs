//! Durable identity-change transaction operations.
use super::*;
use amaterasu_module_sdk::effect_authority::{
    ConfigurationChangeOutcome, Request as EffectRequest,
};

fn effect_payload(request: EffectRequest) -> ModuleResult<Value> {
    serde_json::to_value(request)
        .map_err(|error| format!("effect request serialization failed: {error}"))
}

fn state_set(document: &Document, expected_revision: u64) -> ModuleResult<Value> {
    let value = serde_json::to_value(document)
        .map_err(|error| format!("transaction document serialization failed: {error}"))?;
    Ok(amaterasu_module_sdk::state::StateRequest::Set {
        key: STATE_KEY.into(),
        value,
        expected_revision: Some(expected_revision),
    }
    .payload())
}

pub(super) fn state_mutation_revision(payload: &Value) -> Option<u64> {
    amaterasu_module_sdk::state::decode_response::<amaterasu_module_sdk::state::StateMutation>(
        payload.clone(),
    )
    .ok()?
    .ok()
    .map(|mutation| mutation.revision)
}

impl Pipeline {
    pub(super) fn select_authority(
        &mut self,
        update: UpdateToSave,
        send: &mut impl FnMut(Command) -> TransportResult,
    ) -> ModuleResult<()> {
        let Some(child) = self.submit_detached(
            amaterasu_module_sdk::effect_authority::SERVICE,
            effect_payload(EffectRequest::Status {})?,
            send,
        )?
        else {
            return complete(
                send,
                update.request_id,
                unavailable("effect authority is unavailable"),
            );
        };
        self.pending = Some(Pending {
            request_id: update.request_id,
            deadline: update.deadline,
            state_request_id: child,
            stage: Stage::SelectingAuthority(update),
            phase: PendingPhase::Waiting,
        });
        Ok(())
    }

    pub(super) fn prepare_hold(
        &mut self,
        update: UpdateToSave,
        change: ConfigurationChange,
        send: &mut impl FnMut(Command) -> TransportResult,
    ) -> ModuleResult<()> {
        if !matches!(
            send(Command::CheckRequest {
                request: update.request_id
            })?,
            Response::Ok
        ) {
            return complete(
                send,
                update.request_id,
                error("request_ended", "configuration request ended"),
            );
        }
        let Some(expected_state_revision) = update.state_revision.checked_add(1) else {
            return complete(
                send,
                update.request_id,
                error("revision_exhausted", "state revision is exhausted"),
            );
        };
        if update.state_revision.checked_add(5).is_none() {
            return complete(
                send,
                update.request_id,
                error(
                    "revision_exhausted",
                    "state revision cannot support transaction recovery",
                ),
            );
        }
        let mut previous = update.previous.clone();
        previous.transaction = Some(ConfigurationTransaction {
            change: change.clone(),
            state: ConfigurationTransactionState::Prepared {},
            expected_state_revision,
            settled: false,
        });
        let mut committed = update.document.clone();
        committed.transaction = Some(ConfigurationTransaction {
            change: change.clone(),
            state: ConfigurationTransactionState::Committed {
                configuration_revision: committed.revision,
            },
            expected_state_revision,
            settled: true,
        });
        let mut rolled_back = previous.clone();
        let Some(rollback_revision) = committed.revision.checked_add(1) else {
            return complete(
                send,
                update.request_id,
                error("revision_exhausted", "rollback revision is exhausted"),
            );
        };
        rolled_back.revision = rollback_revision;
        if let Some(transaction) = &mut rolled_back.transaction {
            transaction.state = ConfigurationTransactionState::Aborted {
                configuration_revision: rolled_back.revision,
            };
            transaction.settled = true;
        }
        let mut terminal = committed.clone();
        let transaction = terminal
            .transaction
            .take()
            .expect("constructed transaction");
        terminal.terminal_receipt = Some(crate::settings::ConfigurationTerminalReceipt {
            transaction,
            terminal_state_revision: expected_state_revision + 3,
            identity_fingerprint: terminal.identity_fingerprint()?,
        });
        let mut terminal_rollback = rolled_back.clone();
        let transaction = terminal_rollback
            .transaction
            .take()
            .expect("constructed transaction");
        terminal_rollback.terminal_receipt = Some(crate::settings::ConfigurationTerminalReceipt {
            transaction,
            terminal_state_revision: expected_state_revision + 4,
            identity_fingerprint: terminal_rollback.identity_fingerprint()?,
        });
        if !document_fits(&previous)
            || !document_fits(&committed)
            || !document_fits(&rolled_back)
            || !document_fits(&terminal)
            || !document_fits(&terminal_rollback)
        {
            return complete(
                send,
                update.request_id,
                error(
                    "resource_exhausted",
                    "transaction marker exceeds the state limit",
                ),
            );
        }
        let payload = state_set(&previous, update.state_revision)?;
        let Some(child) = self.submit_detached(crate::STATE_SERVICE, payload, send)? else {
            return complete(
                send,
                update.request_id,
                error(
                    "configuration_transaction_unconfirmed",
                    "transaction marker admission was not confirmed",
                ),
            );
        };
        self.pending = Some(Pending {
            request_id: update.request_id,
            deadline: update.deadline,
            state_request_id: child,
            stage: Stage::PreparingHold { update, change },
            phase: PendingPhase::Waiting,
        });
        Ok(())
    }

    pub(super) fn begin_hold(
        &mut self,
        update: UpdateToSave,
        change: ConfigurationChange,
        send: &mut impl FnMut(Command) -> TransportResult,
    ) -> ModuleResult<()> {
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
                    "request ended before the effect hold was confirmed",
                ),
            );
        }
        let Some(child) = self.submit_detached(
            amaterasu_module_sdk::effect_authority::SERVICE,
            effect_payload(EffectRequest::BeginConfigurationChange {
                change: change.clone(),
            })?,
            send,
        )?
        else {
            return complete(
                send,
                update.request_id,
                error(
                    "configuration_transaction_unconfirmed",
                    "effect hold admission was not confirmed",
                ),
            );
        };
        self.pending = Some(Pending {
            request_id: update.request_id,
            deadline: update.deadline,
            state_request_id: child,
            stage: Stage::BeginningHold { update, change },
            phase: PendingPhase::Waiting,
        });
        Ok(())
    }

    pub(super) fn submit_detached(
        &mut self,
        service: &str,
        payload: Value,
        send: &mut impl FnMut(Command) -> TransportResult,
    ) -> ModuleResult<Option<u64>> {
        let Some(sequence) = self.detached_sequence.checked_add(1) else {
            return Ok(None);
        };
        self.detached_sequence = sequence;
        let Some(epoch) = &self.transaction_epoch else {
            return Ok(None);
        };
        match send(Command::Request {
            service: service.into(),
            payload,
            timeout_ms: MAX_REQUEST_MS,
            operation_id: format!("host-configuration.transaction.{epoch}.{sequence}"),
            parent: None,
        })? {
            Response::Accepted { id } => Ok(Some(id)),
            _ => Ok(None),
        }
    }

    pub(super) fn confirm_hold(
        &mut self,
        request_id: u64,
        deadline: Instant,
        expected_state_revision: u64,
        previous: Document,
        response: Value,
        send: &mut impl FnMut(Command) -> TransportResult,
    ) -> ModuleResult<()> {
        if !matches!(
            send(Command::CheckRequest {
                request: request_id
            })?,
            Response::Ok
        ) {
            return complete(
                send,
                request_id,
                error(
                    "configuration_transaction_unconfirmed",
                    "request ended before the commit fence",
                ),
            );
        }
        let Some(revision) = response["revision"].as_u64() else {
            return complete(
                send,
                request_id,
                error(
                    "configuration_transaction_unconfirmed",
                    "saved configuration revision is missing",
                ),
            );
        };
        let Ok(settings) = serde_json::from_value(response["values"].clone()) else {
            return complete(
                send,
                request_id,
                error(
                    "configuration_transaction_unconfirmed",
                    "saved configuration settings are invalid",
                ),
            );
        };
        let Some(mut transaction) = previous.transaction else {
            return complete(
                send,
                request_id,
                error(
                    "configuration_transaction_unconfirmed",
                    "transaction marker is missing",
                ),
            );
        };
        transaction.state = ConfigurationTransactionState::Committed {
            configuration_revision: revision,
        };
        transaction.settled = true;
        let mut document = Document::from_settings(revision, settings);
        document.transaction = Some(transaction);
        let payload = state_set(&document, expected_state_revision)?;
        let Some(child) = self.submit_detached(crate::STATE_SERVICE, payload, send)? else {
            return complete(
                send,
                request_id,
                error(
                    "configuration_transaction_unconfirmed",
                    "commit fence admission was not confirmed",
                ),
            );
        };
        self.pending = Some(Pending {
            request_id,
            deadline,
            state_request_id: child,
            stage: Stage::ConfirmingHold {
                response,
                document,
                expected_state_revision,
            },
            phase: PendingPhase::Waiting,
        });
        Ok(())
    }

    pub(super) fn finish_hold(
        &mut self,
        completion: HoldCompletion,
        send: &mut impl FnMut(Command) -> TransportResult,
    ) -> ModuleResult<()> {
        let HoldCompletion {
            request_id,
            deadline,
            expected_state_revision,
            document,
            response,
            current_authority_epoch,
            reconcile_change,
        } = completion;
        if !matches!(
            send(Command::CheckRequest {
                request: request_id
            })?,
            Response::Ok
        ) {
            return complete(
                send,
                request_id,
                error(
                    "configuration_transaction_unconfirmed",
                    "request ended before the effect hold was released",
                ),
            );
        }
        let transaction = document
            .transaction
            .as_ref()
            .filter(|transaction| {
                reconcile_change
                    .as_ref()
                    .is_none_or(|change| change == &transaction.change)
            })
            .or_else(|| {
                document
                    .terminal_receipt
                    .as_ref()
                    .map(|receipt| &receipt.transaction)
                    .filter(|transaction| {
                        reconcile_change
                            .as_ref()
                            .is_some_and(|change| change == &transaction.change)
                    })
            });
        let Some(transaction) = transaction else {
            return complete(
                send,
                request_id,
                error(
                    "configuration_transaction_unconfirmed",
                    "transaction marker is missing",
                ),
            );
        };
        let payload = if let Some(epoch) = &current_authority_epoch {
            effect_payload(EffectRequest::ReconcileConfigurationChange {
                change: transaction.change.clone(),
                current_authority_epoch: epoch.clone(),
            })?
        } else {
            effect_payload(EffectRequest::FinishConfigurationChange {
                change: transaction.change.clone(),
                result: ConfigurationChangeOutcome::Committed {
                    configuration_revision: document.revision,
                },
            })?
        };
        let Some(child) = self.submit_detached(
            amaterasu_module_sdk::effect_authority::SERVICE,
            payload,
            send,
        )?
        else {
            return complete(
                send,
                request_id,
                error(
                    "configuration_transaction_unconfirmed",
                    "effect hold completion was not admitted",
                ),
            );
        };
        self.pending = Some(Pending {
            request_id,
            deadline,
            state_request_id: child,
            stage: Stage::FinishingHold {
                response,
                document,
                expected_state_revision,
                current_authority_epoch,
                reconcile_change,
            },
            phase: PendingPhase::Waiting,
        });
        Ok(())
    }

    pub(super) fn clear_hold(
        &mut self,
        request_id: u64,
        deadline: Instant,
        expected_state_revision: u64,
        mut document: Document,
        response: Value,
        send: &mut impl FnMut(Command) -> TransportResult,
    ) -> ModuleResult<()> {
        if !matches!(
            send(Command::CheckRequest {
                request: request_id
            })?,
            Response::Ok
        ) {
            return complete(
                send,
                request_id,
                error(
                    "configuration_transaction_unconfirmed",
                    "request ended before transaction cleanup",
                ),
            );
        }
        let Some(transaction) = document.transaction.take() else {
            return complete(
                send,
                request_id,
                error(
                    "configuration_transaction_unconfirmed",
                    "transaction marker is missing",
                ),
            );
        };
        let Some(terminal_state_revision) = expected_state_revision.checked_add(1) else {
            return complete(
                send,
                request_id,
                error(
                    "revision_exhausted",
                    "terminal receipt revision is exhausted",
                ),
            );
        };
        let identity_fingerprint = document.identity_fingerprint()?;
        let receipt = crate::settings::ConfigurationTerminalReceipt {
            transaction,
            terminal_state_revision,
            identity_fingerprint,
        };
        if !receipt.valid_for(
            document.revision,
            terminal_state_revision,
            &document.identity_fingerprint()?,
        ) {
            return complete(
                send,
                request_id,
                error(
                    "configuration_transaction_unconfirmed",
                    "terminal receipt is invalid",
                ),
            );
        }
        document.terminal_receipt = Some(receipt);
        if !document_fits(&document) {
            return complete(
                send,
                request_id,
                error(
                    "resource_exhausted",
                    "terminal receipt exceeds the state limit",
                ),
            );
        }
        let payload = state_set(&document, expected_state_revision)?;
        let Some(child) = self.submit_detached(crate::STATE_SERVICE, payload, send)? else {
            return complete(
                send,
                request_id,
                error(
                    "configuration_transaction_unconfirmed",
                    "transaction marker removal was not admitted",
                ),
            );
        };
        self.pending = Some(Pending {
            request_id,
            deadline,
            state_request_id: child,
            stage: Stage::ClearingHold {
                response,
                expected_state_revision,
            },
            phase: PendingPhase::Waiting,
        });
        Ok(())
    }

    pub(super) fn reconcile_transaction(
        &mut self,
        request_id: u64,
        deadline: Instant,
        state_revision: u64,
        mut document: Document,
        change: ConfigurationChange,
        send: &mut impl FnMut(Command) -> TransportResult,
    ) -> ModuleResult<()> {
        if !matches!(
            send(Command::CheckRequest {
                request: request_id
            })?,
            Response::Ok
        ) {
            return complete(
                send,
                request_id,
                error("request_ended", "reconciliation request ended"),
            );
        }
        let active_matches = document
            .transaction
            .as_ref()
            .is_some_and(|transaction| transaction.change == change);
        if !active_matches
            && document
                .terminal_receipt
                .as_ref()
                .is_some_and(|receipt| receipt.transaction.change == change)
        {
            return self.select_reconcile_authority(
                request_id,
                deadline,
                state_revision,
                document,
                change,
                send,
            );
        }
        let Some(transaction) = &mut document.transaction else {
            return complete(
                send,
                request_id,
                error(
                    "configuration_change_unknown",
                    "transaction marker is missing",
                ),
            );
        };
        if !change.valid()
            || transaction.change != change
            || state_revision < transaction.expected_state_revision
        {
            return complete(
                send,
                request_id,
                error(
                    "configuration_change_unknown",
                    "transaction marker does not match",
                ),
            );
        }
        if transaction.settled {
            return self.select_reconcile_authority(
                request_id,
                deadline,
                state_revision,
                document,
                change,
                send,
            );
        }
        if matches!(
            transaction.state,
            ConfigurationTransactionState::Prepared {}
        ) {
            transaction.state = ConfigurationTransactionState::Aborted {
                configuration_revision: document.revision,
            };
        }
        transaction.settled = true;
        let payload = state_set(&document, state_revision)?;
        let Some(child) = self.submit_detached(crate::STATE_SERVICE, payload, send)? else {
            return complete(
                send,
                request_id,
                error(
                    "configuration_transaction_unconfirmed",
                    "reconciliation CAS admission was not confirmed",
                ),
            );
        };
        self.pending = Some(Pending {
            request_id,
            deadline,
            state_request_id: child,
            stage: Stage::ReconcilingHold {
                document,
                expected_state_revision: state_revision,
            },
            phase: PendingPhase::Waiting,
        });
        Ok(())
    }

    pub(super) fn select_reconcile_authority(
        &mut self,
        request_id: u64,
        deadline: Instant,
        state_revision: u64,
        document: Document,
        change: ConfigurationChange,
        send: &mut impl FnMut(Command) -> TransportResult,
    ) -> ModuleResult<()> {
        let Some(child) = self.submit_detached(
            amaterasu_module_sdk::effect_authority::SERVICE,
            effect_payload(EffectRequest::Status {})?,
            send,
        )?
        else {
            return complete(
                send,
                request_id,
                error(
                    "configuration_transaction_unconfirmed",
                    "effect authority is unavailable",
                ),
            );
        };
        self.pending = Some(Pending {
            request_id,
            deadline,
            state_request_id: child,
            stage: Stage::SelectingReconcile {
                document,
                state_revision,
                change,
            },
            phase: PendingPhase::Waiting,
        });
        Ok(())
    }
}
