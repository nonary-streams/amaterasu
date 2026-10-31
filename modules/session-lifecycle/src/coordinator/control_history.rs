//! Reconstruct historical control inputs from frozen coordinator evidence.
//! This read-only verifier never authorizes an effect.
use super::*;
use amaterasu_extension_api::{ResourceControlRequest, ResourceControlResponse, ResourceOutput};

pub(super) fn authenticated_input(
    journal: &Journal,
    request: &ResourceControlRequest,
    selection: &composition::CompositionSelectionSnapshot,
) -> Option<ResourceOutput> {
    let mut works = journal
        .receipts
        .values()
        .filter_map(|receipt| receipt.work.as_ref())
        .filter(|work| {
            work.target == request.target
                && work.transition_id == request.transition_id
                && matches!(
                    work.phase,
                    LifecyclePhase::StreamPrepare | LifecyclePhase::StreamRelease
                )
        });
    let live = works.next();
    if works.next().is_some() {
        return None;
    }
    let mut historical = journal.finalize_results.records().filter(|record| {
        record.request.target == request.target
            && record.chain_work.as_ref().is_some_and(|work| {
                work.target == request.target && work.transition_id == request.transition_id
            })
    });
    let historical_record = historical.next();
    if historical.next().is_some() {
        return None;
    }
    let closed_release = journal.composed_releases.verified_work_for(
        journal,
        &request.target,
        &request.transition_id,
    );
    if let (Some(live), Some(closed)) = (live, closed_release)
        && serde_json::to_value(live).ok() != serde_json::to_value(closed).ok()
    {
        return None;
    }
    let work = if let Some(work) = live {
        Some(work)
    } else if let Some(work) = closed_release {
        Some(work)
    } else if let Some(record) = historical_record {
        let permit = journal
            .authority
            .finalize_permits
            .get(&record.request.permit_id)?;
        if !record.validate_structure()
            || !record.validate_control_chain(journal)
            || !permit.consumed
            || !permit.settled
            || permit.request != record.request
            || permit.context.output != record.request.output
            || permit.context.policy != record.selection.policy
            || journal.composition.finalize_selection(
                &record.selection.target,
                &record.selection.transition_id,
                &record.selection.policy.owner_provider,
                &record.selection.input.output.output_id,
            ) != Some(&record.selection)
        {
            return None;
        }
        record.chain_work.as_deref()
    } else {
        None
    };
    let Some(work) = work else {
        // Legacy direct-root controls may outlive their completed Work. They
        // can only refer to the coordinator's exact canonical root, and never
        // bypass an existing staged output or select caller-provided data.
        if journal.composition.staged_outputs().any(|stage| {
            stage.target == request.target
                && stage.after.producer == selection.input.output.producer
                && stage.after.output_id == selection.input.output.output_id
        }) || request.target != selection.target
            || request.proposal.resource != selection.input.output.resource
        {
            return None;
        }
        let root = journal.composition.records().find(|record| {
            record.output == selection.input.output
                && record.admission_id == selection.input.admission_id
                && record.transition_id == selection.input.transition_id
                && record.plan == selection.input.plan
                && record.callback_proof == selection.input.callback_proof
        })?;
        let admission = journal
            .authority
            .settled
            .get(&root.admission_id)
            .or_else(|| journal.authority.admissions.get(&root.admission_id))?;
        return composition::validate_callback_provenance(root, admission)
            .then(|| root.output.clone());
    };
    let (epoch, session_id, Some(stream_id), generation) =
        operations::target_parts(&work.target).ok()?
    else {
        return None;
    };
    let session = journal.sessions.get(session_id)?;
    let stream = session.streams.get(stream_id)?;
    let graph = work.provider_graph.as_ref()?;
    if epoch != session.host_epoch
        || stream.generation != generation
        || serde_json::to_value(&stream.provider_graph).ok()
            != serde_json::to_value(&work.provider_graph).ok()
        || (work.phase == LifecyclePhase::StreamPrepare && work.providers != graph.order)
        || (work.phase == LifecyclePhase::StreamRelease
            && work.providers != graph.order.iter().rev().cloned().collect::<Vec<_>>())
        || !graph
            .dependencies
            .get(&selection.control_policy.dependent_provider)
            .is_some_and(|deps| deps.contains(&selection.control_policy.owner_provider))
    {
        return None;
    }
    let mut stages = journal.composition.staged_outputs().filter(|stage| {
        stage.target == work.target
            && stage.after.producer == selection.input.output.producer
            && stage.after.output_id == selection.input.output.output_id
            && stage.after.origin == selection.input.output.origin
    });
    let stage = stages.next()?;
    if stages.next().is_some() || stage.parent_generation != session.generation {
        return None;
    }
    let stage_admission = journal
        .authority
        .admissions
        .get(&stage.admission_id)
        .or_else(|| journal.authority.settled.get(&stage.admission_id))?;
    let crate::provider_dispatch::DispatchState::Completed { payload } =
        &stage.callback_proof.terminal.state
    else {
        return None;
    };
    let stage_response: amaterasu_extension_api::ProviderResponse =
        serde_json::from_value(payload.clone()).ok()?;
    if stage_admission.provider != stage.after.producer
        || stage_admission.plan != stage.plan
        || stage_admission.selection.participant != stage.callback_proof.participant
        || !session.admission_ids.contains(&stage.admission_id)
        || session.admission_targets.get(&stage.admission_id) != Some(&stage.target)
        || session.admission_phases.get(&stage.admission_id) != Some(&LifecyclePhase::StreamPrepare)
        || journal.admission_transitions.get(&stage.admission_id) != Some(&stage.transition_id)
        || !matches!(&stage_response, amaterasu_extension_api::ProviderResponse::Prepared { outputs, .. } if outputs.contains(&stage.after))
        || !composition::valid_callback_proof(&stage.callback_proof, &stage_response)
        || !composition::callback_approvals_valid(
            &stage.callback_proof,
            &stage_admission.selection.approvals,
            &stage_admission.provider,
        )
    {
        return None;
    }
    let mut current = if work.phase == LifecyclePhase::StreamPrepare {
        stage.after.clone()
    } else {
        let mut results = journal.finalize_results.records().filter(|record| {
            record.request.target == work.target
                && record.selection.input == selection.input
                && record.selection.policy.owner_provider == request.proposal.owner_provider
        });
        let finalized = results.next()?;
        if results.next().is_some()
            || !finalized.validate_structure()
            || !finalized.validate_control_chain(journal)
            || finalized.selection.stage.as_ref() != Some(stage)
            || finalized.selection.policy != selection.finalize_policy
            || finalized.selection.finalize_approval != selection.finalize_approval
            || finalized.selection.finalize_status_approval != selection.finalize_status_approval
        {
            return None;
        }
        let permit = journal
            .authority
            .finalize_permits
            .get(&finalized.request.permit_id)?;
        let admission = journal
            .authority
            .settled
            .get(&finalized.request.admission_id)?;
        if !permit.consumed
            || !permit.settled
            || permit.request != finalized.request
            || permit.context.output != finalized.request.output
            || permit.context.policy != finalized.selection.policy
            || permit.context.target != finalized.request.target
            || permit.context.transition_id != finalized.request.transition_id
            || permit.context.control_results != finalized.request.control_results
            || permit.context.parent_generation != finalized.selection.parent_generation
            || admission.provider != finalized.selection.policy.owner_provider
            || admission.plan != finalized.plan
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
        {
            return None;
        }
        let ResourceControlResponse::Applied {
            output,
            candidate: Some(candidate),
        } = &finalized.applied
        else {
            return None;
        };
        if stream.candidate.as_ref() != Some(candidate.as_ref()) {
            return None;
        }
        output.as_ref().clone()
    };
    let progress = work.composition_progress.as_ref()?;
    let admission = journal
        .authority
        .admissions
        .get(&request.admission_id)
        .or_else(|| journal.authority.settled.get(&request.admission_id))?;
    let state = work.states.get(&admission.provider)?;
    if state.admission_id.as_deref() != Some(request.admission_id.as_str())
        || state.plan.as_ref() != Some(&admission.plan)
        || !session.admission_ids.contains(&request.admission_id)
        || session.admission_targets.get(&request.admission_id) != Some(&work.target)
        || session.admission_phases.get(&request.admission_id) != Some(&work.phase)
        || journal.admission_transitions.get(&request.admission_id) != Some(&work.transition_id)
        || progress
            .controls
            .iter()
            .filter(|control| control.dispatch.request == *request)
            .count()
            != 1
    {
        return None;
    }
    let position = work
        .providers
        .iter()
        .position(|provider| provider == &admission.provider)?;
    let proposal_index = admission
        .plan
        .control_proposals
        .iter()
        .position(|proposal| proposal == &request.proposal)?;
    let current_order = (position, proposal_index);
    let mut last = None;
    let mut found = false;
    for control in &progress.controls {
        let previous = &control.dispatch.request;
        if previous.proposal.owner_provider != request.proposal.owner_provider
            || previous.proposal.output_id != request.proposal.output_id
        {
            continue;
        }
        if previous == request {
            if found || control.dispatch.dependency != current {
                return None;
            }
            found = true;
            break;
        }
        let prior_admission = journal
            .authority
            .admissions
            .get(&previous.admission_id)
            .or_else(|| journal.authority.settled.get(&previous.admission_id))?;
        let order = (
            work.providers
                .iter()
                .position(|provider| provider == &prior_admission.provider)?,
            prior_admission
                .plan
                .control_proposals
                .iter()
                .position(|proposal| proposal == &previous.proposal)?,
        );
        if last.is_some_and(|prior| order <= prior)
            || order >= current_order
            || previous.target != work.target
            || previous.transition_id != work.transition_id
            || control.dispatch.dependency != current
        {
            return None;
        }
        let super::control_dispatch::ControlPhase::Applied { result } = &control.dispatch.phase
        else {
            return None;
        };
        let settlement = control.settlement.as_ref()?;
        if !matches!(
            settlement.phase,
            super::settlement_dispatch::SettlementPhase::Acknowledged
        ) {
            return None;
        }
        let record = journal
            .control_settlements
            .get(&settlement.request.settlement_id)?;
        let permit = journal.authority.control_permits.get(&previous.permit_id)?;
        let frozen = journal.composition.selection(
            &work.target,
            &record.selection_transition_id,
            &prior_admission.provider,
            &previous.proposal.owner_provider,
            &previous.proposal.output_id,
        )?;
        let prior_state = work.states.get(&prior_admission.provider)?;
        if prior_state.admission_id.as_deref() != Some(previous.admission_id.as_str())
            || prior_state.plan.as_ref() != Some(&prior_admission.plan)
            || !session.admission_ids.contains(&previous.admission_id)
            || session.admission_targets.get(&previous.admission_id) != Some(&work.target)
            || session.admission_phases.get(&previous.admission_id) != Some(&work.phase)
            || journal.admission_transitions.get(&previous.admission_id)
                != Some(&work.transition_id)
            || !permit.consumed
            || !permit.settled
            || permit.request != *previous
            || permit.context.output != current
            || permit.context.policy != frozen.control_policy
            || permit.context.target != work.target
            || permit.context.transition_id != work.transition_id
            || record.settlement != settlement.request
            || record.confirmed != **result
            || record.owner_ack != settlement.owner_ack
            || record.owner_dispatch != settlement.dispatch
            || record.owner_ack.is_none()
            || record.owner_dispatch.is_none()
            || !record.valid_under_permit(permit)
            || ![&frozen.control_approval, &frozen.control_status_approval]
                .iter()
                .any(|approval| {
                    approval.broker_approval == record.effect_dispatch.call.approval
                        && composition::approval_matches_call(
                            approval,
                            &record.effect_dispatch.call,
                        )
                })
            || record.effect_pending_proof.as_ref().is_some_and(|proof| {
                proof.call.approval != frozen.control_approval.broker_approval
                    || !composition::approval_matches_call(&frozen.control_approval, &proof.call)
            })
            || record.owner_dispatch.as_ref().is_none_or(|proof| {
                proof.call.approval != frozen.settlement_approval.broker_approval
                    || !composition::approval_matches_call(&frozen.settlement_approval, &proof.call)
            })
            || amaterasu_extension_api::validate_resource_control_settlement_response(
                &record.settlement,
                record.owner_ack.as_ref()?,
            )
            .is_err()
            || amaterasu_extension_api::validate_output_refinement(&current, &result.output)
                .is_err()
        {
            return None;
        }
        let catalog = amaterasu_extension_api::schemas::SchemaCatalog::compile(
            frozen
                .schema_documents
                .iter()
                .map(|doc| (doc.reference.clone(), doc.document.as_str())),
        )
        .ok()?;
        if catalog
            .validate(&result.output.schema, &result.output.data)
            .is_err()
        {
            return None;
        }
        current = result.output.clone();
        last = Some(order);
    }
    found.then_some(current)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn caller_supplied_output_without_journaled_work_is_never_authority() {
        let finalized = super::super::finalize_selection::tests::fixture();
        let owner_method = finalized.policy.method.clone();
        let status_method = finalized.policy.status_method.clone();
        let policy: crate::prepare_authority::ControlPolicy = serde_json::from_value(json!({
            "dependent_provider":"dependent", "owner_provider":finalized.policy.owner_provider,
            "owner_service":finalized.policy.owner_service, "owner_method":owner_method,
            "status_method":status_method, "settlement_method":owner_method,
            "argument_schema":finalized.input.output.schema, "output_capability":finalized.input.output.capability,
            "output_schema":finalized.input.output.schema, "max_access":"exclusive", "mutation_domains":[]
        })).unwrap();
        let selection = composition::CompositionSelectionSnapshot {
            target: finalized.target.clone(),
            transition_id: finalized.transition_id.clone(),
            input: finalized.input.clone(),
            control_policy: policy,
            finalize_policy: finalized.policy.clone(),
            control_approval: finalized.finalize_approval.clone(),
            control_status_approval: finalized.finalize_status_approval.clone(),
            settlement_approval: finalized.finalize_approval.clone(),
            finalize_approval: finalized.finalize_approval.clone(),
            finalize_status_approval: finalized.finalize_status_approval.clone(),
            schema_documents: finalized.schema_documents.clone(),
        };
        let request: ResourceControlRequest = serde_json::from_value(json!({
            "version":2,"transition_id":selection.transition_id,"operation_id":"effect","provider_sequence":1,
            "target":selection.target,"admission_id":"missing","permit_id":"missing","remaining_ms":1000,
            "proposal":{"proposal_id":"proposal","output_id":selection.input.output.output_id,
            "owner_provider":selection.input.output.producer,"owner_service":selection.input.output.producer_service,
            "target":selection.target,"resource":selection.input.output.resource,"method":owner_method,
            "argument_schema":selection.input.output.schema,"arguments":{},"access":"exclusive","mutation_domains":[]}
        })).unwrap();
        let journal: Journal = serde_json::from_value(
            json!({"version":3,"host_epoch":"epoch","receipts":{},"sessions":{}}),
        )
        .unwrap();
        assert!(authenticated_input(&journal, &request, &selection).is_none());
    }
}
