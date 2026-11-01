//! Startup inventory and broker fencing for unresolved protocol sources.
//! A stable read is an observation, not proof of worker retirement, effect
//! cleanup, or permission to recover or reuse a slot.
use super::{STATE_SERVICE, Transport};
use amaterasu_module_sdk::{
    Command, ModuleResult, Response,
    broker_effect_fence::{
        EffectFenceCommand, EffectScope, ProviderIncarnation, SOURCE_SLOT_COUNT, SourceSlotLease,
    },
    protocol_slot_state::{self, RecoverySnapshot},
    protocol_state::fresh_state_operation_id,
};
use serde_json::Value;

/// Bounded evidence retained for recovery orchestration. A source visible only
/// in the broker is never discarded merely because State has no matching key.
pub(super) struct RecoveryInventory {
    pub(super) broker_epoch: String,
    pub(super) broker_ready: bool,
    /// Individual status reads do not freeze concurrent old authority work.
    /// Only a future broker-owned atomic freeze/snapshot may set this true.
    pub(super) broker_frozen: bool,
    pub(super) broker_sources: Vec<(SourceSlotLease, EffectScope)>,
    pub(super) state: RecoverySnapshot,
}

enum Phase {
    Broker(u8),
    State(u8),
    Reading(u8, u64),
    Done,
}
pub(super) enum Outcome {
    Pending,
    Observed(RecoveryInventory),
    Unavailable,
}
pub(super) struct RecoveryScan {
    phase: Phase,
    epoch: Option<String>,
    ready: Option<bool>,
    sources: Vec<(SourceSlotLease, EffectScope)>,
    state_instance: Option<ProviderIncarnation>,
    responses: Vec<(u8, Value)>,
}
impl RecoveryScan {
    pub(super) fn new() -> Self {
        Self {
            phase: Phase::Broker(0),
            epoch: None,
            ready: None,
            sources: Vec::new(),
            state_instance: None,
            responses: Vec::new(),
        }
    }
    pub(super) fn needs_state(&self) -> bool {
        matches!(self.phase, Phase::State(_) | Phase::Reading(_, _))
    }
    pub(super) fn state_instance(&self) -> Option<&ProviderIncarnation> {
        self.state_instance.as_ref()
    }
    pub(super) fn bind_state(&mut self, instance: ProviderIncarnation) {
        if self.state_instance.is_none() {
            self.state_instance = Some(instance);
        }
    }
    pub(super) fn advance(
        &mut self,
        state_current: bool,
        mut send: impl FnMut(Command) -> Transport,
    ) -> ModuleResult<Outcome> {
        let phase = std::mem::replace(&mut self.phase, Phase::Done);
        match phase {
            Phase::Broker(index) => {
                let Ok(Response::Result { payload }) = send(Command::EffectFence {
                    command: Box::new(EffectFenceCommand::SlotStatus { slot: index }),
                }) else {
                    return Ok(Outcome::Unavailable);
                };
                let Some(epoch) = payload["broker_epoch"].as_str().filter(|value| {
                    !value.is_empty() && value.len() <= 128 && !value.chars().any(char::is_control)
                }) else {
                    return Ok(Outcome::Unavailable);
                };
                let Some(ready) = payload["ready"].as_bool() else {
                    return Ok(Outcome::Unavailable);
                };
                if self
                    .epoch
                    .as_deref()
                    .is_some_and(|previous| previous != epoch)
                    || self.ready.is_some_and(|previous| previous != ready)
                {
                    return Ok(Outcome::Unavailable);
                }
                self.epoch = Some(epoch.to_owned());
                self.ready = Some(ready);
                if !ready {
                    // A fresh broker has no current slot inventory. This says
                    // nothing about the prior broker's workers or effects.
                    if !payload["pending_retirements"].is_array() {
                        return Ok(Outcome::Unavailable);
                    }
                    self.phase = Phase::State(0);
                    return Ok(Outcome::Pending);
                }
                let Ok(lease) = serde_json::from_value::<SourceSlotLease>(payload["lease"].clone())
                else {
                    return Ok(Outcome::Unavailable);
                };
                if lease.slot != index
                    || lease.broker_epoch != epoch
                    || lease.incarnation == 0
                    || lease.token.is_empty()
                    || lease.token.len() > 128
                    || lease.token.chars().any(char::is_control)
                    || payload.get("active").is_none()
                {
                    return Ok(Outcome::Unavailable);
                }
                if !payload["active"].is_null() {
                    let Ok(scope) =
                        serde_json::from_value::<EffectScope>(payload["active"].clone())
                    else {
                        return Ok(Outcome::Unavailable);
                    };
                    let valid_source = serde_json::to_value(&scope.source)
                        .ok()
                        .and_then(|value| {
                            serde_json::from_value::<
                                    amaterasu_module_sdk::effect_authority::Source,
                                >(value)
                                .ok()
                        })
                        .is_some_and(|source| source.valid());
                    if !valid_source
                        || scope.source.slot() != Some(index)
                        || scope.authority_epoch.is_empty()
                        || scope.issuer_epoch.is_empty()
                        || scope.authority_epoch.len() > 128
                        || scope.issuer_epoch.len() > 128
                        || scope.authority_epoch.chars().any(char::is_control)
                        || scope.issuer_epoch.chars().any(char::is_control)
                    {
                        return Ok(Outcome::Unavailable);
                    }
                    // Close the broker-owned exact lease immediately. A failed
                    // or lost response is not a retirement acknowledgment.
                    if !matches!(
                        send(Command::EffectFence {
                            command: Box::new(EffectFenceCommand::BeginCloseSlot {
                                lease: lease.clone(),
                                scope: scope.clone()
                            })
                        }),
                        Ok(Response::Ok)
                    ) {
                        return Ok(Outcome::Unavailable);
                    }
                    self.sources.push((lease, scope));
                }
                self.phase = if usize::from(index) + 1 == SOURCE_SLOT_COUNT {
                    Phase::State(0)
                } else {
                    Phase::Broker(index + 1)
                };
                Ok(Outcome::Pending)
            }
            Phase::State(index) => {
                if !state_current || self.state_instance.is_none() {
                    return Ok(Outcome::Unavailable);
                }
                let read = protocol_slot_state::read_request(index).map_err(str::to_owned)?;
                match send(Command::Request {
                    service: STATE_SERVICE.into(),
                    payload: read.payload(),
                    timeout_ms: 5_000,
                    operation_id: fresh_state_operation_id().map_err(str::to_owned)?,
                    parent: None,
                }) {
                    Ok(Response::Accepted { id }) => {
                        self.phase = Phase::Reading(index, id);
                        Ok(Outcome::Pending)
                    }
                    _ => Ok(Outcome::Unavailable),
                }
            }
            Phase::Reading(index, id) => {
                if !state_current {
                    let _ = send(Command::Cancel { request: id });
                    let _ = send(Command::ReleaseResult { request: id });
                    return Ok(Outcome::Unavailable);
                }
                match send(Command::TakeResult { request: id }) {
                    Ok(Response::Pending) => {
                        self.phase = Phase::Reading(index, id);
                        Ok(Outcome::Pending)
                    }
                    Ok(Response::Result { payload }) => {
                        if !matches!(
                            send(Command::ReleaseResult { request: id }),
                            Ok(Response::Ok)
                        ) || protocol_slot_state::read(index, &payload).is_err()
                        {
                            return Ok(Outcome::Unavailable);
                        }
                        self.responses.push((index, payload));
                        if usize::from(index) + 1 < SOURCE_SLOT_COUNT {
                            self.phase = Phase::State(index + 1);
                            return Ok(Outcome::Pending);
                        }
                        let Ok(state) = protocol_slot_state::recovery_snapshot(&self.responses)
                        else {
                            return Ok(Outcome::Unavailable);
                        };
                        let (Some(epoch), Some(ready)) = (self.epoch.take(), self.ready) else {
                            return Ok(Outcome::Unavailable);
                        };
                        // Check the broker epoch again; a restarted broker is a
                        // different authority even if slot values look alike.
                        let Ok(Response::Result { payload }) = send(Command::EffectFence {
                            command: Box::new(EffectFenceCommand::SlotStatus { slot: 0 }),
                        }) else {
                            return Ok(Outcome::Unavailable);
                        };
                        if payload["broker_epoch"] != epoch || payload["ready"] != ready {
                            return Ok(Outcome::Unavailable);
                        }
                        Ok(Outcome::Observed(RecoveryInventory {
                            broker_epoch: epoch,
                            broker_ready: ready,
                            broker_frozen: false,
                            broker_sources: std::mem::take(&mut self.sources),
                            state,
                        }))
                    }
                    _ => {
                        let _ = send(Command::ReleaseResult { request: id });
                        Ok(Outcome::Unavailable)
                    }
                }
            }
            Phase::Done => Ok(Outcome::Unavailable),
        }
    }
}

impl RecoveryInventory {
    fn unresolved(&self) -> bool {
        !self.broker_frozen
            || !self.broker_ready
            || !self.broker_sources.is_empty()
            || !self.state.active.is_empty()
            || !self.state.pending_close.is_empty()
    }
    pub(super) fn refence(&self, send: &mut impl FnMut(Command) -> Transport) {
        if !self.broker_ready {
            return;
        }
        for (lease, scope) in &self.broker_sources {
            if lease.broker_epoch == self.broker_epoch {
                let _ = send(Command::EffectFence {
                    command: Box::new(EffectFenceCommand::BeginCloseSlot {
                        lease: lease.clone(),
                        scope: scope.clone(),
                    }),
                });
            }
        }
    }
}

impl super::Pipeline {
    pub(super) fn progress_protocol_recovery(
        &mut self,
        send: &mut impl FnMut(Command) -> Transport,
    ) {
        if !self.protocol_recovery_auto
            || !(self.protocol_claim_recovery_required || self.retained_protocol_claim.is_some())
            || !matches!(
                self.host_configuration,
                super::HostConfigurationState::Disabled
                    | super::HostConfigurationState::Ready { .. }
            )
            || (self.retained_protocol_claim.is_none()
                && self
                    .effect_authority
                    .as_ref()
                    .is_none_or(|policy| policy.protocol_policies.is_empty()))
        {
            return;
        }
        if self.protocol_recovery_scan.is_none() {
            if self
                .protocol_recovery_retry_at
                .is_some_and(|deadline| std::time::Instant::now() < deadline)
            {
                return;
            }
            self.protocol_recovery_scan = Some(RecoveryScan::new());
        }
        let mut scan = self
            .protocol_recovery_scan
            .take()
            .expect("scan just initialized");
        let state_current = if scan.needs_state() {
            let current = self.selected_protocol_state(send);
            if scan.state_instance().is_none()
                && let Some(instance) = current.clone()
            {
                scan.bind_state(instance);
            }
            current
                .as_ref()
                .is_some_and(|instance| scan.state_instance() == Some(instance))
        } else {
            true
        };
        match scan.advance(state_current, &mut *send) {
            Ok(Outcome::Pending) => self.protocol_recovery_scan = Some(scan),
            Ok(Outcome::Observed(inventory)) => {
                if inventory.unresolved() {
                    self.invalidate_identity();
                    self.recovery_needed = true;
                }
                self.protocol_recovery_inventory = Some(inventory);
                // A completed inventory cannot establish worker retirement or
                // cleanup. Keep scanning while the latch remains closed.
                self.protocol_recovery_retry_at =
                    Some(std::time::Instant::now() + std::time::Duration::from_secs(1));
            }
            Ok(Outcome::Unavailable) | Err(_) => {
                self.protocol_recovery_retry_at =
                    Some(std::time::Instant::now() + std::time::Duration::from_secs(1));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn missing_active_field_is_not_treated_as_an_empty_slot() {
        let mut scan = RecoveryScan::new();
        let outcome = scan.advance(true, |command| match command {
            Command::EffectFence { command } if matches!(*command, EffectFenceCommand::SlotStatus { slot:0 }) => Ok(Response::Result { payload:json!({"broker_epoch":"epoch","ready":true,"lease":{"slot":0,"incarnation":1,"broker_epoch":"epoch","token":"token"}}) }),
            _ => panic!("unexpected command"),
        }).unwrap();
        assert!(matches!(outcome, Outcome::Unavailable));
    }

    #[test]
    fn mixed_state_revisions_never_produce_an_inventory() {
        let mut scan = RecoveryScan::new();
        let mut last_index = 0_u8;
        let mut observed = false;
        for _ in 0..200 {
            if scan.needs_state() && scan.state_instance().is_none() {
                scan.bind_state(ProviderIncarnation {
                    version: 1,
                    id: "a".repeat(32),
                });
            }
            let result = scan.advance(true, |command| match command {
                Command::EffectFence { command } if matches!(*command, EffectFenceCommand::SlotStatus { slot:0 }) => Ok(Response::Result { payload:json!({"broker_epoch":"epoch","ready":false,"pending_retirements":["host"]}) }),
                Command::Request { payload, .. } => {
                    last_index = payload["key"].as_str().unwrap().rsplit('/').next().unwrap().parse().unwrap();
                    Ok(Response::Accepted { id:u64::from(last_index)+1 })
                }
                Command::TakeResult { .. } => Ok(Response::Result { payload:json!({"ok":true,"revision":if last_index < 32 {7} else {8},"found":false,"value":null}) }),
                Command::ReleaseResult { .. } => Ok(Response::Ok),
                _ => panic!("unexpected recovery command"),
            }).unwrap();
            match result {
                Outcome::Pending => {}
                Outcome::Observed(_) => {
                    observed = true;
                    break;
                }
                Outcome::Unavailable => break,
            }
        }
        assert!(!observed);
        assert_eq!(last_index, 63);
    }
}
