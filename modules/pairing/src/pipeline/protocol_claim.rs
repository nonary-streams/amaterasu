//! Pairing-owned asynchronous durable reservation of one broker source slot.
//! No effect is armed here. An uncertain State write fences further authority
//! until the selected owner reconciles it; cancellation is never an undo.
use super::{STATE_SERVICE, Transport};
use amaterasu_module_sdk::{
    Command, ModuleResult, Response,
    broker_effect_fence::{EffectFenceCommand, EffectScope, SourceSlotLease},
    protocol_slot_state::{self, PendingWrite},
    protocol_state::{DurableStateBinding, fresh_state_operation_id},
};
use serde_json::Value;

enum Phase {
    Start,
    Reading {
        request: u64,
        lease: SourceSlotLease,
    },
    Writing {
        request: u64,
        lease: SourceSlotLease,
        write: PendingWrite,
    },
    Reconciling {
        request: u64,
        lease: SourceSlotLease,
    },
    Done,
}

pub(super) enum Outcome {
    Pending,
    /// The exact claim is durable. It is not an open slot or an effect grant.
    Claimed {
        lease: SourceSlotLease,
        scope: Box<EffectScope>,
    },
    /// An uncertain write or lost authority keeps the generic route fenced.
    Failed(&'static str),
}

pub(super) struct ClaimJob {
    parent: u64,
    scope: EffectScope,
    issuer: String,
    _state: DurableStateBinding,
    phase: Phase,
}
impl ClaimJob {
    pub(super) fn parent(&self) -> u64 {
        self.parent
    }
    pub(super) fn scope(&self) -> &EffectScope {
        &self.scope
    }
    pub(super) fn issuer(&self) -> &str {
        &self.issuer
    }
    /// `state` is selected by trusted composition and must actually implement
    /// durable CAS. The issuer is the selected broker principal, not client data.
    pub(super) fn new(
        parent: u64,
        scope: EffectScope,
        issuer: String,
        state: DurableStateBinding,
    ) -> Result<Self, &'static str> {
        if parent == 0
            || scope.source.slot().is_none()
            || !amaterasu_module_sdk::effect_authority::ProtocolCapability(issuer.clone()).valid()
        {
            return Err("invalid_protocol_claim");
        }
        Ok(Self {
            parent,
            scope,
            issuer,
            _state: state,
            phase: Phase::Start,
        })
    }

    /// `current` must be recomputed from pairing's active source, snapshot,
    /// issuer and selected policy at every call, including after State replies.
    pub(super) fn advance(
        &mut self,
        current: bool,
        mut send: impl FnMut(Command) -> Transport,
    ) -> ModuleResult<Outcome> {
        let active = current
            && matches!(
                send(Command::CheckRequest {
                    request: self.parent
                })?,
                Response::Ok
            );
        let phase = std::mem::replace(&mut self.phase, Phase::Done);
        if !active {
            let uncertain = matches!(&phase, Phase::Writing { .. } | Phase::Reconciling { .. });
            if let Phase::Reading { request, .. }
            | Phase::Writing { request, .. }
            | Phase::Reconciling { request, .. } = phase
            {
                let _ = send(Command::Cancel { request });
                let _ = send(Command::ReleaseResult { request });
            }
            return Ok(Outcome::Failed(if uncertain {
                "protocol_claim_uncertain"
            } else {
                "source_denied"
            }));
        }
        match phase {
            Phase::Start => {
                let slot = self.scope.source.slot().ok_or("invalid_source")?;
                let Response::Result { payload } = send(Command::EffectFence {
                    command: Box::new(EffectFenceCommand::SlotStatus { slot }),
                })?
                else {
                    return Ok(Outcome::Failed("protocol_authority_unavailable"));
                };
                if payload["ready"] != true || !payload.get("active").is_some_and(Value::is_null) {
                    return Ok(Outcome::Failed("protocol_reconciliation_required"));
                }
                let lease: SourceSlotLease = serde_json::from_value(payload["lease"].clone())
                    .map_err(|_| "invalid_slot_status")?;
                if lease.slot != slot || payload["broker_epoch"] != lease.broker_epoch {
                    return Ok(Outcome::Failed("invalid_slot_status"));
                }
                let request = self.submit_read(slot, &mut send)?;
                self.phase = Phase::Reading { request, lease };
                Ok(Outcome::Pending)
            }
            Phase::Reading { request, lease } => match send(Command::TakeResult { request })? {
                Response::Pending => {
                    self.phase = Phase::Reading { request, lease };
                    Ok(Outcome::Pending)
                }
                Response::Result { payload } => {
                    if !released(send(Command::ReleaseResult { request })) {
                        return Ok(Outcome::Failed("state_unavailable"));
                    }
                    let write = match protocol_slot_state::prepare_claim(
                        &payload,
                        lease.clone(),
                        self.scope.clone(),
                        self.issuer.clone(),
                    ) {
                        Ok(write) => write,
                        Err(_) => return Ok(Outcome::Failed("protocol_reconciliation_required")),
                    };
                    let response = send(Command::Request {
                        service: STATE_SERVICE.into(),
                        payload: write.request().payload(),
                        timeout_ms: 5_000,
                        operation_id: write.state_operation_id().into(),
                        parent: None,
                    });
                    let id = match response {
                        Ok(Response::Accepted { id }) => id,
                        Ok(Response::Error { .. }) => {
                            return Ok(Outcome::Failed("state_unavailable"));
                        }
                        _ => return Ok(Outcome::Failed("protocol_claim_uncertain")),
                    };
                    self.phase = Phase::Writing {
                        request: id,
                        lease,
                        write,
                    };
                    Ok(Outcome::Pending)
                }
                _ => Ok(Outcome::Failed("state_unavailable")),
            },
            Phase::Writing {
                request,
                lease,
                write,
            } => match send(Command::TakeResult { request }) {
                Ok(Response::Pending) => {
                    self.phase = Phase::Writing {
                        request,
                        lease,
                        write,
                    };
                    Ok(Outcome::Pending)
                }
                Ok(Response::Result { payload }) => {
                    let released = released(send(Command::ReleaseResult { request }));
                    if released && write.confirmed(payload).is_ok() {
                        return Ok(Outcome::Claimed {
                            lease,
                            scope: Box::new(self.scope.clone()),
                        });
                    }
                    let Ok(request) = self.submit_read(lease.slot, &mut send) else {
                        return Ok(Outcome::Failed("protocol_claim_uncertain"));
                    };
                    self.phase = Phase::Reconciling { request, lease };
                    Ok(Outcome::Pending)
                }
                _ => {
                    let _ = send(Command::ReleaseResult { request });
                    let Ok(request) = self.submit_read(lease.slot, &mut send) else {
                        return Ok(Outcome::Failed("protocol_claim_uncertain"));
                    };
                    self.phase = Phase::Reconciling { request, lease };
                    Ok(Outcome::Pending)
                }
            },
            Phase::Reconciling { request, lease } => match send(Command::TakeResult { request })? {
                Response::Pending => {
                    self.phase = Phase::Reconciling { request, lease };
                    Ok(Outcome::Pending)
                }
                Response::Result { payload } => {
                    if !released(send(Command::ReleaseResult { request })) {
                        return Ok(Outcome::Failed("protocol_claim_uncertain"));
                    }
                    let (_, slot) =
                        protocol_slot_state::read(lease.slot, &payload).map_err(str::to_owned)?;
                    let matches = slot.active.is_some_and(|claim| {
                        claim.lease == lease
                            && claim.scope == self.scope
                            && claim.issuer == self.issuer
                            && claim.entries.is_empty()
                    });
                    if matches {
                        Ok(Outcome::Claimed {
                            lease,
                            scope: Box::new(self.scope.clone()),
                        })
                    } else {
                        Ok(Outcome::Failed("protocol_claim_uncertain"))
                    }
                }
                _ => Ok(Outcome::Failed("protocol_claim_uncertain")),
            },
            Phase::Done => Ok(Outcome::Failed("protocol_claim_finished")),
        }
    }
    fn submit_read(
        &self,
        slot: u8,
        send: &mut impl FnMut(Command) -> Transport,
    ) -> ModuleResult<u64> {
        let read = protocol_slot_state::read_request(slot).map_err(str::to_owned)?;
        match send(Command::Request {
            service: STATE_SERVICE.into(),
            payload: read.payload(),
            timeout_ms: 5_000,
            operation_id: fresh_state_operation_id().map_err(str::to_owned)?,
            parent: None,
        })? {
            Response::Accepted { id } => Ok(id),
            _ => Err("state_unavailable".into()),
        }
    }
}
fn released(response: Transport) -> bool {
    match response {
        Ok(Response::Ok) => true,
        Ok(Response::Error { code, .. }) if code == "not_found" => true,
        _ => false,
    }
}

impl super::Pipeline {
    pub(super) fn protocol_claim_current(&self, claim: &ClaimJob, now: u64) -> bool {
        let scope = claim.scope();
        let Some(gate) = self.effect_gate.as_ref() else {
            return false;
        };
        if gate.fenced() || gate.require_epoch(&scope.authority_epoch).is_err() {
            return false;
        }
        let Some(policy) = self.effect_authority.as_ref() else {
            return false;
        };
        if policy.effect_owner != claim.issuer() {
            return false;
        }
        let Some(source) = serde_json::to_value(&scope.source)
            .ok()
            .and_then(|value| {
                serde_json::from_value::<amaterasu_module_sdk::effect_authority::Source>(value).ok()
            })
            .filter(|source| source.valid())
        else {
            return false;
        };
        let Some((snapshot, _)) = self.effect_bindings.get(&source) else {
            return false;
        };
        self.identity.current(snapshot, now)
            && self
                .protocol_capabilities
                .get(&source)
                .is_some_and(|attestation| attestation.issuer_epoch == scope.issuer_epoch)
    }
}

#[cfg(test)]
mod tests;
