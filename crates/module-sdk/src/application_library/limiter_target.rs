//! Versioned executor assertion for a selected streaming limiter target.
//!
//! A reservation predicts the target for one exact launch operation. Before
//! activation, the same provider incarnation must confirm it for its running
//! ticket. The provider must keep that target bound to the execution until the
//! ticket is retired; providers unable to guarantee this must not advertise the
//! capability. A provider must bind reservations to both broker-supplied caller
//! and initiator; these identities are not taken from the payload. A Verify request
//! is a coordinator-self-initiated recheck of the already reserved identity and
//! running ticket; providers must permit it only for that same coordinator. This
//! assertion is not a grant.
//!
//! Executors advertise `limiter_target_capabilities_v1` and implement
//! `attest_limiter_target_v1` through the provider adapter. The initial
//! reservation and running confirmation retain the authenticated transport
//! initiator. The later Verify call is self-initiated by the same trusted
//! coordinator, and must match the previously reserved identity and ticket.
use super::preflight::PolicyIdentity;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Reserve,
    Running,
    /// Coordinator-owned recheck of the running ticket before limiter use.
    Verify,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub version: u8,
    pub stage: Stage,
    pub operation_id: String,
    pub application_id: String,
    pub provider_incarnation: String,
    pub ticket: Option<String>,
    pub policy_identity: PolicyIdentity,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Attestation {
    pub request: Request,
    pub executable_path: String,
}
fn valid_token(value: &str) -> bool {
    !value.is_empty() && value.len() <= 128 && value.bytes().all(|byte| byte.is_ascii_graphic())
}
impl Request {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.version != 1
            || !valid_token(&self.operation_id)
            || !valid_token(&self.provider_incarnation)
            || self.application_id.trim().is_empty()
            || self.application_id.len() > 96
            || self.application_id.chars().any(char::is_control)
            || self.policy_identity.validate().is_err()
            || !matches!(
                self.policy_identity,
                PolicyIdentity::Library {
                    policy_source: Some(_),
                    ..
                }
            )
            || match (&self.stage, &self.ticket) {
                (Stage::Reserve, None) => false,
                (Stage::Running | Stage::Verify, Some(ticket)) => !valid_token(ticket),
                _ => true,
            }
        {
            return Err("invalid_limiter_target_request");
        }
        Ok(())
    }
}
impl Attestation {
    pub fn validate_for(&self, request: &Request) -> Result<(), &'static str> {
        request.validate()?;
        if &self.request != request
            || self.executable_path.trim().is_empty()
            || self.executable_path.len() > 4096
            || self.executable_path.chars().any(char::is_control)
            || !std::path::Path::new(&self.executable_path).is_absolute()
        {
            return Err("invalid_limiter_target_attestation");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application_library::{preflight::PolicyIdentity, v2::ApplicationReference};
    fn request() -> Request {
        Request {
            version: 1,
            stage: Stage::Reserve,
            operation_id: "operation".into(),
            application_id: "target".into(),
            provider_incarnation: "incarnation".into(),
            ticket: None,
            policy_identity: PolicyIdentity::Library {
                version: 1,
                application: ApplicationReference {
                    library: "games".into(),
                    id: "opaque".into(),
                },
                entry_revision: "entry".into(),
                policy_revision: "policy".into(),
                policy_source: Some("source".into()),
                snapshot_id: "snapshot".into(),
            },
        }
    }
    #[test]
    fn assertion_is_bound_to_stage_owner_execution_and_policy() {
        let original = request();
        let assertion = Attestation {
            request: original.clone(),
            executable_path: std::env::current_exe().unwrap().to_string_lossy().into(),
        };
        assert!(assertion.validate_for(&original).is_ok());
        let mut altered = original.clone();
        altered.provider_incarnation = "other".into();
        assert!(assertion.validate_for(&altered).is_err());
        let mut altered = original.clone();
        altered.operation_id = "other".into();
        assert!(assertion.validate_for(&altered).is_err());
        let mut altered = original.clone();
        altered.stage = Stage::Running;
        assert!(altered.validate().is_err());
        altered.ticket = Some("ticket".into());
        assert!(altered.validate().is_ok());
        assert!(assertion.validate_for(&altered).is_err());
        let mut altered = assertion.clone();
        altered.executable_path = "relative".into();
        assert!(altered.validate_for(&original).is_err());
    }
}
