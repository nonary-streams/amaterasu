//! State-verified host configuration hold recovery.
use super::*;
use amaterasu_module_sdk::effect_authority::{
    ConfigurationTransactionState, HostConfigurationTransactionRequest,
    HostConfigurationTransactionStatus,
};

impl Pipeline {
    pub(super) fn progress_configuration_reconciliation(
        &mut self,
        send: &mut impl FnMut(Command) -> Transport,
    ) -> ModuleResult<()> {
        if self.configuration_reconciled {
            return Ok(());
        }
        let Some(owner) = self
            .effect_authority
            .as_ref()
            .and_then(|policy| policy.configuration_owner.clone())
        else {
            return Ok(());
        };
        let request = if let Some(request) = self.configuration_status_request {
            request
        } else {
            let mut nonce = [0; 16];
            getrandom::fill(&mut nonce)
                .map_err(|_| "configuration reconciliation entropy unavailable")?;
            let Response::Accepted { id } = send(Command::Request {
                service: HOST_CONFIGURATION_SERVICE.into(),
                payload: serde_json::to_value(
                    HostConfigurationTransactionRequest::TransactionStatus {},
                )
                .map_err(|_| "configuration status serialization failed")?,
                timeout_ms: HOST_CONFIGURATION_TIMEOUT_MS,
                operation_id: format!(
                    "pairing.configuration-status.{}",
                    u128::from_le_bytes(nonce)
                ),
                parent: None,
            })?
            else {
                return Ok(());
            };
            self.configuration_status_request = Some(id);
            return Ok(());
        };
        let result = send(Command::TakeResult { request })?;
        if matches!(result, Response::Pending) {
            return Ok(());
        }
        match send(Command::ReleaseResult { request })? {
            Response::Ok => {}
            Response::Error { code, .. } if code == "not_found" => {}
            _ => return Ok(()),
        }
        self.configuration_status_request = None;
        let Response::Result { payload } = result else {
            return Ok(());
        };
        let Ok(status) = serde_json::from_value::<HostConfigurationTransactionStatus>(payload)
        else {
            return Ok(());
        };
        if !status.valid() {
            return Ok(());
        }
        match status.transaction {
            None => self.configuration_reconciled = true,
            Some(transaction) => {
                self.configuration_hold = Some(ConfigurationHold {
                    owner,
                    change: transaction.change,
                });
                self.configuration_reconciled = true;
            }
        }
        Ok(())
    }

    pub(super) fn start_hold_confirmation(
        &mut self,
        request_id: u64,
        principal: EffectCaller<'_>,
        change: ConfigurationChange,
        outcome: Option<ConfigurationChangeOutcome>,
        current_epoch: Option<String>,
        send: &mut impl FnMut(Command) -> Transport,
    ) -> ModuleResult<()> {
        let EffectCaller { caller, initiator } = principal;
        let permitted = caller == initiator
            && self
                .effect_authority
                .as_ref()
                .and_then(|policy| policy.configuration_owner.as_deref())
                == Some(caller);
        if !permitted || !change.valid() {
            return complete(send, request_id, error("forbidden"));
        }
        let Some(gate) = &self.effect_gate else {
            return complete(send, request_id, error("authority_unavailable"));
        };
        let epoch = current_epoch.as_deref().unwrap_or(&change.authority_epoch);
        if gate.require_epoch(epoch).is_err() {
            return complete(send, request_id, error("stale_epoch"));
        }
        if let Some((owner, completed, completed_outcome)) = &self.completed_configuration_hold
            && owner == caller
            && completed == &change
            && outcome
                .as_ref()
                .is_none_or(|requested| requested == completed_outcome)
        {
            return complete(
                send,
                request_id,
                json!({"ok":true,"state":"released","operation_id":change.operation_id}),
            );
        }
        if self.configuration_hold.is_none() && current_epoch.is_some() {
            let gate = self.effect_gate.as_mut().ok_or("authority_unavailable")?;
            if let Err(code) = gate.fence_for_configuration_change() {
                return complete(send, request_id, error(code));
            }
            self.effect_bindings.clear();
            self.configuration_hold = Some(ConfigurationHold {
                owner: caller.into(),
                change: change.clone(),
            });
        }
        if self
            .configuration_hold
            .as_ref()
            .is_none_or(|hold| hold.owner != caller || hold.change != change)
        {
            return complete(send, request_id, error("configuration_change_unknown"));
        }
        if self.effect_gate.as_ref().is_none_or(|gate| !gate.drained())
            || self.hold_confirmation.is_some()
        {
            return complete(
                send,
                request_id,
                json!({"ok":true,"state":"pending","operation_id":change.operation_id}),
            );
        }
        let mut nonce = [0_u8; 16];
        getrandom::fill(&mut nonce)
            .map_err(|_| "configuration confirmation entropy unavailable")?;
        let Response::Accepted { id } = send(Command::Request {
            service: HOST_CONFIGURATION_SERVICE.into(),
            payload: serde_json::to_value(
                HostConfigurationTransactionRequest::TransactionStatus {},
            )
            .map_err(|_| "configuration status serialization failed")?,
            timeout_ms: HOST_CONFIGURATION_TIMEOUT_MS,
            operation_id: format!(
                "pairing.configuration-confirmation.{}",
                u128::from_le_bytes(nonce)
            ),
            parent: None,
        })?
        else {
            return complete(send, request_id, error("host_configuration_unavailable"));
        };
        self.hold_confirmation = Some(HoldConfirmation {
            request_id,
            change,
            outcome,
            status_request: Some(id),
            verified_revision: None,
        });
        Ok(())
    }

    pub(super) fn progress_hold_confirmation(
        &mut self,
        send: &mut impl FnMut(Command) -> Transport,
    ) -> ModuleResult<()> {
        let Some(mut confirmation) = self.hold_confirmation.take() else {
            return Ok(());
        };
        match send(Command::CheckRequest {
            request: confirmation.request_id,
        }) {
            Ok(Response::Ok) => {}
            Ok(Response::Error { code, .. }) if code == "request_ended" || code == "not_found" => {
                if let Some(request) = confirmation.status_request {
                    let _ = send(Command::Cancel { request });
                    let _ = send(Command::ReleaseResult { request });
                }
                return Ok(());
            }
            _ => {
                self.hold_confirmation = Some(confirmation);
                return Ok(());
            }
        }
        if let Some(request) = confirmation.status_request {
            let result = match send(Command::TakeResult { request }) {
                Ok(result) => result,
                Err(_) => {
                    self.hold_confirmation = Some(confirmation);
                    return Ok(());
                }
            };
            if matches!(result, Response::Pending) {
                self.hold_confirmation = Some(confirmation);
                return Ok(());
            }
            match send(Command::ReleaseResult { request }) {
                Ok(Response::Ok) => {}
                Ok(Response::Error { code, .. }) if code == "not_found" => {}
                _ => {
                    self.hold_confirmation = Some(confirmation);
                    return Ok(());
                }
            }
            confirmation.status_request = None;
            let Response::Result { payload } = result else {
                return complete(
                    send,
                    confirmation.request_id,
                    error("host_configuration_unavailable"),
                );
            };
            let Ok(status) = serde_json::from_value::<HostConfigurationTransactionStatus>(payload)
            else {
                return complete(
                    send,
                    confirmation.request_id,
                    error("host_configuration_unavailable"),
                );
            };
            if !status.valid() {
                return complete(
                    send,
                    confirmation.request_id,
                    error("host_configuration_unavailable"),
                );
            }
            let transaction = status
                .transaction
                .filter(|transaction| transaction.change == confirmation.change)
                .or_else(|| {
                    status
                        .terminal_receipt
                        .map(|receipt| receipt.transaction)
                        .filter(|transaction| transaction.change == confirmation.change)
                });
            let Some(transaction) = transaction else {
                return complete(
                    send,
                    confirmation.request_id,
                    json!({"ok":true,"state":"pending","operation_id":confirmation.change.operation_id}),
                );
            };
            let observed = match transaction.state {
                ConfigurationTransactionState::Committed {
                    configuration_revision,
                } => Some(ConfigurationChangeOutcome::Committed {
                    configuration_revision,
                }),
                ConfigurationTransactionState::Aborted { .. } => {
                    Some(ConfigurationChangeOutcome::Aborted {})
                }
                ConfigurationTransactionState::Prepared {} => None,
            };
            if !transaction.settled
                || transaction.change != confirmation.change
                || observed.is_none()
                || confirmation
                    .outcome
                    .as_ref()
                    .is_some_and(|requested| Some(requested) != observed.as_ref())
            {
                return complete(
                    send,
                    confirmation.request_id,
                    json!({"ok":true,"state":"pending","operation_id":confirmation.change.operation_id}),
                );
            }
            confirmation.verified_revision = Some(status.configuration_revision);
            confirmation.outcome = observed;
        }
        let expected = confirmation
            .verified_revision
            .map(|revision| (confirmation.change.expected_trust_revision, revision));
        if self.identity.revisions()? != expected
            || self.effect_gate.as_ref().is_none_or(|gate| !gate.drained())
        {
            if let HostConfigurationState::Ready { identity, .. } = &self.host_configuration {
                let configuration_revision = identity.provider_revision;
                self.invalidate_identity();
                self.recovery_needed = true;
                if Some(configuration_revision) != confirmation.verified_revision {
                    self.refresh_identity = None;
                    self.host_configuration = HostConfigurationState::Start;
                }
            }
            self.hold_confirmation = Some(confirmation);
            return Ok(());
        }
        if !matches!(
            send(Command::CheckRequest {
                request: confirmation.request_id
            })?,
            Response::Ok
        ) {
            return Ok(());
        }
        let Some(hold) = self.configuration_hold.take() else {
            return complete(
                send,
                confirmation.request_id,
                error("configuration_change_unknown"),
            );
        };
        self.completed_configuration_hold = Some((
            hold.owner,
            hold.change,
            confirmation.outcome.expect("verified outcome"),
        ));
        complete(
            send,
            confirmation.request_id,
            json!({"ok":true,"state":"released","operation_id":confirmation.change.operation_id}),
        )
    }
}
