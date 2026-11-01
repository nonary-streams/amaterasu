//! Versioned, pairing-journal-backed protocol slot recovery.
//!
//! Pairing authenticates the source, selected policy, permissions and State
//! endpoint. Broker and host retirement are separate from external cleanup.
use crate::{
    Command, Response,
    broker_effect_fence::{EffectFenceCommand, HostProviderRetirement, SourceSlotLease},
    protocol_slot_state,
};
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
