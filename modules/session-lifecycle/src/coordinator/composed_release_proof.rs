//! Durable reverse-graph stream cleanup evidence. Missing evidence fences release.
use super::*;
use crate::prepare_driver::PreparePhase;
use amaterasu_extension_api::{CleanupState, ProviderResponse, ResourceControlResponse};

const MAX_PROOFS: usize = 64;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Proof {
    work: Work,
    finalize_permit_id: String,
    released_output: amaterasu_extension_api::ResourceOutput,
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ComposedReleaseBook {
    proofs: Vec<Proof>,
}

impl ComposedReleaseBook {
    pub fn references_admission(&self, id: &str) -> bool {
        self.proofs.iter().any(|proof| {
            proof
                .work
                .states
                .values()
                .any(|state| state.admission_id.as_deref() == Some(id))
        })
    }
    pub fn validate(&self, journal: &Journal) -> bool {
        self.proofs.len() <= MAX_PROOFS
            && self.proofs.iter().enumerate().all(|(index, proof)| {
                !self.proofs[..index]
                    .iter()
                    .any(|other| other.work.target == proof.work.target)
                    && verify(journal, proof).is_ok()
            })
    }
    pub fn has_valid_release(&self, journal: &Journal, target: &LifecycleTarget) -> bool {
        self.proofs
            .iter()
            .any(|proof| &proof.work.target == target && verify(journal, proof).is_ok())
    }
    pub fn released_output_for_target(
        &self,
        target: &LifecycleTarget,
    ) -> Option<(&str, &amaterasu_extension_api::ResourceOutput)> {
        let proof = self
            .proofs
            .iter()
            .find(|proof| &proof.work.target == target)?;
        let owner = &proof.released_output.producer;
        let admission = proof.work.states.get(owner)?.admission_id.as_deref()?;
        Some((admission, &proof.released_output))
    }
}

fn verify(journal: &Journal, proof: &Proof) -> ModuleResult<()> {
    let work = &proof.work;
    let (epoch, session_id, Some(stream_id), generation) = operations::target_parts(&work.target)?
    else {
        return Err("release_stream_required".into());
    };
    let session = journal
        .sessions
        .get(session_id)
        .ok_or("release_session_missing")?;
    let stream = session
        .streams
        .get(stream_id)
        .ok_or("release_stream_missing")?;
    let graph = work
        .provider_graph
        .as_ref()
        .ok_or("release_graph_missing")?;
    let reverse: Vec<_> = graph.order.iter().rev().cloned().collect();
    let roots: Vec<_> = graph
        .order
        .iter()
        .filter(|provider| graph.dependencies.get(*provider).is_some_and(Vec::is_empty))
        .collect();
    if work.phase != LifecyclePhase::StreamRelease
        || work.providers != reverse
        || work.index != work.providers.len()
        || work.states.len() != work.providers.len()
        || graph.order.len() < 2
        || roots.len() != 1
        || graph.order.iter().collect::<BTreeSet<_>>().len() != graph.order.len()
        || graph.dependencies.len() != graph.order.len()
        || graph.order.iter().enumerate().any(|(index, provider)| {
            graph.dependencies.get(provider).is_none_or(|dependencies| {
                dependencies
                    .iter()
                    .any(|dependency| !graph.order[..index].contains(dependency))
            })
        })
        || !work.cleanup_blockers.is_empty()
        || session.host_epoch != epoch
        || stream.generation != generation
        || !matches!(stream.phase, StreamPhase::Quiescing | StreamPhase::Retired)
        || !stream.required.is_subset(&stream.retirements)
        || serde_json::to_value(&stream.provider_graph).ok()
            != serde_json::to_value(&work.provider_graph).ok()
    {
        return Err("release_graph_context_invalid".into());
    }
    let mut finalized = journal.finalize_results.records().filter(|item| {
        item.request.permit_id == proof.finalize_permit_id && item.request.target == work.target
    });
    let finalized = finalized.next().ok_or("release_finalize_missing")?;
    if finalized.selection.policy.owner_provider != **roots.first().unwrap()
        || !finalized.validate_structure()
        || !finalized.validate_control_chain(journal)
    {
        return Err("release_finalize_invalid".into());
    }
    let finalize_permit = journal
        .authority
        .finalize_permits
        .get(&finalized.request.permit_id)
        .ok_or("release_finalize_permit_missing")?;
    let finalize_admission = journal
        .authority
        .settled
        .get(&finalized.request.admission_id)
        .ok_or("release_finalize_admission_missing")?;
    if !finalize_permit.consumed
        || !finalize_permit.settled
        || finalize_permit.request != finalized.request
        || finalize_permit.context.target != work.target
        || finalize_permit.context.transition_id != finalized.request.transition_id
        || finalize_permit.context.policy != finalized.selection.policy
        || finalize_permit.context.output != finalized.request.output
        || finalize_permit.context.control_results != finalized.request.control_results
        || finalize_permit.context.parent_generation != finalized.selection.parent_generation
        || finalize_admission.provider != finalized.selection.policy.owner_provider
        || finalize_admission.plan != finalized.plan
        || !journal
            .authority
            .output_admissions
            .contains(&finalized.request.admission_id)
        || session
            .admission_targets
            .get(&finalized.request.admission_id)
            != Some(&work.target)
        || session
            .admission_phases
            .get(&finalized.request.admission_id)
            != Some(&LifecyclePhase::StreamPrepare)
        || journal
            .admission_transitions
            .get(&finalized.request.admission_id)
            != Some(&finalized.request.transition_id)
        || journal.composition.finalize_selection(
            &finalized.selection.target,
            &finalized.selection.transition_id,
            &finalized.selection.policy.owner_provider,
            &finalized.selection.input.output.output_id,
        ) != Some(&finalized.selection)
        || finalized.plan_approval.as_ref().is_none_or(|approval| {
            !finalize_admission.selection.approvals.contains(approval)
                || approval.method != finalize_admission.selection.participant.plan_method
        })
    {
        return Err("release_finalize_provenance_invalid".into());
    }
    // The original prepared work is retained with a nonempty settled control
    // chain. Without it a no-op participant cannot be distinguished from an
    // unrecorded callback, so unsupported histories remain fenced.
    let source = finalized
        .chain_work
        .as_ref()
        .ok_or("release_prepare_evidence_missing")?;
    if source.target != work.target
        || source.phase != LifecyclePhase::StreamPrepare
        || source.providers != graph.order
        || serde_json::to_value(&source.provider_graph).ok()
            != serde_json::to_value(&work.provider_graph).ok()
    {
        return Err("release_prepare_evidence_mismatch".into());
    }
    let ResourceControlResponse::Applied {
        output,
        candidate: Some(candidate),
    } = &finalized.applied
    else {
        return Err("release_finalize_missing".into());
    };
    if stream.binding_owner != finalized.selection.policy.owner_provider
        || stream.candidate.as_ref() != Some(candidate.as_ref())
    {
        return Err("release_binding_mismatch".into());
    }
    let mut current = output.as_ref().clone();
    let progress = work
        .composition_progress
        .as_ref()
        .ok_or("release_progress_missing")?;
    let mut controls = progress.controls.iter();
    for provider in &work.providers {
        let original_state = source
            .states
            .get(provider)
            .ok_or("release_prepare_callback_missing")?;
        let original_id = original_state
            .admission_id
            .as_deref()
            .ok_or("original_admission_missing")?;
        let original = journal
            .authority
            .settled
            .get(original_id)
            .ok_or("original_admission_missing")?;
        let original_response = original_state
            .response
            .as_ref()
            .ok_or("release_prepare_callback_missing")?;
        let original_proof = composition::ProducerCallbackProof {
            participant: original_state.participant.clone(),
            request: original_state.request.clone(),
            terminal: original_state
                .terminal_dispatch
                .clone()
                .ok_or("release_prepare_callback_missing")?,
            pending_receipt: original_state.pending_receipt.clone(),
            pending_dispatch: original_state.pending_dispatch.clone(),
        };
        if original_state.phase != PreparePhase::Prepared
            || original.provider != *provider
            || original.selection.participant != original_state.participant
            || original_state.plan.as_ref() != Some(&original.plan)
            || original_state.request.approved_plan.as_ref() != Some(&original.plan)
            || original_state.request.admission_id.as_deref() != Some(original_id)
            || original_state.request.version != amaterasu_extension_api::COMPOSITION_VERSION
            || original_state.request.operation
                != amaterasu_extension_api::ProviderOperation::LifecycleCallback
            || original_state.request.target != work.target
            || original_state.request.phase != LifecyclePhase::StreamPrepare
            || original_state.request.transition_id != source.transition_id
            || !session.admission_ids.contains(original_id)
            || session.admission_targets.get(original_id) != Some(&work.target)
            || session.admission_phases.get(original_id) != Some(&LifecyclePhase::StreamPrepare)
            || journal.admission_transitions.get(original_id) != Some(&source.transition_id)
            || !matches!(original_response, ProviderResponse::Prepared { .. })
            || !composition::valid_callback_proof(&original_proof, original_response)
            || !composition::callback_approvals_valid(
                &original_proof,
                &original.selection.approvals,
                provider,
            )
        {
            return Err("release_prepare_callback_invalid".into());
        }
        if provider == *roots.first().unwrap()
            && finalized.selection.stage.as_ref().is_none_or(|stage| {
                stage.admission_id != original_id || stage.callback_proof != original_proof
            })
        {
            return Err("release_owner_prepare_mismatch".into());
        }
        let state = work
            .states
            .get(provider)
            .ok_or("release_callback_missing")?;
        let id = state
            .admission_id
            .as_deref()
            .ok_or("release_admission_missing")?;
        let admission = journal
            .authority
            .settled
            .get(id)
            .ok_or("release_admission_not_retained")?;
        let plan = state.plan.as_ref().ok_or("release_plan_missing")?;
        let response = state.response.as_ref().ok_or("release_response_missing")?;
        let ProviderResponse::Cleanup { outcomes, outputs } = response else {
            return Err("release_cleanup_required".into());
        };
        let proof_callback = composition::ProducerCallbackProof {
            participant: state.participant.clone(),
            request: state.request.clone(),
            terminal: state
                .terminal_dispatch
                .clone()
                .ok_or("release_callback_missing")?,
            pending_receipt: state.pending_receipt.clone(),
            pending_dispatch: state.pending_dispatch.clone(),
        };
        if state.phase != PreparePhase::CleanupSettled
            || state.provider != *provider
            || state.request.version != amaterasu_extension_api::COMPOSITION_VERSION
            || state.request.operation
                != amaterasu_extension_api::ProviderOperation::LifecycleCallback
            || state.request.phase != LifecyclePhase::StreamRelease
            || state.request.target != work.target
            || state.request.transition_id != work.transition_id
            || state.request.admission_id.as_deref() != Some(id)
            || state.request.approved_plan.as_ref() != Some(plan)
            || admission.provider != *provider
            || admission.plan != *plan
            || admission.selection.participant != state.participant
            || !session.admission_ids.contains(id)
            || session.admission_targets.get(id) != Some(&work.target)
            || session.admission_phases.get(id) != Some(&LifecyclePhase::StreamRelease)
            || journal.admission_transitions.get(id) != Some(&work.transition_id)
            || !composition::valid_callback_proof(&proof_callback, response)
            || !composition::callback_approvals_valid(
                &proof_callback,
                &admission.selection.approvals,
                provider,
            )
            || outcomes.len() != plan.obligations.len()
            || outcomes
                .iter()
                .any(|outcome| outcome.state != CleanupState::Released)
            || plan.obligations.iter().any(|obligation| {
                outcomes
                    .iter()
                    .filter(|outcome| outcome.obligation_id == obligation.obligation_id)
                    .count()
                    != 1
            })
        {
            return Err("release_callback_invalid".into());
        }
        // The exact original stream obligations remain live until this callback covers them.
        for original_id in &session.admission_ids {
            if session.admission_targets.get(original_id) != Some(&work.target)
                || session.admission_phases.get(original_id) != Some(&LifecyclePhase::StreamPrepare)
            {
                continue;
            }
            let original = journal
                .authority
                .settled
                .get(original_id)
                .or_else(|| journal.authority.admissions.get(original_id))
                .ok_or("original_admission_missing")?;
            if original.provider == *provider
                && original
                    .plan
                    .obligations
                    .iter()
                    .any(|obligation| !plan.obligations.contains(obligation))
            {
                return Err("release_obligation_missing".into());
            }
        }
        if provider == *roots.first().unwrap()
            && finalized
                .plan
                .obligations
                .iter()
                .any(|obligation| !plan.obligations.contains(obligation))
        {
            return Err("release_finalize_obligation_missing".into());
        }
        let before = current.clone();
        let mut confirmed = Vec::new();
        for proposal in &plan.control_proposals {
            let control = controls.next().ok_or("release_control_missing")?;
            let request = &control.dispatch.request;
            let super::control_dispatch::ControlPhase::Applied { result } = &control.dispatch.phase
            else {
                return Err("release_control_unsettled".into());
            };
            let settlement = control
                .settlement
                .as_ref()
                .ok_or("release_settlement_missing")?;
            let record = journal
                .control_settlements
                .get(&settlement.request.settlement_id)
                .ok_or("release_settlement_missing")?;
            let permit = journal
                .authority
                .control_permits
                .get(&request.permit_id)
                .ok_or("release_permit_missing")?;
            let selection = journal
                .composition
                .selection(
                    &work.target,
                    &record.selection_transition_id,
                    provider,
                    &request.proposal.owner_provider,
                    &request.proposal.output_id,
                )
                .ok_or("release_selection_missing")?;
            let access_level = |access: &amaterasu_extension_api::ResourceAccess| match access {
                amaterasu_extension_api::ResourceAccess::Read => 0,
                amaterasu_extension_api::ResourceAccess::Shared => 1,
                amaterasu_extension_api::ResourceAccess::Exclusive => 2,
            };
            let domains: BTreeSet<_> = request.proposal.mutation_domains.iter().cloned().collect();
            if request.proposal != *proposal
                || request.admission_id != id
                || request.target != work.target
                || request.transition_id != work.transition_id
                || !selection.validate_structure()
                || request.proposal.method != selection.control_policy.owner_method
                || request.proposal.owner_service != selection.control_policy.owner_service
                || request.proposal.argument_schema != selection.control_policy.argument_schema
                || request.proposal.owner_provider != selection.control_policy.owner_provider
                || selection.control_policy.dependent_provider != *provider
                || current.producer != selection.control_policy.owner_provider
                || current.producer_service != selection.control_policy.owner_service
                || current.capability != selection.control_policy.output_capability
                || current.schema != selection.control_policy.output_schema
                || current.output_id != request.proposal.output_id
                || current.resource != request.proposal.resource
                || domains.is_empty()
                || domains.len() != request.proposal.mutation_domains.len()
                || domains != current.resource.mutation_domains.iter().cloned().collect()
                || (selection.control_policy.domain_scope
                    == crate::prepare_authority::DomainScope::Static
                    && !domains.is_subset(&selection.control_policy.mutation_domains))
                || access_level(&request.proposal.access)
                    > access_level(&selection.control_policy.max_access)
                || !graph
                    .dependencies
                    .get(provider)
                    .is_some_and(|dependencies| {
                        dependencies.contains(&request.proposal.owner_provider)
                    })
                || control.dispatch.dependency != current
                || permit.context.output != current
                || permit.context.policy != selection.control_policy
                || permit.context.target != work.target
                || permit.context.transition_id != work.transition_id
                || permit.context.parent_generation != Some(session.generation)
                || !permit.consumed
                || !permit.settled
                || permit.request != *request
                || !record.valid_under_permit(permit)
                || !matches!(
                    settlement.phase,
                    super::settlement_dispatch::SettlementPhase::Acknowledged
                )
                || record.settlement != settlement.request
                || record.confirmed != **result
                || record.owner_ack != settlement.owner_ack
                || record.owner_dispatch != settlement.dispatch
                || record.owner_ack.is_none()
                || record.owner_dispatch.is_none()
                || ![
                    &selection.control_approval,
                    &selection.control_status_approval,
                ]
                .iter()
                .any(|approval| {
                    approval.broker_approval == record.effect_dispatch.call.approval
                        && composition::approval_matches_call(
                            approval,
                            &record.effect_dispatch.call,
                        )
                })
                || record.effect_pending_proof.as_ref().is_some_and(|pending| {
                    pending.call.approval != selection.control_approval.broker_approval
                        || !composition::approval_matches_call(
                            &selection.control_approval,
                            &pending.call,
                        )
                })
                || record.owner_dispatch.as_ref().is_none_or(|dispatch| {
                    dispatch.call.approval != selection.settlement_approval.broker_approval
                        || !composition::approval_matches_call(
                            &selection.settlement_approval,
                            &dispatch.call,
                        )
                })
            {
                return Err("release_control_invalid".into());
            }
            amaterasu_extension_api::validate_resource_control_settlement_response(
                &record.settlement,
                record.owner_ack.as_ref().unwrap(),
            )?;
            let catalog = amaterasu_extension_api::schemas::SchemaCatalog::compile(
                selection
                    .schema_documents
                    .iter()
                    .map(|doc| (doc.reference.clone(), doc.document.as_str())),
            )?;
            catalog.validate(
                &request.proposal.argument_schema,
                &request.proposal.arguments,
            )?;
            catalog.validate(&result.output.schema, &result.output.data)?;
            amaterasu_extension_api::validate_output_refinement(&current, &result.output)?;
            current = result.output.clone();
            confirmed.push((**result).clone());
        }
        if state.request.control_results != confirmed
            || state.request.dependency_inputs.as_slice() != [before]
        {
            return Err("release_callback_inputs_mismatch".into());
        }
        if provider == *roots.first().unwrap() {
            let [released] = outputs.as_slice() else {
                return Err("release_owner_readback_missing".into());
            };
            amaterasu_extension_api::validate_cleanup_output(provider, &current, released)?;
            let catalog = amaterasu_extension_api::schemas::SchemaCatalog::compile(
                finalized
                    .selection
                    .schema_documents
                    .iter()
                    .map(|doc| (doc.reference.clone(), doc.document.as_str())),
            )?;
            catalog.validate(&released.schema, &released.data)?;
            if released != &proof.released_output {
                return Err("release_readback_mismatch".into());
            }
        } else if !outputs.is_empty() {
            return Err("release_foreign_output".into());
        }
    }
    if controls.next().is_some() {
        return Err("release_extra_controls".into());
    }
    Ok(())
}

/// Caller must atomically save this mutation with stream retirement and work progress.
#[allow(dead_code)] // The release scheduler owns the atomic completion hook.
pub(super) fn record_composed_release_proof(
    journal: &mut Journal,
    work: &Work,
) -> ModuleResult<()> {
    if !journal.receipts.values().any(|receipt| {
        receipt.work.as_ref().is_some_and(|saved| {
            serde_json::to_value(saved).ok() == serde_json::to_value(work).ok()
        })
    }) {
        return Err("release_work_not_journaled".into());
    }
    let mut results = journal
        .finalize_results
        .records()
        .filter(|result| result.request.target == work.target);
    let finalized = results.next().ok_or("release_finalize_missing")?;
    if results.next().is_some() {
        return Err("release_finalize_ambiguous".into());
    }
    let owner = &finalized.selection.policy.owner_provider;
    let Some(ProviderResponse::Cleanup { outputs, .. }) = work
        .states
        .get(owner)
        .and_then(|state| state.response.as_ref())
    else {
        return Err("release_owner_callback_missing".into());
    };
    let [released] = outputs.as_slice() else {
        return Err("release_owner_readback_missing".into());
    };
    let proof = Proof {
        work: work.clone(),
        finalize_permit_id: finalized.request.permit_id.clone(),
        released_output: released.clone(),
    };
    verify(journal, &proof)?;
    if journal
        .composed_releases
        .proofs
        .iter()
        .any(|old| old.work.target == proof.work.target)
    {
        return Err("release_proof_duplicate".into());
    }
    if journal.composed_releases.proofs.len() >= MAX_PROOFS {
        return Err("release_proof_capacity".into());
    }
    journal.composed_releases.proofs.push(proof);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn missing_graph_evidence_cannot_retire_stream() {
        let journal: Journal = serde_json::from_value(
            json!({"version":3,"host_epoch":"epoch","receipts":{},"sessions":{}}),
        )
        .unwrap();
        let target: LifecycleTarget = serde_json::from_value(json!({"kind":"stream","host_epoch":"epoch","session_id":"session","stream_id":"stream","generation":1})).unwrap();
        assert!(
            !journal
                .composed_releases
                .has_valid_release(&journal, &target)
        );
        assert!(journal.verified_released_output(&target).is_none());
    }
}
