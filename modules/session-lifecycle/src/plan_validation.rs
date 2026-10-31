//! Validation bridge from public provider plans to internal planning primitives.
//!
//! The caller supplies authoritative, complete observations, checks installed
//! capability/method grants, acquires returned rights, and repeats this check
//! under those rights immediately before effects. This module does not observe
//! native resources or authenticate provider and consumer reports.

use std::collections::{BTreeMap, BTreeSet};

use crate::planning::{self, Access, DomainEffect, DomainState, EffectPlan, PlanningSnapshot};
use amaterasu_extension_api::{self as api, ResourceAccess};

/// Caller-authorized policy and a complete current observation. The caller must
/// obtain these from trusted authorities, not from the provider's plan.
pub struct AdmissionContext<'a> {
    pub snapshot: &'a PlanningSnapshot,
    pub complete: bool,
    pub approved_capabilities: &'a BTreeSet<String>,
    pub approved_cleanup_methods: &'a [api::MethodRef],
    pub authorized_borrowers: &'a BTreeSet<String>,
}

fn access(value: &ResourceAccess) -> Access {
    match value {
        ResourceAccess::Read => Access::Read,
        ResourceAccess::Shared => Access::Shared,
        ResourceAccess::Exclusive => Access::Exclusive,
    }
}
fn level(value: Access) -> u8 {
    match value {
        Access::Read => 0,
        Access::Shared => 1,
        Access::Exclusive => 2,
    }
}
fn unique_valid(values: &[String]) -> bool {
    let mut seen = BTreeSet::new();
    values.iter().all(|value| {
        !value.is_empty()
            && value.len() <= 256
            && !value.chars().any(char::is_control)
            && seen.insert(value)
    })
}

/// Validate provider plan shape, authority prerequisites, affected domain rights,
/// complete borrower revisions, resources and cleanup dependencies. The result
/// is a deterministic lock order, not an authorization or a native effect.
pub fn validate_effect_plan(
    plan: &api::ResourcePlan,
    context: &AdmissionContext<'_>,
) -> Result<Vec<(String, Access)>, String> {
    api::validate_resource_plan(plan)?;
    if !context.complete {
        return Err("incomplete observation".into());
    }
    if !unique_valid(&plan.required_capabilities)
        || !unique_valid(&plan.affected_consumers)
        || !unique_valid(&plan.allowed_degradation)
        || plan
            .required_capabilities
            .iter()
            .any(|id| !context.approved_capabilities.contains(id))
    {
        return Err("invalid or unapproved plan declarations".into());
    }
    let domains: BTreeMap<_, _> = plan
        .mutation_domains
        .iter()
        .map(|domain| (domain.domain.as_str(), access(&domain.access)))
        .collect();
    let mut existing = Vec::new();
    for resource in &plan.resources {
        if !unique_valid(&resource.mutation_domains) || resource.mutation_domains.len() > 128 {
            return Err("invalid resource domains".into());
        }
        for domain in &resource.mutation_domains {
            if domains
                .get(domain.as_str())
                .is_none_or(|right| level(*right) < level(access(&resource.access)))
            {
                return Err("insufficient domain access".into());
            }
        }
        let observed = context
            .snapshot
            .resources
            .get(&resource.provider)
            .and_then(|by_id| by_id.get(&resource.resource_id));
        match (&resource.incarnation, resource.expected_revision, observed) {
            (Some(incarnation), Some(revision), Some(_)) => existing.push(planning::ResourceRef {
                provider: resource.provider.clone(),
                resource_id: resource.resource_id.clone(),
                incarnation: incarnation.clone(),
                revision,
                execution_domain: resource.execution_domain.clone(),
                mutation_domains: resource.mutation_domains.iter().cloned().collect(),
            }),
            (None, None, None) => {}
            _ => return Err("stale or unresolved resource".into()),
        }
        if access(&resource.access) != Access::Read
            && !plan.obligations.iter().any(|obligation| {
                obligation.provider == resource.provider
                    && obligation.resource_id == resource.resource_id
                    && obligation
                        .mutation_domain
                        .as_ref()
                        .is_none_or(|domain| resource.mutation_domains.contains(domain))
            })
        {
            return Err("mutation lacks cleanup obligation".into());
        }
    }
    validate_obligations(plan, context.approved_cleanup_methods)?;
    for domain in &plan.mutation_domains {
        if access(&domain.access) != Access::Read
            && !plan.obligations.iter().any(|obligation| {
                plan.resources.iter().any(|resource| {
                    resource.provider == obligation.provider
                        && resource.resource_id == obligation.resource_id
                        && resource.mutation_domains.contains(&domain.domain)
                })
            })
        {
            return Err("domain mutation lacks cleanup obligation".into());
        }
    }
    let effects = plan
        .mutation_domains
        .iter()
        .map(|domain| DomainEffect {
            domain: domain.domain.clone(),
            expected: DomainState {
                revision: domain.expected_revision,
                borrowers: domain.borrowers.clone(),
            },
            access: access(&domain.access),
        })
        .collect();
    for domain in &plan.mutation_domains {
        if access(&domain.access) != Access::Read
            && domain
                .borrowers
                .keys()
                .any(|id| !context.authorized_borrowers.contains(id))
        {
            return Err("borrower not authorized".into());
        }
    }
    EffectPlan {
        resources: existing,
        domains: effects,
    }
    .validate(context.snapshot)
    .map_err(|error| format!("stale or invalid effect plan: {error:?}"))
}

fn validate_obligations(
    plan: &api::ResourcePlan,
    approved: &[api::MethodRef],
) -> Result<(), String> {
    let by_id: BTreeMap<_, _> = plan
        .obligations
        .iter()
        .map(|obligation| (obligation.obligation_id.as_str(), obligation))
        .collect();
    for obligation in &plan.obligations {
        if !approved.contains(&obligation.cleanup_method)
            || !unique_valid(&obligation.depends_on)
            || !plan.resources.iter().any(|resource| {
                resource.provider == obligation.provider
                    && resource.resource_id == obligation.resource_id
            })
            || obligation.mutation_domain.as_ref().is_some_and(|domain| {
                !plan
                    .mutation_domains
                    .iter()
                    .any(|entry| &entry.domain == domain)
            })
        {
            return Err("invalid or unapproved cleanup obligation".into());
        }
        let mut pending = vec![obligation.obligation_id.as_str()];
        let mut visited = BTreeSet::new();
        while let Some(id) = pending.pop() {
            if !visited.insert(id) {
                continue;
            }
            for dependency in &by_id[id].depends_on {
                if dependency == &obligation.obligation_id {
                    return Err("cyclic cleanup obligations".into());
                }
                pending.push(dependency);
            }
        }
    }
    Ok(())
}

/// Check a provider-ready candidate against the approved public constraint.
/// The caller must separately validate its selector under the approved schema,
/// compare independent consumer observations, and validate the selector schema.
pub fn validate_candidate(
    plan: &api::ResourcePlan,
    candidate: &api::CaptureBinding,
    expected_generation: u64,
    expected_geometry_revision: u64,
) -> Result<(), String> {
    api::validate_resource_plan(plan)?;
    api::validate_capture_binding(candidate)?;
    let constraint = plan
        .candidate_constraint
        .as_ref()
        .ok_or("missing binding constraint")?;
    let resource = &candidate.resource;
    let planned = plan
        .resources
        .iter()
        .find(|entry| {
            entry.provider == resource.provider && entry.resource_id == resource.resource_id
        })
        .ok_or("candidate resource was not planned")?;
    if constraint.capture_provider != candidate.capture_provider
        || constraint.selector_schema != candidate.selector_schema
        || constraint.resource_provider != resource.provider
        || constraint.resource_id != resource.resource_id
        || constraint.execution_domain != resource.execution_domain
        || constraint.plan_revision != plan.revision
        || candidate.plan_revision != plan.revision
        || constraint
            .incarnation
            .as_ref()
            .is_some_and(|incarnation| incarnation != &resource.incarnation)
        || constraint
            .mode
            .as_ref()
            .is_some_and(|mode| mode != &candidate.mode)
        || planned.execution_domain != resource.execution_domain
        || planned
            .incarnation
            .as_ref()
            .is_some_and(|incarnation| incarnation != &resource.incarnation)
        || planned.expected_revision.is_some_and(|revision| {
            if planned.access == ResourceAccess::Read {
                resource.revision != revision
            } else {
                resource.revision < revision
            }
        })
        || resource
            .mutation_domain
            .as_ref()
            .is_some_and(|domain| !planned.mutation_domains.contains(domain))
        || !unique_valid(&resource.mutation_domains)
        || resource.mutation_domains.iter().collect::<BTreeSet<_>>()
            != planned.mutation_domains.iter().collect::<BTreeSet<_>>()
        || candidate.generation != expected_generation
        || candidate.geometry_revision != expected_geometry_revision
        || constraint
            .geometry_revision
            .is_some_and(|revision| revision != candidate.geometry_revision)
    {
        return Err("candidate violates approved binding".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture() -> (api::ResourcePlan, PlanningSnapshot, Vec<api::MethodRef>) {
        let plan: api::ResourcePlan = serde_json::from_value(json!({
            "plan_id":"plan", "revision":2,
            "resources":[{"provider":"display", "resource_id":"output", "incarnation":"native", "expected_revision":4, "execution_domain":"seat", "mutation_domains":["topology"], "access":"exclusive"}],
            "mutation_domains":[{"domain":"topology", "expected_revision":3, "borrowers":{"stream-a":1,"stream-b":2}, "access":"exclusive"}],
            "required_capabilities":["display/change"], "constraints":{},
            "obligations":[{"obligation_id":"restore", "provider":"display", "resource_id":"output", "mutation_domain":"topology", "depends_on":[], "cleanup_method":{"service":"display/1", "method":"restore", "interface_major":1, "interface_minor":0, "schema_revision":1}, "payload":{}}],
            "affected_consumers":["capture"], "consumer_change":"restart_pipeline",
            "candidate_constraint":{"capture_provider":"capture", "selector_schema":"selector/1", "resource_provider":"display", "resource_id":"output", "incarnation":"native", "execution_domain":"seat", "mode":{"width":1920,"height":1080,"refresh_millihz":60000,"hdr":false}, "plan_revision":2},
            "allowed_degradation":[]
        })).unwrap();
        let reference = planning::ResourceRef {
            provider: "display".into(),
            resource_id: "output".into(),
            incarnation: "native".into(),
            revision: 4,
            execution_domain: "seat".into(),
            mutation_domains: ["topology".into()].into(),
        };
        let snapshot = PlanningSnapshot {
            resources: [("display".into(), [("output".into(), reference)].into())].into(),
            domains: [(
                "topology".into(),
                DomainState {
                    revision: 3,
                    borrowers: [("stream-a".into(), 1), ("stream-b".into(), 2)].into(),
                },
            )]
            .into(),
        };
        let methods = vec![plan.obligations[0].cleanup_method.clone()];
        (plan, snapshot, methods)
    }

    #[test]
    fn admits_only_fresh_complete_authorized_effects_with_cleanup() {
        let (mut plan, mut snapshot, methods) = fixture();
        let capabilities = ["display/change".into()].into();
        let borrowers = ["stream-a".into(), "stream-b".into()].into();
        macro_rules! context {
            () => {
                AdmissionContext {
                    snapshot: &snapshot,
                    complete: true,
                    approved_capabilities: &capabilities,
                    approved_cleanup_methods: &methods,
                    authorized_borrowers: &borrowers,
                }
            };
        }
        assert!(validate_effect_plan(&plan, &context!()).is_ok());
        snapshot
            .domains
            .get_mut("topology")
            .unwrap()
            .borrowers
            .insert("stream-c".into(), 1);
        assert!(validate_effect_plan(&plan, &context!()).is_err());
        snapshot = fixture().1;
        plan.obligations.clear();
        assert!(validate_effect_plan(&plan, &context!()).is_err());
        plan = fixture().0;
        plan.mutation_domains[0].access = ResourceAccess::Read;
        assert!(validate_effect_plan(&plan, &context!()).is_err());
    }

    #[test]
    fn candidate_must_match_approved_resource_and_mode() {
        let (plan, _, _) = fixture();
        let mut candidate: api::CaptureBinding = serde_json::from_value(json!({"capture_provider":"capture", "selector_schema":"selector/1", "selector":{}, "resource":{"provider":"display", "resource_id":"output", "incarnation":"native", "revision":4, "execution_domain":"seat", "mutation_domain":"topology", "mutation_domains":["topology"]}, "mode":{"width":1920,"height":1080,"refresh_millihz":60000,"hdr":false}, "plan_revision":2, "geometry_revision":3, "generation":4})).unwrap();
        assert!(validate_candidate(&plan, &candidate, 4, 3).is_ok());
        let mut frozen = plan.clone();
        frozen
            .candidate_constraint
            .as_mut()
            .unwrap()
            .geometry_revision = Some(4);
        assert!(validate_candidate(&frozen, &candidate, 4, candidate.geometry_revision).is_err());
        frozen
            .candidate_constraint
            .as_mut()
            .unwrap()
            .geometry_revision = Some(3);
        assert!(validate_candidate(&frozen, &candidate, 4, candidate.geometry_revision).is_ok());
        candidate.resource.revision = 5;
        assert!(validate_candidate(&plan, &candidate, 4, 3).is_ok());
        let mut read_only = plan.clone();
        read_only.resources[0].access = ResourceAccess::Read;
        assert!(validate_candidate(&read_only, &candidate, 4, 3).is_err());
        candidate.resource.revision = 3;
        assert!(validate_candidate(&plan, &candidate, 4, 3).is_err());
        candidate.resource.revision = 4;
        candidate.resource.execution_domain = "other-seat".into();
        assert!(validate_candidate(&plan, &candidate, 4, 3).is_err());
        candidate.resource.execution_domain = "seat".into();
        candidate.resource.incarnation = "replacement".into();
        assert!(validate_candidate(&plan, &candidate, 4, 3).is_err());
        candidate.resource.incarnation = "native".into();
        candidate.mode.width = 1280;
        assert!(validate_candidate(&plan, &candidate, 4, 3).is_err());
    }
}
