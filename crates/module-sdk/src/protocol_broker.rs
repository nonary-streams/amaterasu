//! Journal-bound broker dispatch for selected protocol effects.
//!
//! The selected pairing authority must authenticate and authorize the source,
//! negotiate capability and select the provider. These helpers are inert until
//! trusted composition supplies the exact authority and issuer broker endpoints.
//! Opening or recovering a source requires durable journal reconciliation and
//! actual old-worker retirement; no State read alone establishes that proof.
use crate::{
    Command, Response,
    broker_effect_fence::{self, EffectFenceCommand, EffectScope, GuardedEffect},
    effect_authority::Source,
    extension_api::LifecycleTarget,
    protocol::{ObligationStore, PendingEffect, Reservation, Route},
    protocol_sequence::{IssuerRetirement, SequencedSource},
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
        // Apply the same route and reserved-message validation used by the
        // unchanged protocol consumer before creating any broker operation.
        crate::protocol::ProtocolRouter::new(vec![route.clone()])?;
        let source = SequencedSource::new(&pending.caller, &pending.context)?;
        let target_matches = matches!(&pending.context.target, LifecycleTarget::Stream { host_epoch, session_id, stream_id, generation }
            if issuer.host_epoch == *host_epoch && issuer.session_id == *session_id
            && issuer.stream_id == *stream_id && issuer.stream_generation == *generation);
        let digest = format!("{:x}", Sha256::digest(payload));
        if broker_effect_fence::source(&issuer)? != scope.source
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
            || payload.len() > route.max_payload_bytes
            || digest != pending.payload_sha256
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
