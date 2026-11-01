//! Journal-bound broker dispatch for selected protocol effects.
//!
//! The selected pairing authority must authenticate and authorize the source,
//! negotiate capability and select the provider. These helpers are inert until
//! trusted composition supplies the exact authority and issuer broker endpoints.
//! Opening or recovering a source requires durable journal reconciliation and
//! actual old-worker retirement; no State read alone establishes that proof.
use crate::{
    Command, Response,
    broker_effect_fence::{
        self, EffectFenceCommand, EffectScope, GuardedEffect, GuardedEffectIdentity,
        ProtectedMethod, SelectedEffectMethod, SourceSlotLease,
    },
    effect_authority::Source,
    extension_api::LifecycleTarget,
    protocol::{ObligationStore, PendingEffect, Reservation, Route},
    protocol_sequence::{Entry, IssuerRetirement, SequencedSource},
    protocol_state::ReadyReservation,
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use sha2::{Digest, Sha256};

type SendResult = Result<Response, String>;

/// A validated exact mapping. Construct only from an authenticated lifecycle
/// source, trusted startup route and journal record, never from remote selectors.
#[derive(Clone)]
pub struct JournaledEffect {
    pending: PendingEffect,
    issuer: Source,
    route: Route,
    guarded: GuardedEffect,
}
impl JournaledEffect {
    pub fn new(
        pending: PendingEffect,
        issuer: Source,
        scope: EffectScope,
        route: Route,
        sequence: u64,
        payload: &[u8],
    ) -> Result<Self, &'static str> {
        let identity = journal_identity(&pending, &issuer, &scope, &route, sequence)?;
        let digest = format!("{:x}", Sha256::digest(payload));
        if payload.len() > route.max_payload_bytes || digest != identity.payload_sha256 {
            return Err("effect_identity_mismatch");
        }
        let schema_revision = identity.schema_revision;
        let guarded = GuardedEffect {
            scope,
            sequence,
            capability: pending.capability.clone(),
            service: pending.method.service.clone(),
            method: pending.method.method.clone(),
            interface_major: pending.method.interface_major,
            interface_minor: pending.method.interface_minor,
            schema_revision,
            payload_sha256: digest,
            payload: serde_json::json!({"context":pending.context, "capability":pending.capability,
                "message_type":pending.message_type,"payload_base64":STANDARD.encode(payload)}),
            timeout_ms: pending.timeout_ms,
            operation_id: pending.operation_id.clone(),
            parent: pending.parent,
        };
        Ok(Self {
            pending,
            issuer,
            route,
            guarded,
        })
    }
    /// Bind this exact effect to a broker-issued slot lease. Slot mode never
    /// uses the older unbound Arm command.
    pub fn in_slot(self, lease: SourceSlotLease) -> Result<SlottedEffect, &'static str> {
        if self.guarded.scope.source.slot() != Some(lease.slot) {
            return Err("source_slot_mismatch");
        }
        Ok(SlottedEffect {
            effect: self,
            lease,
        })
    }
    /// The authority may arm only after durable journal commit, source binding,
    /// provider selection and the applicable client permission are confirmed.
    pub fn arm(&self, mut authority: impl FnMut(Command) -> SendResult) -> Result<(), String> {
        require_ok(authority(command(EffectFenceCommand::Arm {
            effect: self.guarded.clone(),
        }))?)
    }
    /// Uses the real broker's serialized dispatch, never direct CallApi. Even a
    /// lost reply consumes the receipt and leaves recovery responsible for fence.
    pub fn dispatch(
        &self,
        receipt: &mut ReadyReservation,
        mut issuer: impl FnMut(Command) -> SendResult,
    ) -> SendResult {
        for approval in [&self.route.approval, &self.route.cleanup_approval] {
            let method = &approval.method;
            let response = issuer(Command::QueryApiApproval {
                service: method.service.clone(),
                method: method.method.clone(),
                interface_major: method.interface_major,
                min_minor: method.interface_minor,
            })?;
            let Response::Result { payload } = response else {
                return Err("approval_unavailable".into());
            };
            if serde_json::from_value::<amaterasu_contracts::ApiMethodApproval>(payload)
                .ok()
                .as_ref()
                != Some(approval)
            {
                return Err("approval_mismatch".into());
            }
        }
        if receipt.reserve(&self.pending).map_err(str::to_owned)? != Reservation::New {
            return Err("operation_requires_reconciliation".into());
        }
        issuer(command(EffectFenceCommand::Dispatch {
            effect: self.guarded.clone(),
        }))
    }
    /// Fencing alone is not evidence of provider retirement.
    pub fn fence(&self, mut authority: impl FnMut(Command) -> SendResult) -> Result<(), String> {
        require_ok(authority(command(EffectFenceCommand::Fence {
            effect: self.guarded.clone(),
        }))?)
    }
    /// Fetch exact retirement through the selected authority endpoint. A
    /// compacted watermark is not an exact receipt and is rejected here.
    pub fn retirement(
        &self,
        mut authority: impl FnMut(Command) -> SendResult,
    ) -> Result<BrokerRetirement, String> {
        match authority(command(EffectFenceCommand::Lookup {
            effect: self.guarded.clone(),
        }))? {
            Response::Result { payload }
                if payload.get("status").and_then(serde_json::Value::as_str) == Some("retired") =>
            {
                Ok(BrokerRetirement {
                    issuer: self.issuer.clone(),
                    pending: self.pending.clone(),
                })
            }
            _ => Err("issuer_retirement_unproven".into()),
        }
    }
}
/// An effect bound to a selected broker epoch and slot incarnation.
pub struct SlottedEffect {
    effect: JournaledEffect,
    lease: SourceSlotLease,
}
impl SlottedEffect {
    /// The authority must confirm durable issue, authenticated pairing,
    /// permission and its semantic schema revision before this call.
    pub fn arm(&self, mut authority: impl FnMut(Command) -> SendResult) -> Result<(), String> {
        let guarded = &self.effect.guarded;
        let response = authority(command(EffectFenceCommand::SelectedMethod {
            method: ProtectedMethod {
                service: guarded.service.clone(),
                method: guarded.method.clone(),
                interface_major: guarded.interface_major,
            },
        }))?;
        let Response::Result { payload } = response else {
            return Err("selection_unavailable".into());
        };
        let selected: SelectedEffectMethod =
            serde_json::from_value(payload).map_err(|_| "selection_invalid")?;
        if selected.version != 1
            || selected.provider != self.effect.route.approval.provider
            || selected.method != self.effect.route.approval.method
        {
            return Err("selection_mismatch".into());
        }
        require_ok(authority(slot_command(
            &self.lease,
            EffectFenceCommand::ArmSelected {
                effect: guarded.clone(),
                expected: Box::new(selected),
            },
        ))?)
    }
    pub fn dispatch(
        &self,
        receipt: &mut ReadyReservation,
        mut issuer: impl FnMut(Command) -> SendResult,
    ) -> SendResult {
        for approval in [
            &self.effect.route.approval,
            &self.effect.route.cleanup_approval,
        ] {
            let method = &approval.method;
            let response = issuer(Command::QueryApiApproval {
                service: method.service.clone(),
                method: method.method.clone(),
                interface_major: method.interface_major,
                min_minor: method.interface_minor,
            })?;
            let Response::Result { payload } = response else {
                return Err("approval_unavailable".into());
            };
            if serde_json::from_value::<amaterasu_contracts::ApiMethodApproval>(payload)
                .ok()
                .as_ref()
                != Some(approval)
            {
                return Err("approval_mismatch".into());
            }
        }
        if receipt
            .reserve(&self.effect.pending)
            .map_err(str::to_owned)?
            != Reservation::New
        {
            return Err("operation_requires_reconciliation".into());
        }
        issuer(slot_command(
            &self.lease,
            EffectFenceCommand::Dispatch {
                effect: self.effect.guarded.clone(),
            },
        ))
    }
    pub fn fence(&self, mut authority: impl FnMut(Command) -> SendResult) -> Result<(), String> {
        require_ok(authority(slot_command(
            &self.lease,
            EffectFenceCommand::Fence {
                effect: self.effect.guarded.clone(),
            },
        ))?)
    }
    pub fn retirement(
        &self,
        mut authority: impl FnMut(Command) -> SendResult,
    ) -> Result<BrokerRetirement, String> {
        retired_response(
            authority(slot_command(
                &self.lease,
                EffectFenceCommand::Lookup {
                    effect: self.effect.guarded.clone(),
                },
            ))?,
            &self.effect.issuer,
            &self.effect.pending,
        )
    }
}

/// Payload-free recovery binding obtained from a validated State journal entry.
/// A broker retired result does not prove persistent provider cleanup.
/// A legacy v4 record lacks a host provider incarnation. It cannot establish
/// historical worker retirement and is retained for explicit reconciliation.
pub struct JournaledIdentity;
impl JournaledIdentity {
    pub fn new(
        entry: &Entry,
        scope: EffectScope,
        route: &Route,
        lease: SourceSlotLease,
    ) -> Result<Self, &'static str> {
        if scope.source.slot() != Some(lease.slot) {
            return Err("source_slot_mismatch");
        }
        journal_identity(&entry.effect, &entry.issuer, &scope, route, entry.sequence)?;
        Ok(Self)
    }
    /// Identity-only recovery cannot substitute for the original host receipt.
    pub fn fence(&self, _authority: impl FnMut(Command) -> SendResult) -> Result<(), String> {
        Err("historical_host_receipt_required".into())
    }
    pub fn retirement(
        &self,
        _authority: impl FnMut(Command) -> SendResult,
    ) -> Result<BrokerRetirement, String> {
        Err("historical_host_receipt_required".into())
    }
}

fn journal_identity(
    pending: &PendingEffect,
    issuer: &Source,
    scope: &EffectScope,
    route: &Route,
    sequence: u64,
) -> Result<GuardedEffectIdentity, &'static str> {
    crate::protocol::ProtocolRouter::new(vec![route.clone()])?;
    let source = SequencedSource::new(&pending.caller, &pending.context)?;
    let target_matches = matches!(&pending.context.target, LifecycleTarget::Stream { host_epoch, session_id, stream_id, generation }
        if issuer.host_epoch == *host_epoch && issuer.session_id == *session_id
        && issuer.stream_id == *stream_id && issuer.stream_generation == *generation);
    if broker_effect_fence::source(issuer)? != scope.source
        || scope.authority_epoch != pending.context.authority_epoch
        || issuer.effect_owner_epoch != pending.context.effect_owner_epoch
        || scope.issuer_epoch != pending.issuer_epoch
        || scope.issuer_epoch.is_empty()
        || scope.issuer_epoch.len() > 128
        || scope.issuer_epoch.chars().any(char::is_control)
        || !target_matches
        || pending.operation_id != source.operation_id(sequence)?
        || pending.message_type != route.message_type
        || pending.capability != route.capability
        || pending.provider != route.approval.provider
        || pending.method != route.method
        || pending.cleanup != route.cleanup
        || pending.action_approval_sha256 != crate::protocol::approval_fingerprint(&route.approval)
        || pending.cleanup_approval_sha256
            != crate::protocol::approval_fingerprint(&route.cleanup_approval)
        || route.approval.method.service != pending.method.service
        || route.approval.method.method != pending.method.method
        || route.approval.method.interface_major != pending.method.interface_major
        || route.approval.method.interface_minor != pending.method.interface_minor
        || route.cleanup_approval.provider != pending.provider
        || !route.approval.callers.contains(&pending.caller)
        || !route.cleanup_approval.callers.contains(&pending.caller)
    {
        return Err("effect_identity_mismatch");
    }
    let schema_revision = u32::try_from(pending.method.schema_revision)
        .ok()
        .filter(|value| *value != 0)
        .ok_or("effect_identity_mismatch")?;
    Ok(GuardedEffectIdentity {
        scope: scope.clone(),
        sequence,
        capability: pending.capability.clone(),
        provider: pending.provider.clone(),
        service: pending.method.service.clone(),
        method: pending.method.method.clone(),
        interface_major: pending.method.interface_major,
        interface_minor: pending.method.interface_minor,
        schema_revision,
        payload_sha256: pending.payload_sha256.clone(),
        timeout_ms: pending.timeout_ms,
        operation_id: pending.operation_id.clone(),
        parent: pending.parent,
    })
}
fn retired_response(
    response: Response,
    issuer: &Source,
    pending: &PendingEffect,
) -> Result<BrokerRetirement, String> {
    match response {
        Response::Result { payload }
            if payload.get("status").and_then(serde_json::Value::as_str) == Some("retired") =>
        {
            Ok(BrokerRetirement {
                issuer: issuer.clone(),
                pending: pending.clone(),
            })
        }
        _ => Err("issuer_retirement_unproven".into()),
    }
}
fn slot_command(lease: &SourceSlotLease, operation: EffectFenceCommand) -> Command {
    command(EffectFenceCommand::SlotCommand {
        lease: lease.clone(),
        command: Box::new(operation),
    })
}

/// An exact broker lookup result, not a provider result or delivery proof.
pub struct BrokerRetirement {
    issuer: Source,
    pending: PendingEffect,
}
impl IssuerRetirement for BrokerRetirement {
    fn verify_retired(&self, issuer: &Source, effect: &PendingEffect) -> Result<(), &'static str> {
        if issuer == &self.issuer && effect == &self.pending {
            Ok(())
        } else {
            Err("issuer_retirement_unproven")
        }
    }
}
fn command(command: EffectFenceCommand) -> Command {
    Command::EffectFence {
        command: Box::new(command),
    }
}
fn require_ok(response: Response) -> Result<(), String> {
    match response {
        Response::Ok => Ok(()),
        Response::Error { code, .. } => Err(code),
        _ => Err("unexpected_broker_response".into()),
    }
}

/// Owns the actual aggregate issuer worker for one trusted source. The worker
/// must not return until all its child dispatchers and callbacks are retired.
/// The separate host lifecycle acknowledgment is still required by the broker.
pub struct IssuerWorkerRetirement {
    acknowledgment: EffectFenceCommand,
    worker: Option<std::thread::JoinHandle<Result<(), String>>>,
    joined: bool,
    acknowledged: bool,
    failed: bool,
}
impl IssuerWorkerRetirement {
    /// Trusted composition must bind this handle to the actual aggregate issuer
    /// worker for this exact source. A thread handle is not client authority.
    pub fn new(
        lease: SourceSlotLease,
        scope: EffectScope,
        worker: std::thread::JoinHandle<Result<(), String>>,
    ) -> Result<Self, &'static str> {
        if scope.source.slot() != Some(lease.slot) {
            return Err("source_slot_mismatch");
        }
        Ok(Self {
            acknowledgment: EffectFenceCommand::SourceRetired { lease, scope },
            worker: Some(worker),
            joined: false,
            acknowledged: false,
            failed: false,
        })
    }
    /// Broker restart form: the aggregate worker must include every old issuer
    /// source/dispatcher. Provider and host recovery acknowledgments are separate.
    pub fn for_recovery(
        broker_epoch: String,
        worker: std::thread::JoinHandle<Result<(), String>>,
    ) -> Result<Self, &'static str> {
        if broker_epoch.len() != 32
            || !broker_epoch
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err("invalid_broker_epoch");
        }
        Ok(Self {
            acknowledgment: EffectFenceCommand::RecoveryRetired { broker_epoch },
            worker: Some(worker),
            joined: false,
            acknowledged: false,
            failed: false,
        })
    }
    /// Nonblocking while the worker is live. A lost/error acknowledgment can
    /// be retried after join; a failed or panicked worker is never acknowledged.
    pub fn poll(&mut self, mut issuer: impl FnMut(Command) -> SendResult) -> Result<bool, String> {
        if self.acknowledged {
            return Ok(true);
        }
        if self.failed {
            return Err("issuer_worker_retirement_failed".into());
        }
        if !self.joined {
            let Some(worker) = self.worker.as_ref() else {
                self.failed = true;
                return Err("issuer_worker_retirement_failed".into());
            };
            if !worker.is_finished() {
                return Ok(false);
            }
            let worker = self.worker.take().expect("checked worker");
            if !matches!(worker.join(), Ok(Ok(()))) {
                self.failed = true;
                return Err("issuer_worker_retirement_failed".into());
            }
            self.joined = true;
        }
        require_ok(issuer(command(self.acknowledgment.clone()))?)?;
        self.acknowledged = true;
        Ok(true)
    }
}
