//! Client-side request builder for the shared pairing effect authority.
//! This is not a second gate, source attestation, permission policy, or ingress.
use crate::effect_authority::{Request, Source, response_matches, response_ticket};
use serde_json::Value;
use sha2::{Digest, Sha256};

/// Validated reply from the selected pairing authority, obtained only after
/// the trusted protocol owner attests the authenticated session negotiation.
/// This opaque epoch identifies an issuer incarnation; it is not a grant.
pub struct AttestedProtocolSource {
    issuer_epoch: String,
}
impl AttestedProtocolSource {
    pub fn from_authority_response(
        response: &Value,
        epoch: &str,
        source: &Source,
    ) -> Result<Self, &'static str> {
        let issuer_epoch = response
            .get("issuer_epoch")
            .and_then(Value::as_str)
            .filter(|value| {
                value.len() == 32
                    && value
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            })
            .ok_or("invalid_protocol_attestation")?;
        if !source.valid()
            || epoch.is_empty()
            || epoch.len() > 128
            || epoch.chars().any(char::is_control)
            || response.get("ok").and_then(Value::as_bool) != Some(true)
            || response.get("version").and_then(Value::as_u64) != Some(1)
            || response.get("epoch").and_then(Value::as_str) != Some(epoch)
            || serde_json::from_value::<Source>(
                response
                    .get("source")
                    .cloned()
                    .ok_or("invalid_protocol_attestation")?,
            )
            .ok()
            .as_ref()
                != Some(source)
        {
            return Err("invalid_protocol_attestation");
        }
        Ok(Self {
            issuer_epoch: issuer_epoch.into(),
        })
    }

    pub fn issuer_epoch(&self) -> &str {
        &self.issuer_epoch
    }
}

/// Construct only after trusted protocol-to-lifecycle binding and route-specific
/// permission/negotiation. The selected effect owner must drive all requests and
/// keep the ticket through the final effect and any required transport drain.
pub struct EffectOperation {
    epoch: String,
    source: Source,
    operation_id: String,
    sequence: u64,
}
impl EffectOperation {
    pub fn new(
        epoch: String,
        source: Source,
        caller: &str,
        operation_id: &str,
        sequence: u64,
    ) -> Result<Self, &'static str> {
        if epoch.is_empty()
            || epoch.len() > 128
            || epoch.chars().any(char::is_control)
            || !source.valid()
            || !amaterasu_contracts::valid_name(caller)
            || !amaterasu_contracts::valid_name(operation_id)
            || sequence == 0
        {
            return Err("invalid_authority_operation");
        }
        let mut hash = Sha256::new();
        hash.update(b"amaterasu.protocol.effect/v1\0");
        hash.update((caller.len() as u64).to_be_bytes());
        hash.update(caller.as_bytes());
        hash.update((operation_id.len() as u64).to_be_bytes());
        hash.update(operation_id.as_bytes());
        Ok(Self {
            epoch,
            source,
            operation_id: format!("{:x}", hash.finalize()),
            sequence,
        })
    }
    pub fn admit(&self) -> Request {
        Request::Admit {
            epoch: self.epoch.clone(),
            source: self.source.clone(),
            operation_id: self.operation_id.clone(),
            sequence: self.sequence,
        }
    }
    pub fn lookup(&self) -> Request {
        Request::Lookup {
            epoch: self.epoch.clone(),
            operation_id: self.operation_id.clone(),
        }
    }
    /// Admit responses are accepted only from the selected authority endpoint.
    pub fn accepted_admission(&self, response: &Value) -> Result<AdmittedEffect, &'static str> {
        let (ticket, _) = response_ticket(response, &self.epoch, &self.operation_id, false)
            .ok_or("authority_denied")?;
        Ok(self.admitted(ticket))
    }
    /// A fenced lookup retains the ticket only for worker retirement and completion.
    pub fn accepted_lookup(&self, response: &Value) -> Result<LookupResult, &'static str> {
        let (ticket, fenced) = response_ticket(response, &self.epoch, &self.operation_id, true)
            .ok_or("authority_denied")?;
        let admitted = self.admitted(ticket);
        Ok(if fenced {
            LookupResult::Fenced(FencedEffect(admitted))
        } else {
            LookupResult::Active(admitted)
        })
    }
    fn admitted(&self, ticket: &str) -> AdmittedEffect {
        AdmittedEffect {
            epoch: self.epoch.clone(),
            operation_id: self.operation_id.clone(),
            ticket: ticket.into(),
        }
    }
}
pub enum LookupResult {
    Active(AdmittedEffect),
    Fenced(FencedEffect),
}
/// A fenced ticket is usable only after the effect and transport are retired.
pub struct FencedEffect(AdmittedEffect);
impl FencedEffect {
    pub fn complete_after_retirement(&self) -> Request {
        self.0.complete()
    }
    pub fn confirm_completion(&self, response: &Value) -> Result<CompletedEffect, &'static str> {
        self.0.confirm_completion(response)
    }
}
/// Ticket remains private and non-serializable; never persist it as cleanup data.
pub struct AdmittedEffect {
    epoch: String,
    operation_id: String,
    ticket: String,
}
impl AdmittedEffect {
    /// Submit only after the worker's final effect or send has actually retired.
    pub fn complete(&self) -> Request {
        Request::Complete {
            epoch: self.epoch.clone(),
            operation_id: self.operation_id.clone(),
            ticket: self.ticket.clone(),
        }
    }
    /// A completion response may be lost. Repeat complete with this live ticket;
    /// only confirmed completion permits an acknowledgement.
    pub fn confirm_completion(&self, response: &Value) -> Result<CompletedEffect, &'static str> {
        if !response_matches(response, &self.epoch, &self.operation_id) {
            return Err("completion_unconfirmed");
        }
        Ok(CompletedEffect(AdmittedEffect {
            epoch: self.epoch.clone(),
            operation_id: self.operation_id.clone(),
            ticket: self.ticket.clone(),
        }))
    }
}
pub struct CompletedEffect(AdmittedEffect);
impl CompletedEffect {
    pub fn acknowledge(&self) -> Request {
        Request::Acknowledge {
            epoch: self.0.epoch.clone(),
            operation_id: self.0.operation_id.clone(),
            ticket: self.0.ticket.clone(),
        }
    }
}

/// Evidence of a completed shared-authority recovery. The coordinator must obtain
/// these responses in order from the selected authenticated authority endpoint,
/// after retiring old workers and transport, and keep new admission closed until
/// the journal CAS commits. JSON alone is not an authentication mechanism.
pub struct ConfirmedEpochRotation {
    previous: String,
    current: String,
}
impl ConfirmedEpochRotation {
    pub fn from_authority_responses(
        previous: &str,
        old_status: &Value,
        recovery: &Value,
        new_status: &Value,
        new_owner_epoch: &str,
    ) -> Result<Self, &'static str> {
        let token = |value: &str| {
            !value.is_empty() && value.len() <= 128 && !value.chars().any(char::is_control)
        };
        let current = recovery
            .get("epoch")
            .and_then(Value::as_str)
            .ok_or("invalid_rotation")?;
        let status_clear = |v: &Value| {
            v.get("ok").and_then(Value::as_bool) == Some(true)
                && v.get("drain_required").and_then(Value::as_bool) == Some(false)
                && v.get("in_flight").and_then(Value::as_u64) == Some(0)
        };
        if !token(previous)
            || !token(current)
            || !token(new_owner_epoch)
            || previous == current
            || !status_clear(old_status)
            || !old_status
                .get("owner_epoch")
                .and_then(Value::as_str)
                .is_some_and(token)
            || old_status.get("epoch").and_then(Value::as_str) != Some(previous)
            || !old_status
                .get("fence_generation")
                .and_then(Value::as_u64)
                .is_some_and(|v| v > 0)
            || recovery.get("ok").and_then(Value::as_bool) != Some(true)
            || !status_clear(new_status)
            || new_status.get("epoch").and_then(Value::as_str) != Some(current)
            || new_status.get("owner_epoch").and_then(Value::as_str) != Some(new_owner_epoch)
            || new_status.get("fence_generation").and_then(Value::as_u64) != Some(0)
        {
            return Err("invalid_rotation");
        }
        Ok(Self {
            previous: previous.into(),
            current: current.into(),
        })
    }
    pub fn previous(&self) -> &str {
        &self.previous
    }
    pub fn current(&self) -> &str {
        &self.current
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn source() -> Source {
        Source {
            version: 1,
            id: "a".repeat(32),
            generation: 1,
            host_epoch: "host".into(),
            session_id: "session".into(),
            stream_id: "stream".into(),
            stream_generation: 1,
            device: "device".into(),
            device_generation: 1,
            effect_owner_epoch: "owner".into(),
        }
    }
    #[test]
    fn attestation_receipt_requires_exact_source_epoch_and_opaque_issuer() {
        let source = source();
        let mut response = json!({"ok":true,"version":1,"epoch":"epoch","source":source,"issuer_epoch":"b".repeat(32)});
        assert_eq!(
            AttestedProtocolSource::from_authority_response(&response, "epoch", &source)
                .unwrap()
                .issuer_epoch(),
            "b".repeat(32)
        );
        assert!(
            AttestedProtocolSource::from_authority_response(&response, "other", &source).is_err()
        );
        response["issuer_epoch"] = json!("not-an-issuer");
        assert!(
            AttestedProtocolSource::from_authority_response(&response, "epoch", &source).is_err()
        );
        response["issuer_epoch"] = json!("b".repeat(32));
        response["source"]["device_generation"] = json!(2);
        assert!(
            AttestedProtocolSource::from_authority_response(&response, "epoch", &source).is_err()
        );
    }
    #[test]
    fn tickets_are_caller_scoped_fenced_and_retained_through_completion() {
        let a = EffectOperation::new("epoch".into(), source(), "caller.a", "op", 1).unwrap();
        let b = EffectOperation::new("epoch".into(), source(), "caller.b", "op", 2).unwrap();
        let Request::Admit {
            operation_id: id, ..
        } = a.admit()
        else {
            panic!()
        };
        let Request::Admit {
            operation_id: other,
            ..
        } = b.admit()
        else {
            panic!()
        };
        assert_ne!(id, other);
        let mut result = json!({"ok":true,"epoch":"epoch","operation_id":id,"ticket":"b".repeat(32),"fenced":true});
        assert!(matches!(
            a.accepted_lookup(&result),
            Ok(LookupResult::Fenced(_))
        ));
        result["fenced"] = json!(false);
        let LookupResult::Active(ticket) = a.accepted_lookup(&result).unwrap() else {
            panic!()
        };
        assert!(ticket.confirm_completion(&json!({"ok":false})).is_err());
        assert!(matches!(ticket.complete(), Request::Complete { .. }));
        let done = ticket
            .confirm_completion(&json!({"ok":true,"epoch":"epoch","operation_id":id}))
            .unwrap();
        assert!(matches!(done.acknowledge(), Request::Acknowledge { .. }));
    }
}
