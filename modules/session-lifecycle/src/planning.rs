//! Read-only effect plans and exact consumer binding checks.
//!
//! The caller must acquire the returned rights in order and revalidate against a
//! fresh snapshot while holding them, immediately before any native effect.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

/// A reference identifies a resource; it does not grant mutation authority.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceRef {
    pub provider: String,
    pub resource_id: String,
    pub incarnation: String,
    pub revision: u64,
    pub execution_domain: String,
    pub mutation_domains: BTreeSet<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Access {
    Read,
    Shared,
    Exclusive,
}

/// All borrowers of a mutation domain, not just borrowers of the named output.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DomainState {
    pub revision: u64,
    pub borrowers: BTreeMap<String, u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanningSnapshot {
    pub resources: BTreeMap<String, BTreeMap<String, ResourceRef>>,
    pub domains: BTreeMap<String, DomainState>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DomainEffect {
    pub domain: String,
    pub expected: DomainState,
    pub access: Access,
}

/// An approved plan is compared with a current authoritative snapshot twice:
/// at admission and again after acquiring its rights at the effect boundary.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectPlan {
    pub resources: Vec<ResourceRef>,
    pub domains: Vec<DomainEffect>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlanningError {
    DuplicateResource,
    DuplicateDomain,
    MissingDomain,
    StaleResource,
    StaleDomain,
    EmptyIdentity,
    DuplicateConsumer,
    UnexpectedConsumer,
    MissingConsumer,
    BindingMismatch,
}

impl EffectPlan {
    /// Validate the entire affected borrower set and return a deterministic
    /// domain order for lock acquisition. Authority is checked by the caller.
    pub fn validate(
        &self,
        current: &PlanningSnapshot,
    ) -> Result<Vec<(String, Access)>, PlanningError> {
        let mut resources = BTreeSet::new();
        let mut domains = BTreeMap::new();
        for effect in &self.domains {
            if effect.domain.is_empty() || effect.expected.borrowers.keys().any(String::is_empty) {
                return Err(PlanningError::EmptyIdentity);
            }
            if domains
                .insert(effect.domain.clone(), effect.access)
                .is_some()
            {
                return Err(PlanningError::DuplicateDomain);
            }
            if current.domains.get(&effect.domain) != Some(&effect.expected) {
                return Err(PlanningError::StaleDomain);
            }
        }
        for resource in &self.resources {
            if resource.provider.is_empty()
                || resource.resource_id.is_empty()
                || resource.incarnation.is_empty()
                || resource.execution_domain.is_empty()
                || resource.mutation_domains.iter().any(String::is_empty)
            {
                return Err(PlanningError::EmptyIdentity);
            }
            if !resources.insert((resource.provider.clone(), resource.resource_id.clone())) {
                return Err(PlanningError::DuplicateResource);
            }
            if current
                .resources
                .get(&resource.provider)
                .and_then(|provider| provider.get(&resource.resource_id))
                != Some(resource)
            {
                return Err(PlanningError::StaleResource);
            }
            if resource
                .mutation_domains
                .iter()
                .any(|domain| !domains.contains_key(domain))
            {
                return Err(PlanningError::MissingDomain);
            }
        }
        Ok(domains.into_iter().collect())
    }
}

/// The geometry revision identifies the authoritative capture/input mapping;
/// its contents are provider-defined and carried by the owning presentation plan.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExactBinding {
    pub provider: String,
    pub selector_schema: String,
    pub resource_id: String,
    pub incarnation: String,
    pub execution_domain: String,
    pub effective_mode: String,
    pub geometry_revision: u64,
    pub plan_revision: u64,
    pub generation: u64,
}

/// An unresolved native incarnation or presentation may be refined only within
/// the approved provider, logical resource and execution domain.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BindingConstraint {
    pub provider: String,
    pub selector_schema: String,
    pub resource_id: String,
    pub incarnation: Option<String>,
    pub execution_domain: String,
    pub effective_mode: Option<String>,
    pub geometry_revision: Option<u64>,
    pub plan_revision: u64,
    pub generation: u64,
}

impl BindingConstraint {
    pub fn validate_candidate(&self, candidate: &ExactBinding) -> Result<(), PlanningError> {
        if [
            &self.provider,
            &self.selector_schema,
            &self.resource_id,
            &self.execution_domain,
            &candidate.provider,
            &candidate.selector_schema,
            &candidate.resource_id,
            &candidate.incarnation,
            &candidate.execution_domain,
            &candidate.effective_mode,
        ]
        .iter()
        .any(|value| value.is_empty())
            || self.incarnation.as_ref().is_some_and(String::is_empty)
            || self.effective_mode.as_ref().is_some_and(String::is_empty)
        {
            return Err(PlanningError::EmptyIdentity);
        }
        if self.provider != candidate.provider
            || self.selector_schema != candidate.selector_schema
            || self.resource_id != candidate.resource_id
            || self.execution_domain != candidate.execution_domain
            || self
                .incarnation
                .as_ref()
                .is_some_and(|value| value != &candidate.incarnation)
            || self
                .effective_mode
                .as_ref()
                .is_some_and(|value| value != &candidate.effective_mode)
            || self
                .geometry_revision
                .is_some_and(|value| value != candidate.geometry_revision)
            || self.plan_revision != candidate.plan_revision
            || self.generation != candidate.generation
        {
            return Err(PlanningError::BindingMismatch);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsumerAcknowledgement {
    pub consumer_id: String,
    pub binding: ExactBinding,
}

/// A provider-ready candidate is not committed until every admitted consumer
/// acknowledges the exact binding and generation. No fallback is inferred.
pub fn validate_acknowledgements(
    binding: &ExactBinding,
    expected_consumers: &[String],
    acknowledgements: &[ConsumerAcknowledgement],
) -> Result<(), PlanningError> {
    if [
        &binding.provider,
        &binding.selector_schema,
        &binding.resource_id,
        &binding.incarnation,
        &binding.execution_domain,
        &binding.effective_mode,
    ]
    .iter()
    .any(|value| value.is_empty())
    {
        return Err(PlanningError::EmptyIdentity);
    }
    let mut expected = BTreeSet::new();
    for consumer in expected_consumers {
        if consumer.is_empty() {
            return Err(PlanningError::EmptyIdentity);
        }
        if !expected.insert(consumer) {
            return Err(PlanningError::DuplicateConsumer);
        }
    }
    let mut seen = BTreeSet::new();
    for ack in acknowledgements {
        if !expected.contains(&ack.consumer_id) {
            return Err(PlanningError::UnexpectedConsumer);
        }
        if !seen.insert(&ack.consumer_id) {
            return Err(PlanningError::DuplicateConsumer);
        }
        if ack.binding != *binding {
            return Err(PlanningError::BindingMismatch);
        }
    }
    if seen != expected {
        return Err(PlanningError::MissingConsumer);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (EffectPlan, PlanningSnapshot) {
        let resource = ResourceRef {
            provider: "display".into(),
            resource_id: "output".into(),
            incarnation: "native-1".into(),
            revision: 4,
            execution_domain: "seat".into(),
            mutation_domains: ["topology".into()].into(),
        };
        let domain = DomainState {
            revision: 3,
            borrowers: [("stream-a".into(), 8), ("stream-b".into(), 2)].into(),
        };
        let plan = EffectPlan {
            resources: vec![resource.clone()],
            domains: vec![DomainEffect {
                domain: "topology".into(),
                expected: domain.clone(),
                access: Access::Exclusive,
            }],
        };
        let snapshot = PlanningSnapshot {
            resources: [("display".into(), [("output".into(), resource)].into())].into(),
            domains: [("topology".into(), domain)].into(),
        };
        (plan, snapshot)
    }

    #[test]
    fn detects_shared_borrower_and_resource_changes_at_effect_boundary() {
        let (plan, mut snapshot) = fixture();
        assert_eq!(
            plan.validate(&snapshot),
            Ok(vec![("topology".into(), Access::Exclusive)])
        );
        snapshot
            .domains
            .get_mut("topology")
            .unwrap()
            .borrowers
            .insert("stream-c".into(), 1);
        assert_eq!(plan.validate(&snapshot), Err(PlanningError::StaleDomain));
        snapshot = fixture().1;
        snapshot
            .domains
            .get_mut("topology")
            .unwrap()
            .borrowers
            .insert("stream-b".into(), 3);
        assert_eq!(plan.validate(&snapshot), Err(PlanningError::StaleDomain));
        snapshot = fixture().1;
        snapshot
            .resources
            .get_mut("display")
            .unwrap()
            .get_mut("output")
            .unwrap()
            .incarnation = "native-2".into();
        assert_eq!(plan.validate(&snapshot), Err(PlanningError::StaleResource));
    }

    #[test]
    fn requires_all_mutation_domains_and_orders_rights() {
        let (mut plan, mut snapshot) = fixture();
        plan.domains.clear();
        assert_eq!(plan.validate(&snapshot), Err(PlanningError::MissingDomain));
        (plan, snapshot) = fixture();
        snapshot.domains.insert(
            "alpha".into(),
            DomainState {
                revision: 1,
                borrowers: BTreeMap::new(),
            },
        );
        plan.domains.push(DomainEffect {
            domain: "alpha".into(),
            expected: snapshot.domains["alpha"].clone(),
            access: Access::Shared,
        });
        assert_eq!(plan.validate(&snapshot).unwrap()[0].0, "alpha");
        plan.domains.push(plan.domains[0].clone());
        assert_eq!(
            plan.validate(&snapshot),
            Err(PlanningError::DuplicateDomain)
        );
    }

    #[test]
    fn every_consumer_must_acknowledge_the_exact_candidate_and_fence() {
        let binding = ExactBinding {
            provider: "capture".into(),
            selector_schema: "v1".into(),
            resource_id: "output".into(),
            incarnation: "native-1".into(),
            execution_domain: "seat".into(),
            effective_mode: "mode".into(),
            geometry_revision: 2,
            plan_revision: 4,
            generation: 7,
        };
        let constraint = BindingConstraint {
            provider: binding.provider.clone(),
            selector_schema: binding.selector_schema.clone(),
            resource_id: binding.resource_id.clone(),
            incarnation: None,
            execution_domain: binding.execution_domain.clone(),
            effective_mode: None,
            geometry_revision: None,
            plan_revision: binding.plan_revision,
            generation: binding.generation,
        };
        assert_eq!(constraint.validate_candidate(&binding), Ok(()));
        let mut wrong_resource = binding.clone();
        wrong_resource.resource_id = "other-output".into();
        assert_eq!(
            constraint.validate_candidate(&wrong_resource),
            Err(PlanningError::BindingMismatch)
        );
        let consumers: Vec<String> = vec!["capture".into(), "input".into()];
        let mut acks: Vec<_> = consumers
            .iter()
            .map(|id| ConsumerAcknowledgement {
                consumer_id: id.clone(),
                binding: binding.clone(),
            })
            .collect();
        assert_eq!(
            validate_acknowledgements(&binding, &consumers, &acks),
            Ok(())
        );
        acks[1].binding.geometry_revision += 1;
        assert_eq!(
            validate_acknowledgements(&binding, &consumers, &acks),
            Err(PlanningError::BindingMismatch)
        );
        acks[1].binding = binding.clone();
        acks[1].binding.generation -= 1;
        assert_eq!(
            validate_acknowledgements(&binding, &consumers, &acks),
            Err(PlanningError::BindingMismatch)
        );
        acks.pop();
        assert_eq!(
            validate_acknowledgements(&binding, &consumers, &acks),
            Err(PlanningError::MissingConsumer)
        );
        acks.push(acks[0].clone());
        assert_eq!(
            validate_acknowledgements(&binding, &consumers, &acks),
            Err(PlanningError::DuplicateConsumer)
        );
    }
}
