//! Versioned, pairing-journal-backed protocol slot selection and recovery.
//!
//! Pairing authenticates the source, selected policy, permissions and State
//! endpoint. Broker and host retirement are separate from external cleanup.
use crate::{
    Command, Response,
    broker_effect_fence::{
        self, EffectFenceCommand, EffectScope, GuardedEffect, GuardedEffectIdentity,
        HostProviderInstanceRetirement, HostProviderRetirement, ProtectedMethod,
        SelectedEffectMethod, SourceSlotLease,
    },
    effect_authority::Source,
    extension_api::LifecycleTarget,
    protocol::{DispatchRequest, PendingEffect, ProtocolRouter, Route},
    protocol_slot_state,
    protocol_state::{CleanupReference, DurableStateBinding},
    state::StateRequest,
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use sha2::{Digest, Sha256};
type SendResult = Result<Response, String>;
fn command(command: EffectFenceCommand) -> Command {
    Command::EffectFence {
        command: Box::new(command),
    }
}
fn slot_command(lease: &SourceSlotLease, operation: EffectFenceCommand) -> Command {
    command(EffectFenceCommand::SlotCommand {
        lease: lease.clone(),
        command: Box::new(operation),
    })
}
fn require_ok(response: Response) -> Result<(), String> {
    match response {
        Response::Ok => Ok(()),
        Response::Error { code, .. } => Err(code),
        _ => Err("unexpected_broker_response".into()),
    }
}
/// Inputs authenticated by pairing and selected at trusted composition.
pub struct SlotActionRequest<'a> {
    pub pending: PendingEffect,
    pub issuer: &'a Source,
    pub scope: EffectScope,
    pub lease: SourceSlotLease,
    pub route: &'a Route,
    pub sequence: u64,
    pub payload: &'a [u8],
}

/// A selected action, prepared only after trusted pairing has authenticated
/// the source, negotiated capability and checked the client's permission.
/// This is not an admission receipt; the broker must recheck the selection
/// under its admission lock after the canonical State write.
pub struct SelectedSlotAction {
    selected: SelectedEffectMethod,
    cleanup_approval: amaterasu_contracts::ApiMethodApproval,
    pending: PendingEffect,
    scope: EffectScope,
    lease: SourceSlotLease,
    sequence: u64,
}
impl SelectedSlotAction {
    /// Query the selected broker authority and bind its host-issued instance.
    /// `issuer` and `scope.issuer_epoch` must come from pairing attestation.
    pub fn query(
        request: SlotActionRequest<'_>,
        mut authority: impl FnMut(Command) -> SendResult,
    ) -> Result<Self, String> {
        let SlotActionRequest {
            pending,
            issuer,
            scope,
            lease,
            route,
            sequence,
            payload,
        } = request;
        validate_action(&pending, issuer, &scope, &lease, route, sequence, payload)?;
        let method = &route.approval.method;
        let response = authority(command(EffectFenceCommand::SelectedMethod {
            method: ProtectedMethod {
                service: method.service.clone(),
                method: method.method.clone(),
                interface_major: method.interface_major,
            },
        }))?;
        let Response::Result { payload } = response else {
            return Err("selection_unavailable".into());
        };
        let selected: SelectedEffectMethod =
            serde_json::from_value(payload).map_err(|_| "selection_invalid")?;
        let Some(instance) = selected.provider_incarnation.as_ref() else {
            return Err("host_provider_incarnation_required".into());
        };
        if selected.version != 1
            || selected.provider != route.approval.provider
            || selected.method != route.approval.method
            || !valid_incarnation(instance)
        {
            return Err("selection_mismatch".into());
        }
        let cleanup = &route.cleanup_approval.method;
        let response = authority(command(EffectFenceCommand::SelectedCleanupMethod {
            method: ProtectedMethod {
                service: cleanup.service.clone(),
                method: cleanup.method.clone(),
                interface_major: cleanup.interface_major,
            },
        }))?;
        let Response::Result { payload } = response else {
            return Err("cleanup_selection_unavailable".into());
        };
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct CleanupSelection {
            approval: amaterasu_contracts::ApiMethodApproval,
            provider_incarnation: Option<broker_effect_fence::ProviderIncarnation>,
        }
        let cleanup: CleanupSelection =
            serde_json::from_value(payload).map_err(|_| "cleanup_selection_invalid")?;
        if cleanup.approval != route.cleanup_approval
            || cleanup.approval.provider != selected.provider
            || cleanup.provider_incarnation != selected.provider_incarnation
        {
            return Err("cleanup_selection_mismatch".into());
        }
        Ok(Self {
            selected,
            cleanup_approval: cleanup.approval,
            pending,
            scope,
            lease,
            sequence,
        })
    }
    pub fn selected_method(&self) -> &SelectedEffectMethod {
        &self.selected
    }
    pub fn scope(&self) -> &EffectScope {
        &self.scope
    }
    pub fn lease(&self) -> &SourceSlotLease {
        &self.lease
    }
    pub fn sequence(&self) -> u64 {
        self.sequence
    }
    pub fn pending(&self) -> &PendingEffect {
        &self.pending
    }
    /// Construct the exact transient broker payload from the selected identity.
    /// The bytes remain outside the durable slot journal.
    pub fn guarded_effect(&self, payload: &[u8]) -> Result<GuardedEffect, &'static str> {
        if payload.len() > crate::protocol::MAX_MESSAGE_BYTES
            || format!("{:x}", Sha256::digest(payload)) != self.pending.payload_sha256
        {
            return Err("effect_identity_mismatch");
        }
        let schema_revision = u32::try_from(self.pending.method.schema_revision)
            .ok()
            .filter(|value| *value != 0)
            .ok_or("invalid_schema_revision")?;
        Ok(GuardedEffect {
            scope: self.scope.clone(),
            sequence: self.sequence,
            capability: self.pending.capability.clone(),
            service: self.pending.method.service.clone(),
            method: self.pending.method.method.clone(),
            interface_major: self.pending.method.interface_major,
            interface_minor: self.pending.method.interface_minor,
            schema_revision,
            payload_sha256: self.pending.payload_sha256.clone(),
            payload: serde_json::json!({"context":self.pending.context,"capability":self.pending.capability,"message_type":self.pending.message_type,"payload_base64":STANDARD.encode(payload)}),
            timeout_ms: self.pending.timeout_ms,
            operation_id: self.pending.operation_id.clone(),
            parent: self.pending.parent,
        })
    }
    /// Reconcile a lost State reply from a fresh exact journal read. This is
    /// persistence evidence only; the broker rechecks the retained read.
    pub fn reconcile_issue(
        &self,
        _binding: &DurableStateBinding,
        response: &serde_json::Value,
        expected: &protocol_slot_state::Entry,
    ) -> Result<ConfirmedSlotIssue, &'static str> {
        let (_, slot) = protocol_slot_state::read(self.lease.slot, response)?;
        let claim = slot.active.ok_or("unknown_claim")?;
        if claim.lease != self.lease
            || claim.scope != self.scope
            || claim.issuer != self.pending.caller
            || !claim.entries.iter().any(|entry| entry == expected)
        {
            return Err("claim_mismatch");
        }
        Ok(ConfirmedSlotIssue {
            entry: expected.clone(),
        })
    }
    /// Atomically hand a confirmed issue and retained State read to the
    /// broker. Transport failure is an uncertain result, not cancellation.
    pub fn commit_admission(
        &self,
        confirmed: &ConfirmedSlotIssue,
        payload: &[u8],
        request: u64,
        state_request: u64,
        mut authority: impl FnMut(Command) -> SendResult,
    ) -> Result<(), String> {
        let entry = confirmed.entry();
        let effect = self.guarded_effect(payload).map_err(str::to_owned)?;
        if entry.issued_lease != self.lease
            || entry.selected_method != self.selected
            || entry.cleanup_approval != self.cleanup_approval
            || entry.cleanup_method != self.pending.cleanup
            || entry.cleanup_confirmed
            || effect.identity(&self.selected.provider) != entry.identity
        {
            return Err("effect_identity_mismatch".into());
        }
        require_ok(authority(slot_command(
            &self.lease,
            EffectFenceCommand::CommitAdmission {
                request,
                effect,
                expected: Box::new(self.selected.clone()),
                expected_entry: serde_json::to_value(entry).map_err(|_| "invalid_journal")?,
                state_request,
            },
        ))?)
    }
    /// Prepare the pairing-owned canonical slot CAS. The binding and State
    /// response must refer to the authority's selected durable CAS provider.
    /// A successful State write is not a broker admission or retirement proof.
    pub fn prepare_issue(
        &self,
        _binding: &DurableStateBinding,
        response: &serde_json::Value,
        cleanup: CleanupReference,
    ) -> Result<PendingSlotIssue, &'static str> {
        let (_, slot) = protocol_slot_state::read(self.lease.slot, response)?;
        let claim = slot.active.ok_or("unknown_claim")?;
        if claim.lease != self.lease
            || claim.scope != self.scope
            || claim.issuer != self.pending.caller
        {
            return Err("claim_mismatch");
        }
        let instance = self
            .selected
            .provider_incarnation
            .clone()
            .ok_or("host_provider_incarnation_required")?;
        let schema_revision = u32::try_from(self.pending.method.schema_revision)
            .ok()
            .filter(|value| *value != 0)
            .ok_or("invalid_schema_revision")?;
        let entry = protocol_slot_state::Entry {
            identity: GuardedEffectIdentity {
                scope: self.scope.clone(),
                sequence: self.sequence,
                capability: self.pending.capability.clone(),
                provider: self.selected.provider.clone(),
                service: self.pending.method.service.clone(),
                method: self.pending.method.method.clone(),
                interface_major: self.pending.method.interface_major,
                interface_minor: self.pending.method.interface_minor,
                schema_revision,
                payload_sha256: self.pending.payload_sha256.clone(),
                timeout_ms: self.pending.timeout_ms,
                operation_id: self.pending.operation_id.clone(),
                parent: self.pending.parent,
            },
            issued_lease: self.lease.clone(),
            provider_incarnation: instance,
            selected_method: self.selected.clone(),
            cleanup_method: self.pending.cleanup.clone(),
            cleanup_approval: self.cleanup_approval.clone(),
            cleanup,
            cleanup_confirmed: false,
        };
        let write = protocol_slot_state::prepare_issue(
            response,
            self.lease.slot,
            &self.lease,
            entry.clone(),
        )?;
        Ok(PendingSlotIssue { write, entry })
    }
}
/// A canonical pairing State CAS, not a broker admission grant.
pub struct PendingSlotIssue {
    write: protocol_slot_state::PendingWrite,
    entry: protocol_slot_state::Entry,
}
impl PendingSlotIssue {
    /// Exact payload-free identity to compare with a fresh State read if the
    /// CAS result is lost. This is not a persistence or admission receipt.
    pub fn entry(&self) -> &protocol_slot_state::Entry {
        &self.entry
    }
    pub fn request(&self) -> &StateRequest {
        self.write.request()
    }
    pub fn state_operation_id(&self) -> &str {
        self.write.state_operation_id()
    }
    /// The caller must supply the terminal result of this exact selected-State
    /// operation. A lost reply is reconciled by rereading the slot.
    pub fn confirmed(
        self,
        response: serde_json::Value,
    ) -> Result<ConfirmedSlotIssue, &'static str> {
        self.write.confirmed(response)?;
        Ok(ConfirmedSlotIssue { entry: self.entry })
    }
}
/// A State confirmation only. Pairing must still authorize and arm the broker;
/// this value has no dispatch method and proves no provider retirement.
pub struct ConfirmedSlotIssue {
    entry: protocol_slot_state::Entry,
}
impl ConfirmedSlotIssue {
    pub fn entry(&self) -> &protocol_slot_state::Entry {
        &self.entry
    }
}
fn valid_incarnation(instance: &broker_effect_fence::ProviderIncarnation) -> bool {
    instance.version == 1
        && instance.id.len() == 32
        && instance
            .id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
fn validate_action(
    pending: &PendingEffect,
    issuer: &Source,
    scope: &EffectScope,
    lease: &SourceSlotLease,
    route: &Route,
    sequence: u64,
    payload: &[u8],
) -> Result<(), String> {
    let router = ProtocolRouter::new(vec![route.clone()]).map_err(str::to_owned)?;
    let request = DispatchRequest {
        context: &pending.context,
        message_type: pending.message_type,
        payload,
        operation_id: &pending.operation_id,
        issuer_epoch: &pending.issuer_epoch,
        timeout_ms: pending.timeout_ms,
        parent: pending.parent,
    };
    if router.prepare_effect(&pending.caller, &request)? != *pending
        || broker_effect_fence::source(issuer).map_err(str::to_owned)? != scope.source
        || scope.source.slot() != Some(lease.slot)
        || pending.context.authority_epoch != scope.authority_epoch
        || pending.issuer_epoch != scope.issuer_epoch
        || pending.context.effect_owner_epoch != issuer.effect_owner_epoch
        || pending.context.peer_ref != issuer.id
        || !matches!(&pending.context.target, LifecycleTarget::Stream { host_epoch, session_id, stream_id, generation }
            if *host_epoch == issuer.host_epoch && *session_id == issuer.session_id
                && *stream_id == issuer.stream_id && *generation == issuer.stream_generation)
        || pending.operation_id
            != protocol_slot_state::operation_id(&pending.caller, scope, sequence)
                .map_err(str::to_owned)?
        || !route.approval.callers.contains(&pending.caller)
        || !route.cleanup_approval.callers.contains(&pending.caller)
    {
        return Err("effect_identity_mismatch".into());
    }
    Ok(())
}

/// A digest-only historical effect read from pairing's canonical State slot.
/// This object is not evidence of old-worker retirement or external cleanup.
pub struct HistoricalSlotEffect {
    lease: SourceSlotLease,
    entry: protocol_slot_state::Entry,
}
impl HistoricalSlotEffect {
    pub fn from_state(
        slot: u8,
        sequence: u64,
        response: &serde_json::Value,
    ) -> Result<Self, &'static str> {
        let (_, state) = protocol_slot_state::read(slot, response)?;
        let claim = state.active.ok_or("unknown_claim")?;
        let entry = claim
            .entries
            .into_iter()
            .find(|item| item.identity.sequence == sequence)
            .ok_or("unknown_obligation")?;
        if entry.identity.operation_id
            != protocol_slot_state::operation_id(&claim.issuer, &claim.scope, sequence)?
        {
            return Err("effect_identity_mismatch");
        }
        Ok(Self {
            lease: claim.lease,
            entry,
        })
    }
    pub fn entry(&self) -> &protocol_slot_state::Entry {
        &self.entry
    }
    fn expected_entry(&self) -> Result<serde_json::Value, String> {
        serde_json::to_value(&self.entry).map_err(|_| "invalid_journal".into())
    }
    /// Verifies only the pairing-owned State read; no retirement is implied.
    pub fn verify_journal(
        &self,
        state_request: u64,
        mut authority: impl FnMut(Command) -> SendResult,
    ) -> Result<(), String> {
        let response = authority(slot_command(
            &self.lease,
            EffectFenceCommand::VerifyHistoricalJournal {
                identity: Box::new(self.entry.identity.clone()),
                provider_incarnation: self.entry.provider_incarnation.clone(),
                expected_entry: self.expected_entry()?,
                state_request,
            },
        ))?;
        match response {
            Response::Result { payload }
                if payload.get("status").and_then(serde_json::Value::as_str)
                    == Some("journal_verified") =>
            {
                Ok(())
            }
            _ => Err("historical_journal_unverified".into()),
        }
    }
    /// The host must have joined the original provider instance and persisted
    /// its exact receipt. The broker checks both retained selected-State reads.
    pub fn restore_from_host(
        &self,
        receipt: HostProviderRetirement,
        state_request: u64,
        host_receipt_state_request: u64,
        mut host: impl FnMut(Command) -> SendResult,
    ) -> Result<(), String> {
        if receipt.version != 1
            || receipt.issued_lease != self.entry.issued_lease
            || receipt.broker_epoch != self.entry.issued_lease.broker_epoch
            || receipt.provider_incarnation != self.entry.provider_incarnation
            || receipt.effect != self.entry.identity
        {
            return Err("historical_receipt_mismatch".into());
        }
        require_ok(host(command(EffectFenceCommand::HostRecoverHistorical {
            lease: self.lease.clone(),
            receipt: Box::new(receipt),
            expected_entry: self.expected_entry()?,
            state_request,
            host_receipt_state_request,
        }))?)
    }
    /// Use the host's durable proof that the original provider instance was
    /// joined, even when no volatile per-effect broker record survived.
    pub fn restore_from_instance(
        &self,
        receipt: HostProviderInstanceRetirement,
        state_request: u64,
        host_receipt_state_request: u64,
        mut host: impl FnMut(Command) -> SendResult,
    ) -> Result<(), String> {
        if receipt.version != 1
            || receipt.broker_epoch != self.entry.issued_lease.broker_epoch
            || receipt.provider != self.entry.identity.provider
            || receipt.provider_incarnation != self.entry.provider_incarnation
        {
            return Err("historical_receipt_mismatch".into());
        }
        require_ok(host(command(
            EffectFenceCommand::HostRecoverHistoricalInstance {
                lease: self.lease.clone(),
                receipt: Box::new(receipt),
                identity: Box::new(self.entry.identity.clone()),
                expected_entry: self.expected_entry()?,
                state_request,
                host_receipt_state_request,
            },
        ))?)
    }
    /// Use a freshly read entry only after pairing independently verifies and
    /// persists cleanup. This does not itself establish cleanup proof.
    pub fn confirm_cleanup(
        &self,
        state_request: u64,
        mut authority: impl FnMut(Command) -> SendResult,
    ) -> Result<(), String> {
        if !self.entry.cleanup_confirmed {
            return Err("cleanup_unproven".into());
        }
        require_ok(authority(slot_command(
            &self.lease,
            EffectFenceCommand::ConfirmHistoricalCleanup {
                identity: Box::new(self.entry.identity.clone()),
                provider_incarnation: self.entry.provider_incarnation.clone(),
                expected_entry: self.expected_entry()?,
                state_request,
            },
        ))?)
    }
}
