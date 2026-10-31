//! Provider lifecycle contract for the launch coordinator.
//!
//! The provider must keep its incarnation stable for the lifetime of each
//! operation, reject stale incarnations before effects, deduplicate launch IDs,
//! and fence delayed starts before asserting a terminal proof. A proof is a
//! trusted provider assertion, not a host-enforced sandbox or cryptographic proof.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Launch,
    Lookup,
    RecoverStop,
    Stop,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub action: Action,
    pub operation_id: String,
    pub application_id: String,
    pub provider_incarnation: String,
    pub ticket: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Starting,
    Running,
    Stopping,
    Stopped,
    Unknown,
    LaunchFailed,
    CleanupFailed,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RetirementProof {
    pub kind: ProofKind,
    pub operation_id: String,
    pub application_id: String,
    pub provider_incarnation: String,
    pub ticket: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProofKind {
    FencedStopped,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Observation {
    pub operation_id: String,
    pub application_id: String,
    pub provider_incarnation: String,
    pub ticket: Option<String>,
    pub state: State,
    pub retirement_proof: Option<RetirementProof>,
}

fn valid_token(value: &str) -> bool {
    !value.is_empty() && value.len() <= 128 && value.bytes().all(|byte| byte.is_ascii_graphic())
}

impl Request {
    pub fn validate(&self) -> Result<(), &'static str> {
        if !valid_token(&self.operation_id)
            || !valid_token(&self.provider_incarnation)
            || self.application_id.trim().is_empty()
            || self.application_id.len() > 96
            || self.application_id.chars().any(char::is_control)
            || self
                .ticket
                .as_deref()
                .is_some_and(|ticket| !valid_token(ticket))
            || (self.action == Action::Stop) != self.ticket.is_some()
        {
            return Err("invalid lifecycle request");
        }
        Ok(())
    }
}

impl Observation {
    pub fn validate_for(&self, request: &Request) -> Result<(), &'static str> {
        request.validate()?;
        if self.operation_id != request.operation_id
            || self.application_id != request.application_id
            || self.provider_incarnation != request.provider_incarnation
            || self
                .ticket
                .as_deref()
                .is_some_and(|ticket| !valid_token(ticket))
            || request
                .ticket
                .as_ref()
                .is_some_and(|ticket| self.ticket.as_ref() != Some(ticket))
        {
            return Err("lifecycle identity mismatch");
        }
        if matches!(
            self.state,
            State::Starting | State::Running | State::Stopping
        ) && self.ticket.is_none()
        {
            return Err("missing lifecycle ticket");
        }
        if matches!(self.state, State::Stopped | State::LaunchFailed) {
            let Some(proof) = &self.retirement_proof else {
                return Err("missing retirement proof");
            };
            if proof.kind != ProofKind::FencedStopped
                || proof.operation_id != self.operation_id
                || proof.application_id != self.application_id
                || proof.provider_incarnation != self.provider_incarnation
                || proof.ticket != self.ticket
            {
                return Err("retirement proof mismatch");
            }
        } else if self.retirement_proof.is_some() {
            return Err("unexpected retirement proof");
        }
        Ok(())
    }
}

pub(crate) fn valid_incarnation(value: &str) -> bool {
    valid_token(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn terminal_proof_must_match_every_bound_identity_and_ticket() {
        let request = Request {
            action: Action::Stop,
            operation_id: "op".into(),
            application_id: "app".into(),
            provider_incarnation: "epoch".into(),
            ticket: Some("ticket".into()),
        };
        let proof = RetirementProof {
            kind: ProofKind::FencedStopped,
            operation_id: "op".into(),
            application_id: "app".into(),
            provider_incarnation: "epoch".into(),
            ticket: Some("ticket".into()),
        };
        let mut result = Observation {
            operation_id: "op".into(),
            application_id: "app".into(),
            provider_incarnation: "epoch".into(),
            ticket: Some("ticket".into()),
            state: State::Stopped,
            retirement_proof: Some(proof),
        };
        assert!(result.validate_for(&request).is_ok());
        result
            .retirement_proof
            .as_mut()
            .unwrap()
            .provider_incarnation = "new".into();
        assert!(result.validate_for(&request).is_err());
        result.retirement_proof = None;
        assert!(result.validate_for(&request).is_err());
        result.state = State::Unknown;
        assert!(result.validate_for(&request).is_ok());
        result.ticket = None;
        assert!(result.validate_for(&request).is_err());
    }
}
