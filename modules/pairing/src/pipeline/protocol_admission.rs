//! Pairing-owned protocol admission and source-specific negotiation state.
//!
//! The selected protocol owner may attest only capabilities verified for the
//! actual authenticated session. The selected issuer may request an effect, but
//! no effect is admitted until host-issued provider incarnation and the durable
//! broker-slot transaction are wired end to end.
use super::Pipeline;
use amaterasu_module_sdk::{
    effect_authority::{FeedbackPermission, ProtocolCapability, Source},
    extension_api::LifecycleTarget,
    protocol::PendingEffect,
};
use serde_json::{Value, json};
use std::collections::BTreeSet;

impl Pipeline {
    pub(super) fn attest_protocol_capabilities(
        &mut self,
        version: u8,
        epoch: String,
        source: Source,
        capabilities: BTreeSet<ProtocolCapability>,
        now: u64,
    ) -> Result<Value, &'static str> {
        if version != 1
            || capabilities.len() > 32
            || capabilities.iter().any(|capability| !capability.valid())
        {
            return Err("invalid_protocol_attestation");
        }
        let gate = self.effect_gate.as_ref().ok_or("authority_unavailable")?;
        gate.require_epoch(&epoch)?;
        if gate.fenced() {
            return Err("source_denied");
        }
        let (snapshot, _) = self.effect_bindings.get(&source).ok_or("source_denied")?;
        if !self.identity.current(snapshot, now) {
            return Err("source_denied");
        }
        let policy = self
            .effect_authority
            .as_ref()
            .ok_or("authority_unavailable")?;
        if capabilities.iter().any(|capability| {
            !policy
                .protocol_policies
                .iter()
                .any(|rule| &rule.capability == capability)
        }) {
            return Err("scope_denied");
        }
        if self
            .protocol_capabilities
            .get(&source)
            .is_some_and(|previous| previous != &capabilities)
        {
            return Err("protocol_attestation_conflict");
        }
        self.protocol_capabilities
            .insert(source.clone(), capabilities);
        Ok(json!({"ok":true,"version":1,"epoch":epoch,"source":source}))
    }

    pub(super) fn admit_protocol_effect(
        &self,
        version: u8,
        epoch: String,
        source: Source,
        effect: PendingEffect,
        caller: &str,
        now: u64,
    ) -> Result<Value, &'static str> {
        if version != 1 {
            return Err("invalid_protocol_effect");
        }
        let gate = self.effect_gate.as_ref().ok_or("authority_unavailable")?;
        gate.require_epoch(&epoch)?;
        if gate.fenced() {
            return Err("source_denied");
        }
        let (snapshot, certificate) = self.effect_bindings.get(&source).ok_or("source_denied")?;
        if !self.identity.current(snapshot, now) {
            return Err("source_denied");
        }
        let policy = self
            .effect_authority
            .as_ref()
            .ok_or("authority_unavailable")?;
        let capability = ProtocolCapability(effect.capability.clone());
        let selected = policy
            .protocol_policies
            .iter()
            .find(|rule| rule.capability == capability)
            .ok_or("scope_denied")?;
        if !self
            .protocol_capabilities
            .get(&source)
            .is_some_and(|caps| caps.contains(&capability))
            || effect.caller != caller
            || effect.context.authority_epoch != epoch
            || effect.context.effect_owner_epoch != source.effect_owner_epoch
            || effect.context.peer_ref != source.id
            || !matches!(&effect.context.target, LifecycleTarget::Stream { host_epoch, session_id, stream_id, generation }
                if *host_epoch == source.host_epoch && *session_id == source.session_id
                    && *stream_id == source.stream_id && *generation == source.stream_generation)
            || effect.provider != selected.provider
            || effect.message_type != selected.message_type
            || effect.method != selected.method
            || effect.cleanup != selected.cleanup
        {
            return Err("scope_denied");
        }
        snapshot
            .check_protocol_grant(
                certificate,
                now,
                &capability,
                match selected.required_permission {
                    FeedbackPermission::View => amaterasu_identity::PERMISSION_VIEW,
                    FeedbackPermission::Launch => amaterasu_identity::PERMISSION_LAUNCH,
                },
            )
            .map_err(|_| "scope_denied")?;
        // Source negotiation and client grant are necessary but not sufficient.
        // The broker's current SelectedMethod lacks a host-issued provider
        // incarnation, and the slot/retirement transaction is not wired here.
        Err("protocol_authority_unavailable")
    }
}
