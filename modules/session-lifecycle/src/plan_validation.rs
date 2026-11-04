//! Validation bridge from public provider plans to internal planning primitives.
//!
//! The caller supplies authoritative, complete observations, checks installed
//! capability/method grants, acquires returned rights, and repeats this check
//! under those rights immediately before effects. This module does not observe
//! native resources or authenticate provider and consumer reports.

use std::collections::{BTreeMap, BTreeSet};

use crate::planning::{self, Access, DomainEffect, DomainState, EffectPlan, PlanningSnapshot};
use crate::prepare_authority::{ControlPolicy, DomainScope};
use amaterasu_extension_api::{self as api, ResourceAccess};
use serde::{Deserialize, Serialize};

/// Selected by the coordinator from installed policy, authenticated producer
/// evidence and broker approvals. Provider plans cannot authorize themselves.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovedControlBinding {
    pub policy: ControlPolicy,
    pub output: api::ResourceOutput,
    pub target: api::LifecycleTarget,
}

/// Caller-authorized policy and a complete current observation. The caller must
/// obtain these from trusted authorities, not from the provider's plan.
pub struct AdmissionContext<'a> {
    pub provider: &'a str,
    pub approved_control_bindings: &'a [ApprovedControlBinding],
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
    if plan.control_proposals.iter().any(|proposal| {
        !plan
            .resources
            .iter()
            .any(|resource| proposal_is_approved(plan, context, proposal, resource))
    }) {
        return Err("unapproved control proposal".into());
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
                obligation_matches_resource(plan, context, obligation, resource)
                    && obligation
                        .mutation_domain
                        .as_ref()
                        .is_none_or(|domain| resource.mutation_domains.contains(domain))
            })
        {
            return Err("mutation lacks cleanup obligation".into());
        }
    }
    validate_obligations(plan, context)?;
    for domain in &plan.mutation_domains {
        if access(&domain.access) != Access::Read
            && !plan.obligations.iter().any(|obligation| {
                plan.resources.iter().any(|resource| {
                    obligation_matches_resource(plan, context, obligation, resource)
                        && resource.mutation_domains.contains(&domain.domain)
                        && obligation
                            .mutation_domain
                            .as_deref()
                            .is_none_or(|id| id == domain.domain)
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
    context: &AdmissionContext<'_>,
) -> Result<(), String> {
    let by_id: BTreeMap<_, _> = plan
        .obligations
        .iter()
        .map(|obligation| (obligation.obligation_id.as_str(), obligation))
        .collect();
    for obligation in &plan.obligations {
        if !context
            .approved_cleanup_methods
            .contains(&obligation.cleanup_method)
            || !unique_valid(&obligation.depends_on)
            || plan
                .resources
                .iter()
                .filter(|resource| obligation_matches_resource(plan, context, obligation, resource))
                .count()
                != 1
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

fn obligation_matches_resource(
    plan: &api::ResourcePlan,
    context: &AdmissionContext<'_>,
    obligation: &api::ResourceObligation,
    resource: &api::PlannedResource,
) -> bool {
    if obligation.resource_id != resource.resource_id
        || (!context.provider.is_empty() && obligation.provider != context.provider)
        || obligation
            .mutation_domain
            .as_ref()
            .is_some_and(|domain| !resource.mutation_domains.contains(domain))
    {
        return false;
    }
    if obligation.provider == resource.provider {
        return true;
    }
    if obligation.provider != context.provider || resource.access == ResourceAccess::Read {
        return false;
    }
    let mut matches = plan
        .control_proposals
        .iter()
        .filter(|proposal| proposal_is_approved(plan, context, proposal, resource));
    matches.next().is_some() && matches.next().is_none()
}

fn proposal_is_approved(
    plan: &api::ResourcePlan,
    context: &AdmissionContext<'_>,
    proposal: &api::ResourceControlProposal,
    resource: &api::PlannedResource,
) -> bool {
    context
        .approved_control_bindings
        .iter()
        .filter(|binding| {
            proposal_matches_binding(proposal, binding, context.provider, resource)
                && binding
                    .output
                    .domain_observations
                    .iter()
                    .all(|observation| {
                        observation.complete
                            && plan.mutation_domains.iter().any(|domain| {
                                domain.domain == observation.domain
                                    && domain.expected_revision == observation.revision
                                    && domain.borrowers == observation.borrowers
                            })
                    })
        })
        .count()
        == 1
}

fn proposal_matches_binding(
    proposal: &api::ResourceControlProposal,
    binding: &ApprovedControlBinding,
    dependent: &str,
    planned: &api::PlannedResource,
) -> bool {
    let policy = &binding.policy;
    let output = &binding.output;
    let resource = &output.resource;
    let domains: BTreeSet<_> = resource
        .mutation_domains
        .iter()
        .cloned()
        .chain(resource.mutation_domain.iter().cloned())
        .collect();
    let planned_domains: BTreeSet<_> = planned.mutation_domains.iter().cloned().collect();
    let proposal_domains: BTreeSet<_> = proposal.mutation_domains.iter().cloned().collect();
    api::validate_resource_control_proposal(proposal).is_ok()
        && api::validate_resource_output(output).is_ok()
        && api::output_in_scope(output, &binding.target)
        && policy.dependent_provider == dependent
        && policy.owner_provider == output.producer
        && policy.owner_service == output.producer_service
        && policy.output_capability == output.capability
        && policy.output_schema == output.schema
        && proposal.target == binding.target
        && proposal.output_id == output.output_id
        && proposal.owner_provider == output.producer
        && proposal.owner_service == output.producer_service
        && proposal.method == policy.owner_method
        && proposal.argument_schema == policy.argument_schema
        && proposal.resource == *resource
        && proposal.access == planned.access
        && level(access(&proposal.access)) <= level(access(&policy.max_access))
        && planned.provider == resource.provider
        && planned.resource_id == resource.resource_id
        && planned.incarnation.as_deref() == Some(resource.incarnation.as_str())
        && planned.expected_revision == Some(resource.revision)
        && planned.execution_domain == resource.execution_domain
        && !domains.is_empty()
        && planned_domains == domains
        && proposal_domains == domains
        && proposal_domains.len() == proposal.mutation_domains.len()
        && match policy.domain_scope {
            DomainScope::Static => policy.mutation_domains == domains,
            DomainScope::Resource => policy.mutation_domains.is_empty(),
        }
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
    if let Some(context) = api::stream_join_plan_context(plan)? {
        let mut expected = context.binding.ok_or("managed join binding missing")?;
        expected.generation = expected_generation;
        if candidate != &expected || candidate.geometry_revision != expected_geometry_revision {
            return Err("joined binding differs from shared source".into());
        }
        return Ok(());
    }
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
                    provider: "display",
                    approved_control_bindings: &[],
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
    fn dependent_cleanup_requires_exact_approved_owner_binding() {
        let (mut plan, snapshot, _) = fixture();
        plan.obligations[0].provider = "settings".into();
        plan.obligations[0].cleanup_method.service = "settings/2".into();
        plan.obligations[0].cleanup_method.interface_major = 2;
        let method: api::MethodRef = serde_json::from_value(json!({"service":"owner/2","method":"resource_control","interface_major":2,"interface_minor":0,"schema_revision":1})).unwrap();
        let schema = json!({"id":"output/1","major":1,"minor":0,"digest":"a".repeat(64)});
        let target = json!({"kind":"stream","host_epoch":"epoch","session_id":"session","stream_id":"stream","generation":1});
        let resource = json!({"provider":"display","resource_id":"output","incarnation":"native","revision":4,"execution_domain":"seat","mutation_domain":"topology","mutation_domains":["topology"]});
        let output: api::ResourceOutput = serde_json::from_value(json!({"output_id":"out","capability":"display/output","schema":schema,"producer":"display","producer_service":"owner/2","origin":target,"resource":resource,"domain_observations":[{"domain":"topology","revision":3,"borrowers":{"stream-a":1,"stream-b":2},"complete":true}],"data":{}})).unwrap();
        let policy: ControlPolicy = serde_json::from_value(json!({"dependent_provider":"settings","owner_provider":"display","owner_service":"owner/2","owner_method":method,"status_method":method,"settlement_method":method,"argument_schema":schema,"output_capability":"display/output","output_schema":schema,"max_access":"exclusive","domain_scope":"resource","mutation_domains":[]})).unwrap();
        let binding = ApprovedControlBinding {
            policy,
            output,
            target: serde_json::from_value(target.clone()).unwrap(),
        };
        let proposal: api::ResourceControlProposal = serde_json::from_value(json!({"proposal_id":"proposal","output_id":"out","owner_provider":"display","owner_service":"owner/2","target":target,"resource":resource,"method":method,"argument_schema":schema,"arguments":{},"access":"exclusive","mutation_domains":["topology"]})).unwrap();
        plan.control_proposals = vec![proposal];
        let methods = vec![plan.obligations[0].cleanup_method.clone()];
        let capabilities = ["display/change".into()].into();
        let borrowers = ["stream-a".into(), "stream-b".into()].into();
        let run = |candidate: &api::ResourcePlan, bindings: &[ApprovedControlBinding]| {
            validate_effect_plan(
                candidate,
                &AdmissionContext {
                    provider: "settings",
                    approved_control_bindings: bindings,
                    snapshot: &snapshot,
                    complete: true,
                    approved_capabilities: &capabilities,
                    approved_cleanup_methods: &methods,
                    authorized_borrowers: &borrowers,
                },
            )
        };
        assert!(run(&plan, std::slice::from_ref(&binding)).is_ok());
        assert!(run(&plan, &[]).is_err());
        let mut bad = plan.clone();
        bad.obligations[0].provider = "display".into();
        assert!(run(&bad, std::slice::from_ref(&binding)).is_err());
        let mut bad = plan.clone();
        bad.obligations[0].cleanup_method.method = "unapproved".into();
        assert!(run(&bad, std::slice::from_ref(&binding)).is_err());
        for field in [
            "owner",
            "output",
            "incarnation",
            "revision",
            "domain",
            "access",
            "method",
            "schema",
            "target",
        ] {
            let mut bad = plan.clone();
            let proposal = &mut bad.control_proposals[0];
            match field {
                "owner" => proposal.owner_provider = "other".into(),
                "output" => proposal.output_id = "other".into(),
                "incarnation" => proposal.resource.incarnation = "other".into(),
                "revision" => proposal.resource.revision += 1,
                "domain" => proposal.mutation_domains.clear(),
                "access" => proposal.access = ResourceAccess::Shared,
                "method" => proposal.method.method = "other".into(),
                "schema" => proposal.argument_schema.digest = "b".repeat(64),
                "target" => {
                    if let api::LifecycleTarget::Stream { generation, .. } = &mut proposal.target {
                        *generation += 1
                    }
                }
                _ => unreachable!(),
            }
            assert!(
                run(&bad, std::slice::from_ref(&binding)).is_err(),
                "accepted changed {field}"
            );
        }
        let mut duplicate = plan.clone();
        let mut second = duplicate.control_proposals[0].clone();
        second.proposal_id = "other-proposal".into();
        duplicate.control_proposals.push(second);
        assert!(run(&duplicate, std::slice::from_ref(&binding)).is_err());
        let mut forged_extra = plan.clone();
        let mut forged = forged_extra.control_proposals[0].clone();
        forged.proposal_id = "forged".into();
        forged.output_id = "other".into();
        forged_extra.control_proposals.push(forged);
        assert!(run(&forged_extra, std::slice::from_ref(&binding)).is_err());
        let mut stale = binding.clone();
        stale.output.domain_observations[0].revision += 1;
        assert!(run(&plan, &[stale]).is_err());

        // An obligation has no owner field separate from its dependent
        // provider. Identical resource IDs from two owners must not let one
        // cleanup record cover both merely because both proposals are valid.
        let mut ambiguous_plan = plan.clone();
        let mut second_resource = ambiguous_plan.resources[0].clone();
        second_resource.provider = "other-owner".into();
        ambiguous_plan.resources.push(second_resource);
        let mut second_binding = binding.clone();
        second_binding.policy.owner_provider = "other-owner".into();
        second_binding.policy.owner_service = "other/2".into();
        second_binding.policy.owner_method.service = "other/2".into();
        second_binding.output.producer = "other-owner".into();
        second_binding.output.producer_service = "other/2".into();
        second_binding.output.output_id = "other-out".into();
        second_binding.output.resource.provider = "other-owner".into();
        let mut second_proposal = ambiguous_plan.control_proposals[0].clone();
        second_proposal.proposal_id = "second".into();
        second_proposal.owner_provider = "other-owner".into();
        second_proposal.owner_service = "other/2".into();
        second_proposal.output_id = "other-out".into();
        second_proposal.method.service = "other/2".into();
        second_proposal.resource.provider = "other-owner".into();
        ambiguous_plan.control_proposals.push(second_proposal);
        let mut ambiguous_snapshot = snapshot.clone();
        let mut second_observed = ambiguous_snapshot.resources["display"]["output"].clone();
        second_observed.provider = "other-owner".into();
        ambiguous_snapshot.resources.insert(
            "other-owner".into(),
            [("output".into(), second_observed)].into(),
        );
        let bindings = [binding.clone(), second_binding];
        assert!(
            validate_effect_plan(
                &ambiguous_plan,
                &AdmissionContext {
                    provider: "settings",
                    approved_control_bindings: &bindings,
                    snapshot: &ambiguous_snapshot,
                    complete: true,
                    approved_capabilities: &capabilities,
                    approved_cleanup_methods: &methods,
                    authorized_borrowers: &borrowers
                }
            )
            .is_err()
        );
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
