//! Additive sequenced limiter contracts. A range fence is a trusted executor
//! assertion that no reservation or start at or below its watermark can become
//! live, including requests delivered after the assertion. It is scoped to the
//! broker-attested caller, original initiator, executor incarnation and opaque
//! coordinator generation. It is not an application authorization grant.
//!
//! A provider must serialize reservations, starts, stops, and range fencing for
//! each scope. It must refuse a fence while an affected execution or reservation
//! is live. It must retain the watermark until its incarnation ends and reject
//! delayed sequenced requests below it. Unsequenced v1 requests remain separate.
//! It is exposed on a distinct `/2` service. Providers must keep v1 and v2
//! operation identities in separate execution namespaces even if both services
//! share a backend; v1 callers retain their entire opaque operation space.
use super::limiter_target;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
pub struct Position {
    pub generation: String,
    pub sequence: u64,
}

impl Position {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.sequence == 0
            || self.generation.is_empty()
            || self.generation.len() > 106
            || !self.generation.bytes().all(|byte| byte.is_ascii_graphic())
        {
            return Err("invalid_limiter_position");
        }
        Ok(())
    }

    /// Domain-separated identity for the new contract; v1 IDs remain opaque.
    pub fn operation_id(&self) -> String {
        format!("lim2.{}.{:016x}", self.generation, self.sequence)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub version: u8,
    pub position: Position,
    pub target: limiter_target::Request,
    pub original_initiator: String,
}

impl Request {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.version != 2
            || !super::v2::key(&self.original_initiator)
            || self.target.operation_id != self.position.operation_id()
        {
            return Err("invalid_sequenced_limiter_request");
        }
        self.position.validate()?;
        self.target.validate()
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Attestation {
    pub request: Request,
    pub executable_path: String,
}

impl Attestation {
    pub fn validate_for(&self, request: &Request) -> Result<(), &'static str> {
        request.validate()?;
        if &self.request != request {
            return Err("invalid_sequenced_limiter_attestation");
        }
        limiter_target::Attestation {
            request: request.target.clone(),
            executable_path: self.executable_path.clone(),
        }
        .validate_for(&request.target)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RetirementRequest {
    pub version: u8,
    pub position: Position,
    pub retirement: limiter_target::RetirementRequest,
}

impl RetirementRequest {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.version != 2 || self.retirement.operation_id != self.position.operation_id() {
            return Err("invalid_sequenced_retirement");
        }
        self.position.validate()?;
        self.retirement.validate()
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Retirement {
    pub request: RetirementRequest,
    pub retired: bool,
}

impl Retirement {
    pub fn validate_for(&self, request: &RetirementRequest) -> Result<(), &'static str> {
        request.validate()?;
        if !self.retired || &self.request != request {
            return Err("invalid_sequenced_retirement");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FenceRequest {
    pub version: u8,
    pub generation: String,
    pub through: u64,
    pub provider_incarnation: String,
    pub original_initiator: String,
}

impl FenceRequest {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.version != 2
            || self.through == 0
            || !super::v2::key(&self.original_initiator)
            || self.provider_incarnation.is_empty()
            || self.provider_incarnation.len() > 128
            || !self
                .provider_incarnation
                .bytes()
                .all(|byte| byte.is_ascii_graphic())
        {
            return Err("invalid_limiter_range_fence");
        }
        Position {
            generation: self.generation.clone(),
            sequence: self.through,
        }
        .validate()
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Fence {
    pub request: FenceRequest,
    pub fenced: bool,
}

impl Fence {
    pub fn validate_for(&self, request: &FenceRequest) -> Result<(), &'static str> {
        request.validate()?;
        if !self.fenced || &self.request != request {
            return Err("invalid_limiter_range_fence");
        }
        Ok(())
    }
}

/// Sequenced starts must use this path: a range fence cannot constrain a
/// delayed v1 start which does not carry its position.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleAction {
    Launch,
    Lookup,
    RecoverStop,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LifecycleRequest {
    pub version: u8,
    pub action: LifecycleAction,
    pub reservation: Request,
}

impl LifecycleRequest {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.version != 2 || self.reservation.target.stage != limiter_target::Stage::Reserve {
            return Err("invalid_sequenced_lifecycle");
        }
        self.reservation.validate()
    }

    pub fn provider_request(&self) -> super::provider_lifecycle::Request {
        use super::provider_lifecycle::Action;
        super::provider_lifecycle::Request {
            action: match self.action {
                LifecycleAction::Launch => Action::Launch,
                LifecycleAction::Lookup => Action::Lookup,
                LifecycleAction::RecoverStop => Action::RecoverStop,
            },
            operation_id: self.reservation.target.operation_id.clone(),
            application_id: self.reservation.target.application_id.clone(),
            provider_incarnation: self.reservation.target.provider_incarnation.clone(),
            ticket: None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LifecycleObservation {
    pub request: LifecycleRequest,
    pub observation: super::provider_lifecycle::Observation,
}

impl LifecycleObservation {
    pub fn validate_for(&self, request: &LifecycleRequest) -> Result<(), &'static str> {
        request.validate()?;
        if &self.request != request {
            return Err("invalid_sequenced_lifecycle_response");
        }
        self.observation.validate_for(&request.provider_request())
    }
}

/// In-memory bookkeeping helper for trusted executors. The executor must call
/// `retire` only after independently proving its execution has stopped, and
/// must persist or reconstruct this ledger whenever its incarnation survives a
/// process restart. All methods for a scope must be serialized with effects.
#[derive(Default)]
pub struct FenceLedger {
    scopes: BTreeMap<(String, String, String, String), Scope>,
}

#[derive(Default)]
struct Scope {
    through: u64,
    live: BTreeMap<u64, limiter_target::Request>,
    retired: BTreeMap<u64, limiter_target::Request>,
}

impl FenceLedger {
    fn scope<'a>(
        &'a mut self,
        caller: &str,
        owner: &str,
        incarnation: &str,
        generation: &str,
    ) -> Result<&'a mut Scope, &'static str> {
        let key = (
            caller.to_owned(),
            owner.to_owned(),
            incarnation.to_owned(),
            generation.to_owned(),
        );
        if !self.scopes.contains_key(&key) && self.scopes.len() >= 4096 {
            return Err("fence_capacity_exhausted");
        }
        Ok(self.scopes.entry(key).or_default())
    }

    pub fn reserve(
        &mut self,
        caller: &str,
        initiator: &str,
        request: &Request,
    ) -> Result<(), &'static str> {
        request.validate()?;
        if initiator != request.original_initiator
            || request.target.stage != limiter_target::Stage::Reserve
        {
            return Err("unauthorized");
        }
        let scope = self.scope(
            caller,
            initiator,
            &request.target.provider_incarnation,
            &request.position.generation,
        )?;
        let sequence = request.position.sequence;
        if sequence <= scope.through || scope.retired.contains_key(&sequence) {
            return Err("reservation_retired");
        }
        if scope
            .live
            .get(&sequence)
            .is_some_and(|previous| previous != &request.target)
            || scope
                .live
                .iter()
                .chain(scope.retired.iter())
                .any(|(other, value)| {
                    *other != sequence && value.operation_id == request.target.operation_id
                })
        {
            return Err("operation_conflict");
        }
        if !scope.live.contains_key(&sequence) && scope.live.len() + scope.retired.len() >= 4096 {
            return Err("capacity_exhausted");
        }
        scope.live.insert(sequence, request.target.clone());
        Ok(())
    }

    pub fn is_fenced(
        &mut self,
        caller: &str,
        owner: &str,
        incarnation: &str,
        position: &Position,
    ) -> Result<bool, &'static str> {
        position.validate()?;
        Ok(position.sequence
            <= self
                .scope(caller, owner, incarnation, &position.generation)?
                .through)
    }

    pub fn contains(&mut self, caller: &str, request: &Request) -> Result<bool, &'static str> {
        request.validate()?;
        let scope = self.scope(
            caller,
            &request.original_initiator,
            &request.target.provider_incarnation,
            &request.position.generation,
        )?;
        let mut reserved = request.target.clone();
        reserved.stage = limiter_target::Stage::Reserve;
        reserved.ticket = None;
        Ok(request.position.sequence > scope.through
            && scope.live.get(&request.position.sequence) == Some(&reserved))
    }

    pub fn retire(
        &mut self,
        caller: &str,
        request: &RetirementRequest,
    ) -> Result<(), &'static str> {
        request.validate()?;
        let value = &request.retirement;
        let scope = self.scope(
            caller,
            &value.original_initiator,
            &value.provider_incarnation,
            &request.position.generation,
        )?;
        let sequence = request.position.sequence;
        // A range watermark does not authenticate the per-operation claims.
        // The coordinator must use its separately authenticated receipt.
        if sequence <= scope.through {
            return Err("sequence_fenced");
        }
        let expected = limiter_target::Request {
            version: 1,
            stage: limiter_target::Stage::Reserve,
            operation_id: value.operation_id.clone(),
            application_id: value.application_id.clone(),
            provider_incarnation: value.provider_incarnation.clone(),
            ticket: None,
            policy_identity: value.policy_identity.clone(),
        };
        if scope
            .live
            .get(&sequence)
            .is_some_and(|current| current != &expected)
            || scope
                .retired
                .get(&sequence)
                .is_some_and(|current| current != &expected)
        {
            return Err("operation_conflict");
        }
        if !scope.retired.contains_key(&sequence)
            && !scope.live.contains_key(&sequence)
            && scope.live.len() + scope.retired.len() >= 4096
        {
            return Err("capacity_exhausted");
        }
        scope.live.remove(&sequence);
        scope.retired.insert(sequence, expected);
        Ok(())
    }

    pub fn fence(&mut self, caller: &str, request: &FenceRequest) -> Result<Fence, &'static str> {
        request.validate()?;
        let scope = self.scope(
            caller,
            &request.original_initiator,
            &request.provider_incarnation,
            &request.generation,
        )?;
        if scope
            .live
            .keys()
            .any(|sequence| *sequence <= request.through)
        {
            return Err("execution_not_retired");
        }
        scope.through = scope.through.max(request.through);
        scope
            .retired
            .retain(|sequence, _| *sequence > scope.through);
        Ok(Fence {
            request: request.clone(),
            fenced: true,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application_library::{preflight::PolicyIdentity, v2::ApplicationReference};

    fn request(sequence: u64) -> Request {
        Request {
            version: 2,
            position: Position {
                generation: "generation".into(),
                sequence,
            },
            original_initiator: "nvhttp".into(),
            target: limiter_target::Request {
                version: 1,
                stage: limiter_target::Stage::Reserve,
                operation_id: format!("lim2.generation.{sequence:016x}"),
                application_id: "game".into(),
                provider_incarnation: "incarnation".into(),
                ticket: None,
                policy_identity: PolicyIdentity::Library {
                    version: 1,
                    application: ApplicationReference {
                        library: "library".into(),
                        id: "opaque".into(),
                    },
                    entry_revision: "opaque-entry".into(),
                    policy_revision: "opaque-policy".into(),
                    policy_source: Some("authority".into()),
                    snapshot_id: "snapshot".into(),
                },
            },
        }
    }

    fn retirement(request: &Request) -> RetirementRequest {
        RetirementRequest {
            version: 2,
            position: request.position.clone(),
            retirement: limiter_target::RetirementRequest {
                version: 1,
                operation_id: request.target.operation_id.clone(),
                application_id: request.target.application_id.clone(),
                provider_incarnation: request.target.provider_incarnation.clone(),
                original_initiator: request.original_initiator.clone(),
                policy_identity: request.target.policy_identity.clone(),
            },
        }
    }

    #[test]
    fn range_fences_compact_retired_sequences_and_block_delayed_effects() {
        let mut ledger = FenceLedger::default();
        for sequence in 1..=5000 {
            let request = request(sequence);
            ledger.reserve("launch", "nvhttp", &request).unwrap();
            let fence = FenceRequest {
                version: 2,
                generation: "generation".into(),
                through: sequence,
                provider_incarnation: "incarnation".into(),
                original_initiator: "nvhttp".into(),
            };
            assert!(ledger.fence("launch", &fence).is_err());
            ledger.retire("launch", &retirement(&request)).unwrap();
            ledger
                .fence("launch", &fence)
                .unwrap()
                .validate_for(&fence)
                .unwrap();
            assert!(ledger.reserve("launch", "nvhttp", &request).is_err());
            assert!(!ledger.contains("launch", &request).unwrap());
        }
        assert!(ledger.reserve("launch", "foreign", &request(5001)).is_err());
        let request = request(5001);
        ledger.reserve("launch", "nvhttp", &request).unwrap();
        let mut changed = retirement(&request);
        changed.retirement.application_id = "other".into();
        assert!(ledger.retire("launch", &changed).is_err());
    }
}
