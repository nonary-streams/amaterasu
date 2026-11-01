//! Pairing-owned protocol admission and source-specific negotiation state.
//!
//! The selected protocol owner may attest only capabilities verified for the
//! actual authenticated session. The selected issuer may request an effect, but
//! no effect is admitted until host-issued provider incarnation and the durable
//! broker-slot transaction are wired end to end.
use super::{Pipeline, Transport, complete};
use amaterasu_module_sdk::{
    Command, ModuleResult, Response,
    broker_effect_fence::{EffectFenceCommand, ProtectedMethod, SelectedEffectMethod},
    effect_authority::{FeedbackPermission, ProtocolCapability, Source},
    extension_api::LifecycleTarget,
    protocol::PendingEffect,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

pub(super) struct ProtocolAttestation {
    capabilities: BTreeSet<ProtocolCapability>,
    issuer_epoch: String,
}

impl Pipeline {
    /// Check the real broker selection after source and client policy. A v1
    /// selection has no host-issued provider incarnation and cannot be armed.
    pub(super) fn handle_protocol_admission(
        &self,
        request_id: u64,
        caller: &str,
        initiator: &str,
        payload: Value,
        now: u64,
        send: &mut impl FnMut(Command) -> Transport,
    ) -> ModuleResult<()> {
        let response = (|| {
            let policy = self
                .effect_authority
                .as_ref()
                .ok_or("authority_unavailable")?;
            if caller != initiator
                || caller != policy.effect_owner
                || !serde_json::to_vec(&payload).is_ok_and(|bytes| bytes.len() <= 12_000)
            {
                return Err("forbidden");
            }
            let request: amaterasu_module_sdk::effect_authority::Request =
                serde_json::from_value(payload).map_err(|_| "invalid_request")?;
            use amaterasu_module_sdk::effect_authority::Request;
            let selected = match request {
                Request::AdmitProtocolEffect {
                    version,
                    epoch,
                    source,
                    effect,
                } => self.admit_protocol_effect(version, epoch, source, *effect, caller, now)?,
                Request::AdmitProtocolEffectV2 {
                    version,
                    epoch,
                    source,
                    effect,
                    sequence,
                    payload,
                    cleanup,
                } => {
                    if version != 2
                        || payload.len() > amaterasu_module_sdk::protocol::MAX_MESSAGE_BYTES
                        || amaterasu_module_sdk::protocol_state::CleanupReference::new(cleanup.id)
                            .is_err()
                        || format!("{:x}", Sha256::digest(&payload)) != effect.payload_sha256
                    {
                        return Err("invalid_protocol_effect");
                    }
                    // The existing sequence key does not include the full
                    // device source. This check is provisional only: v2 stays
                    // closed until a source-qualified journal is available.
                    let source_sequence =
                        amaterasu_module_sdk::protocol_sequence::SequencedSource::new(
                            caller,
                            &effect.context,
                        )
                        .map_err(|_| "invalid_protocol_effect")?;
                    if source_sequence.operation_id(sequence).ok().as_deref()
                        != Some(effect.operation_id.as_str())
                    {
                        return Err("invalid_protocol_effect");
                    }
                    let selected =
                        self.admit_protocol_effect(1, epoch, source, *effect, caller, now)?;
                    if payload.len() > selected.max_payload_bytes {
                        return Err("invalid_protocol_effect");
                    }
                    selected
                }
                _ => return Err("invalid_request"),
            };
            let query = Command::EffectFence {
                command: Box::new(EffectFenceCommand::SelectedMethod {
                    method: ProtectedMethod {
                        service: selected.method.service.clone(),
                        method: selected.method.method.clone(),
                        interface_major: selected.method.interface_major,
                    },
                }),
            };
            let Ok(Response::Result { payload }) = send(query) else {
                return Err("protocol_authority_unavailable");
            };
            let actual: SelectedEffectMethod =
                serde_json::from_value(payload).map_err(|_| "protocol_authority_unavailable")?;
            if actual.version != 1
                || actual.provider != selected.provider
                || actual.method != selected.method_schema
            {
                return Err("scope_denied");
            }
            // v1 cannot bind a host-issued provider incarnation. Do not issue
            // a claim, ticket, State write or Arm from this point.
            Err("protocol_authority_unavailable")
        })();
        complete(
            send,
            request_id,
            match response {
                Ok(value) => value,
                Err(code) => super::error(code),
            },
        )
    }

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
            .is_some_and(|previous| previous.capabilities != capabilities)
        {
            return Err("protocol_attestation_conflict");
        }
        let issuer_epoch = if let Some(previous) = self.protocol_capabilities.get(&source) {
            previous.issuer_epoch.clone()
        } else {
            let mut bytes = [0_u8; 16];
            getrandom::fill(&mut bytes).map_err(|_| "entropy_unavailable")?;
            let issuer_epoch = bytes
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>();
            self.protocol_capabilities.insert(
                source.clone(),
                ProtocolAttestation {
                    capabilities,
                    issuer_epoch: issuer_epoch.clone(),
                },
            );
            issuer_epoch
        };
        Ok(json!({"ok":true,"version":1,"epoch":epoch,"source":source,"issuer_epoch":issuer_epoch}))
    }

    pub(super) fn admit_protocol_effect(
        &self,
        version: u8,
        epoch: String,
        source: Source,
        effect: PendingEffect,
        caller: &str,
        now: u64,
    ) -> Result<amaterasu_module_sdk::effect_authority::ProtocolEffectPolicy, &'static str> {
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
            .is_some_and(|attestation| {
                attestation.capabilities.contains(&capability)
                    && attestation.issuer_epoch == effect.issuer_epoch
            })
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
        Ok(selected.clone())
    }
}
