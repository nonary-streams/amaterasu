//! Serialized participant outputs and owner-mediated control contracts.
//!
//! Payloads and identifiers are descriptive data, never authority. They must not
//! contain native handles; owners verify coordinator admission and permits before
//! executing a control request.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

use crate::{CaptureBinding, LifecycleTarget, MethodRef, ResourceAccess, ResourceRef};

/// Identifies an approved, versioned schema and its exact content digest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SchemaRef {
    pub id: String,
    pub major: u32,
    pub minor: u32,
    pub digest: String,
}

/// Complete observation of one mutation domain at the reported revision.
/// The coordinator independently verifies its provenance and freshness.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DomainObservation {
    pub domain: String,
    pub revision: u64,
    pub borrowers: BTreeMap<String, u64>,
    pub complete: bool,
}

/// A producer's serialized observation, bound to its lifecycle and resource.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceOutput {
    pub output_id: String,
    pub capability: String,
    pub schema: SchemaRef,
    pub producer: String,
    pub producer_service: String,
    pub origin: LifecycleTarget,
    pub resource: ResourceRef,
    /// Revisions can advance only under an owner-authenticated held permit;
    /// independently verify each observation before admitting effects.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub domain_observations: Vec<DomainObservation>,
    pub data: Value,
}

/// A proposed owner-mediated operation; admission and a permit are required.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceControlProposal {
    pub proposal_id: String,
    pub output_id: String,
    pub owner_provider: String,
    pub owner_service: String,
    pub target: LifecycleTarget,
    pub resource: ResourceRef,
    pub method: MethodRef,
    pub argument_schema: SchemaRef,
    pub arguments: Value,
    pub access: ResourceAccess,
    pub mutation_domains: Vec<String>,
}

/// A control result confirmed by the coordinator for its intended consumer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfirmedControl {
    pub proposal_id: String,
    pub output: ResourceOutput,
    pub candidate: Option<CaptureBinding>,
}

/// Coordinator request to the resource owner, with a separately verified permit.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceControlRequest {
    pub version: u32,
    pub transition_id: String,
    pub operation_id: String,
    pub provider_sequence: u64,
    pub target: LifecycleTarget,
    pub proposal: ResourceControlProposal,
    pub admission_id: String,
    pub permit_id: String,
    pub remaining_ms: u64,
}

/// Owner's result for an admitted control operation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum ResourceControlResponse {
    Applied {
        output: Box<ResourceOutput>,
        candidate: Option<Box<CaptureBinding>>,
    },
    Pending {
        receipt_id: String,
    },
    Rejected {
        reason: String,
    },
    RecoveryRequired {
        reason: String,
    },
}

/// Finalize a staged owner resource after all admitted dependent controls.
/// The permit is independently verified against the current resource revision.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceFinalizeRequest {
    pub version: u32,
    pub transition_id: String,
    pub operation_id: String,
    pub provider_sequence: u64,
    pub target: LifecycleTarget,
    pub output: ResourceOutput,
    pub control_results: Vec<ConfirmedControl>,
    pub admission_id: String,
    pub permit_id: String,
    pub remaining_ms: u64,
}

const MAX_COMPOSITION_ITEMS: usize = 32;
const MAX_COMPOSITION_BYTES: usize = 12 * 1024;

fn within_size(value: &impl Serialize) -> bool {
    serde_json::to_vec(value).is_ok_and(|bytes| bytes.len() <= MAX_COMPOSITION_BYTES)
}

fn valid_target(target: &LifecycleTarget) -> bool {
    match target {
        LifecycleTarget::Session {
            host_epoch,
            session_id,
            generation,
        } => {
            crate::valid_opaque_id(host_epoch)
                && crate::valid_opaque_id(session_id)
                && *generation > 0
        }
        LifecycleTarget::Stream {
            host_epoch,
            session_id,
            stream_id,
            generation,
        } => {
            crate::valid_opaque_id(host_epoch)
                && crate::valid_opaque_id(session_id)
                && crate::valid_opaque_id(stream_id)
                && *generation > 0
        }
        LifecycleTarget::Validation { .. } => false,
    }
}

fn valid_service(service: &str) -> bool {
    crate::valid_identifier(service)
        && service
            .strip_suffix("/2")
            .is_some_and(|name| !name.is_empty() && !name.contains('/'))
}

fn domains(resource: &ResourceRef) -> Option<std::collections::BTreeSet<&str>> {
    let mut seen = std::collections::BTreeSet::new();
    if resource.mutation_domains.len() > MAX_COMPOSITION_ITEMS {
        return None;
    }
    for domain in &resource.mutation_domains {
        if !crate::valid_opaque_id(domain) || !seen.insert(domain.as_str()) {
            return None;
        }
    }
    if let Some(domain) = &resource.mutation_domain {
        if !crate::valid_opaque_id(domain) {
            return None;
        }
        seen.insert(domain);
    }
    Some(seen)
}

fn valid_resource(resource: &ResourceRef) -> bool {
    crate::valid_identifier(&resource.provider)
        && crate::valid_opaque_id(&resource.resource_id)
        && crate::valid_opaque_id(&resource.incarnation)
        && crate::valid_opaque_id(&resource.execution_domain)
        && resource.revision > 0
        && domains(resource).is_some()
}

fn same_resource(a: &ResourceRef, b: &ResourceRef) -> bool {
    a.provider == b.provider
        && a.resource_id == b.resource_id
        && a.incarnation == b.incarnation
        && a.execution_domain == b.execution_domain
        && domains(a) == domains(b)
}

/// Validate shape only. Installed schemas and approvals must be verified separately.
pub fn validate_schema_ref(schema: &SchemaRef) -> Result<(), String> {
    if !crate::valid_identifier(&schema.id)
        || schema.major == 0
        || schema.digest.len() != 64
        || !schema
            .digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        || !within_size(schema)
    {
        return Err("invalid schema reference".into());
    }
    Ok(())
}

/// Check the producer's origin against a session or its child stream; no authority is granted.
pub fn output_in_scope(output: &ResourceOutput, target: &LifecycleTarget) -> bool {
    if !valid_target(&output.origin) || !valid_target(target) {
        return false;
    }
    match (&output.origin, target) {
        (
            LifecycleTarget::Session {
                host_epoch: a,
                session_id: b,
                ..
            },
            LifecycleTarget::Stream {
                host_epoch: x,
                session_id: y,
                ..
            },
        ) => a == x && b == y,
        (a, b) => a == b,
    }
}

/// Check an output's bounds and identity; it does not validate its schema contents.
pub fn validate_resource_output(output: &ResourceOutput) -> Result<(), String> {
    validate_schema_ref(&output.schema)?;
    let resource_domains = domains(&output.resource).ok_or("invalid resource domains")?;
    let mut observed = std::collections::BTreeSet::new();
    if output.domain_observations.len() > MAX_COMPOSITION_ITEMS
        || output.domain_observations.iter().any(|observation| {
            !crate::valid_opaque_id(&observation.domain)
                || !observation.complete
                || observation.revision == 0
                || observation.borrowers.len() > 128
                || observation
                    .borrowers
                    .iter()
                    .any(|(id, generation)| !crate::valid_opaque_id(id) || *generation == 0)
                || !observed.insert(observation.domain.as_str())
        })
        || observed != resource_domains
    {
        return Err("invalid domain observations".into());
    }
    if !crate::valid_opaque_id(&output.output_id)
        || !crate::valid_identifier(&output.capability)
        || !crate::valid_identifier(&output.producer)
        || !valid_service(&output.producer_service)
        || !valid_target(&output.origin)
        || !valid_resource(&output.resource)
        || output.producer != output.resource.provider
        || !output.data.is_object()
        || !within_size(output)
    {
        return Err("invalid resource output".into());
    }
    Ok(())
}

/// Structural checks only; the selected owner, permissions, and effect still require validation.
pub fn validate_resource_control_proposal(
    proposal: &ResourceControlProposal,
) -> Result<(), String> {
    validate_schema_ref(&proposal.argument_schema)?;
    let Some(resource_domains) = domains(&proposal.resource) else {
        return Err("invalid resource domains".into());
    };
    let mut selected = std::collections::BTreeSet::new();
    if !crate::valid_opaque_id(&proposal.proposal_id)
        || !crate::valid_opaque_id(&proposal.output_id)
        || !crate::valid_identifier(&proposal.owner_provider)
        || !valid_service(&proposal.owner_service)
        || !valid_target(&proposal.target)
        || !valid_resource(&proposal.resource)
        || proposal.owner_provider != proposal.resource.provider
        || proposal.method.service != proposal.owner_service
        || proposal.method.interface_major != 2
        || proposal.method.schema_revision == 0
        || !crate::valid_identifier(&proposal.method.method)
        || proposal.method.method.contains('/')
        || !proposal.arguments.is_object()
        || proposal.mutation_domains.len() > MAX_COMPOSITION_ITEMS
        || proposal
            .mutation_domains
            .iter()
            .any(|domain| !resource_domains.contains(domain.as_str()) || !selected.insert(domain))
        || !within_size(proposal)
    {
        return Err("invalid resource control proposal".into());
    }
    Ok(())
}

fn valid_envelope(
    version: u32,
    transition: &str,
    operation: &str,
    sequence: u64,
    admission: &str,
    permit: &str,
    remaining: u64,
) -> bool {
    version == crate::COMPOSITION_VERSION
        && crate::valid_opaque_id(transition)
        && crate::valid_opaque_id(operation)
        && sequence > 0
        && crate::valid_opaque_id(admission)
        && crate::valid_opaque_id(permit)
        && (1..=30_000).contains(&remaining)
}

/// Verify the request's structural envelope and target binding, not the permit's authority.
pub fn validate_resource_control_request(request: &ResourceControlRequest) -> Result<(), String> {
    validate_resource_control_proposal(&request.proposal)?;
    if !valid_envelope(
        request.version,
        &request.transition_id,
        &request.operation_id,
        request.provider_sequence,
        &request.admission_id,
        &request.permit_id,
        request.remaining_ms,
    ) || request.target != request.proposal.target
        || !within_size(request)
    {
        return Err("invalid resource control request".into());
    }
    Ok(())
}

fn valid_candidate(
    candidate: &CaptureBinding,
    output: &ResourceOutput,
    target: &LifecycleTarget,
) -> bool {
    let generation = match target {
        LifecycleTarget::Stream { generation, .. } => *generation,
        _ => return false,
    };
    crate::validate_capture_binding(candidate).is_ok()
        && candidate.resource == output.resource
        && candidate.generation == generation
}

/// Bind an owner response to its request; native effects and readback require independent checks.
pub fn validate_resource_control_response(
    request: &ResourceControlRequest,
    response: &ResourceControlResponse,
) -> Result<(), String> {
    validate_resource_control_request(request)?;
    if !within_size(response) {
        return Err("control response exceeds size limit".into());
    }
    match response {
        ResourceControlResponse::Applied { output, candidate } => {
            validate_resource_output(output)?;
            if output.output_id != request.proposal.output_id
                || output.producer != request.proposal.owner_provider
                || output.producer_service != request.proposal.owner_service
                || !output_in_scope(output, &request.target)
                || !same_resource(&output.resource, &request.proposal.resource)
                || output.resource.revision < request.proposal.resource.revision
                || candidate
                    .as_ref()
                    .is_some_and(|candidate| !valid_candidate(candidate, output, &request.target))
            {
                return Err("control response does not match request".into());
            }
        }
        ResourceControlResponse::Pending { receipt_id } if crate::valid_opaque_id(receipt_id) => {}
        ResourceControlResponse::Rejected { reason }
        | ResourceControlResponse::RecoveryRequired { reason }
            if crate::valid_opaque_id(reason) => {}
        _ => return Err("invalid control response".into()),
    }
    Ok(())
}

/// Validate finalized data structurally; confirmations, authorization and readback remain separate.
pub fn validate_resource_finalize_request(request: &ResourceFinalizeRequest) -> Result<(), String> {
    validate_resource_output(&request.output)?;
    if !valid_envelope(
        request.version,
        &request.transition_id,
        &request.operation_id,
        request.provider_sequence,
        &request.admission_id,
        &request.permit_id,
        request.remaining_ms,
    ) || !output_in_scope(&request.output, &request.target)
        || request.control_results.len() > MAX_COMPOSITION_ITEMS
        || !within_size(request)
    {
        return Err("invalid resource finalize request".into());
    }
    let mut proposals = std::collections::BTreeSet::new();
    for result in &request.control_results {
        validate_resource_output(&result.output)?;
        if !crate::valid_opaque_id(&result.proposal_id)
            || !proposals.insert(&result.proposal_id)
            || !output_in_scope(&result.output, &request.target)
            || !same_resource(&request.output.resource, &result.output.resource)
            || result.output.resource.revision > request.output.resource.revision
            || result.candidate.as_ref().is_some_and(|candidate| {
                !valid_candidate(candidate, &result.output, &request.target)
            })
        {
            return Err("invalid confirmed control".into());
        }
    }
    Ok(())
}

/// Bind finalize output to the selected resource; authorization and readback remain separate.
pub fn validate_resource_finalize_response(
    request: &ResourceFinalizeRequest,
    response: &ResourceControlResponse,
) -> Result<(), String> {
    validate_resource_finalize_request(request)?;
    if !within_size(response) {
        return Err("finalize response exceeds size limit".into());
    }
    match response {
        ResourceControlResponse::Applied { output, candidate } => {
            validate_resource_output(output)?;
            let selected = &request.output;
            if output.output_id != selected.output_id
                || output.capability != selected.capability
                || output.schema != selected.schema
                || output.producer != selected.producer
                || output.producer_service != selected.producer_service
                || output.origin != selected.origin
                || !same_resource(&output.resource, &selected.resource)
                || output.resource.revision < selected.resource.revision
                || candidate
                    .as_ref()
                    .is_some_and(|candidate| !valid_candidate(candidate, output, &request.target))
            {
                return Err("finalize response does not match selected output".into());
            }
        }
        ResourceControlResponse::Pending { receipt_id } if crate::valid_opaque_id(receipt_id) => {}
        ResourceControlResponse::Rejected { reason }
        | ResourceControlResponse::RecoveryRequired { reason }
            if crate::valid_opaque_id(reason) => {}
        _ => return Err("invalid finalize response".into()),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture() -> (
        ResourceOutput,
        ResourceControlRequest,
        ResourceFinalizeRequest,
    ) {
        let session =
            json!({"kind":"session","host_epoch":"epoch","session_id":"session","generation":3});
        let stream = json!({"kind":"stream","host_epoch":"epoch","session_id":"session","stream_id":"stream","generation":5});
        let resource = json!({"provider":"example.display","resource_id":"output","incarnation":"native","revision":4,"execution_domain":"seat","mutation_domain":"topology","mutation_domains":["topology"]});
        let schema = json!({"id":"example.schema","major":2,"minor":0,"digest":"a".repeat(64)});
        let output: ResourceOutput = serde_json::from_value(json!({"output_id":"output.1","capability":"example.display","schema":schema,"producer":"example.display","producer_service":"example.display/2","origin":session,"resource":resource,"domain_observations":[{"domain":"topology","revision":4,"borrowers":{"stream":5},"complete":true}],"data":{"width":1920}})).unwrap();
        let proposal = json!({"proposal_id":"proposal","output_id":"output.1","owner_provider":"example.display","owner_service":"example.display/2","target":stream,"resource":resource,"method":{"service":"example.display/2","method":"configure","interface_major":2,"interface_minor":0,"schema_revision":1},"argument_schema":schema,"arguments":{"width":1920},"access":"exclusive","mutation_domains":["topology"]});
        let request: ResourceControlRequest = serde_json::from_value(json!({"version":2,"transition_id":"transition","operation_id":"operation","provider_sequence":1,"target":stream,"proposal":proposal,"admission_id":"admission","permit_id":"permit","remaining_ms":5000})).unwrap();
        let finalize: ResourceFinalizeRequest = serde_json::from_value(json!({"version":2,"transition_id":"transition","operation_id":"finalize","provider_sequence":2,"target":stream,"output":output,"control_results":[],"admission_id":"admission","permit_id":"permit","remaining_ms":5000})).unwrap();
        (output, request, finalize)
    }

    #[test]
    fn output_scope_and_digest_are_checked() {
        let (mut output, request, _) = fixture();
        assert!(validate_resource_output(&output).is_ok());
        assert!(output_in_scope(&output, &request.target));
        let mut foreign = request.target.clone();
        if let LifecycleTarget::Stream { session_id, .. } = &mut foreign {
            *session_id = "other".into();
        }
        assert!(!output_in_scope(&output, &foreign));
        output.schema.digest = "A".repeat(64);
        assert!(validate_resource_output(&output).is_err());
    }

    #[test]
    fn control_rejects_owner_scope_and_domain_substitution() {
        let (_, request, _) = fixture();
        assert!(validate_resource_control_request(&request).is_ok());
        let mut other = request.clone();
        other.proposal.owner_provider = "other".into();
        assert!(validate_resource_control_request(&other).is_err());
        let mut other = request.clone();
        other.proposal.mutation_domains.push("foreign".into());
        assert!(validate_resource_control_request(&other).is_err());
        let mut other = request.clone();
        other.proposal.method.interface_major = 1;
        assert!(validate_resource_control_request(&other).is_err());
        let mut other = request.clone();
        other.provider_sequence = 0;
        assert!(validate_resource_control_request(&other).is_err());
    }

    #[test]
    fn response_and_finalize_fence_resource_substitution_and_bounds() {
        let (output, request, mut finalize) = fixture();
        let response = ResourceControlResponse::Applied {
            output: Box::new(output.clone()),
            candidate: None,
        };
        assert!(validate_resource_control_response(&request, &response).is_ok());
        let mut changed = output.clone();
        changed.resource.incarnation = "other".into();
        assert!(
            validate_resource_control_response(
                &request,
                &ResourceControlResponse::Applied {
                    output: Box::new(changed),
                    candidate: None
                }
            )
            .is_err()
        );
        finalize.control_results.push(ConfirmedControl {
            proposal_id: "proposal".into(),
            output: output.clone(),
            candidate: None,
        });
        assert!(validate_resource_finalize_request(&finalize).is_ok());
        finalize
            .control_results
            .push(finalize.control_results[0].clone());
        assert!(validate_resource_finalize_request(&finalize).is_err());
        let mut oversized = output;
        oversized.data = json!({"data":"x".repeat(12 * 1024)});
        assert!(validate_resource_output(&oversized).is_err());
    }
    #[test]
    fn finalize_response_binds_selected_output_and_candidate() {
        let (output, _, finalize) = fixture();
        let candidate: CaptureBinding = serde_json::from_value(json!({"capture_provider":"example.capture","selector_schema":"example.selector","selector":{},"resource":output.resource,"mode":{"width":1920,"height":1080,"refresh_millihz":60000,"hdr":false},"plan_revision":1,"geometry_revision":1,"generation":5})).unwrap();
        let mut returned = output.clone();
        returned.resource.revision += 1;
        let mut updated_candidate = candidate.clone();
        updated_candidate.resource = returned.resource.clone();
        let response = |output: ResourceOutput, candidate: Option<CaptureBinding>| {
            ResourceControlResponse::Applied {
                output: Box::new(output),
                candidate: candidate.map(Box::new),
            }
        };
        assert!(
            validate_resource_finalize_response(
                &finalize,
                &response(returned.clone(), Some(updated_candidate.clone()))
            )
            .is_ok()
        );
        let mut stale = returned.clone();
        stale.resource.revision = output.resource.revision - 1;
        assert!(validate_resource_finalize_response(&finalize, &response(stale, None)).is_err());
        let mut changed = returned.clone();
        changed.schema.digest = "b".repeat(64);
        assert!(validate_resource_finalize_response(&finalize, &response(changed, None)).is_err());
        let mut changed = returned.clone();
        changed.capability = "example.other".into();
        assert!(validate_resource_finalize_response(&finalize, &response(changed, None)).is_err());
        let mut changed = returned.clone();
        changed.resource.incarnation = "other".into();
        assert!(validate_resource_finalize_response(&finalize, &response(changed, None)).is_err());
        updated_candidate.generation += 1;
        assert!(
            validate_resource_finalize_response(
                &finalize,
                &response(returned, Some(updated_candidate))
            )
            .is_err()
        );
        assert!(
            validate_resource_finalize_response(
                &finalize,
                &ResourceControlResponse::Pending {
                    receipt_id: "receipt".into()
                }
            )
            .is_ok()
        );
        assert!(
            validate_resource_finalize_response(
                &finalize,
                &ResourceControlResponse::Rejected {
                    reason: "unavailable".into()
                }
            )
            .is_ok()
        );
        assert!(
            validate_resource_finalize_response(
                &finalize,
                &ResourceControlResponse::RecoveryRequired {
                    reason: "unknown".into()
                }
            )
            .is_ok()
        );
    }
    #[test]
    fn output_requires_complete_unique_domain_observations() {
        let (output, _, _) = fixture();
        assert!(validate_resource_output(&output).is_ok());
        let mut missing = output.clone();
        missing.domain_observations.clear();
        assert!(validate_resource_output(&missing).is_err());
        let mut incomplete = output.clone();
        incomplete.domain_observations[0].complete = false;
        assert!(validate_resource_output(&incomplete).is_err());
        let mut duplicate = output.clone();
        duplicate
            .domain_observations
            .push(duplicate.domain_observations[0].clone());
        assert!(validate_resource_output(&duplicate).is_err());
        let mut stale = output.clone();
        stale.domain_observations[0]
            .borrowers
            .insert("other".into(), 0);
        assert!(validate_resource_output(&stale).is_err());
        let mut foreign = output;
        foreign.domain_observations[0].domain = "foreign".into();
        assert!(validate_resource_output(&foreign).is_err());
    }
}
