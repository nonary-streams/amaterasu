//! Reconstruct the owner output from coordinator-retained settlement proofs.
use super::finalize_selection::FinalizeSelectionSnapshot;
use super::*;
use crate::prepare_driver::PreparePhase;
use amaterasu_extension_api::{ConfirmedControl, ProviderResponse, ResourceOutput};

/// The caller must independently recheck installed grants, schema and fresh
/// observations before admitting a new owner plan or effect.
pub(super) fn resolve_control_chain(
    journal: &Journal,
    work: &Work,
    selection: &FinalizeSelectionSnapshot,
) -> ModuleResult<(ResourceOutput, Vec<ConfirmedControl>)> {
    if journal.composition.get(
        &selection.input.output.producer,
        &selection.input.output.output_id,
        &selection.input.output.origin,
    ) != Some(&selection.input)
    {
        return Err("composition_chain_context_invalid".into());
    }
    let current =
        super::finalize_backend::authenticated_current_output(&journal.composition, selection)?
            .clone();
    resolve_chain(journal, work, selection, current)
}

/// Validate immutable historical evidence only; this must never authorize an effect.
pub(super) fn resolve_historical_control_chain(
    journal: &Journal,
    work: &Work,
    selection: &FinalizeSelectionSnapshot,
) -> ModuleResult<(ResourceOutput, Vec<ConfirmedControl>)> {
    let mut matches = journal.composition.records().filter(|record| {
        record.output.producer == selection.input.output.producer
            && record.output.output_id == selection.input.output.output_id
            && record.output.origin == selection.input.output.origin
    });
    let root = matches.next().ok_or("historical_root_missing")?;
    if matches.next().is_some() {
        return Err("historical_root_ambiguous".into());
    }
    let mut original = root.clone();
    original.state = super::composition::OutputState::Retained;
    if original != selection.input {
        return Err("historical_root_mismatch".into());
    }
    if root.state == super::composition::OutputState::Released
        && !journal.session_releases.has_valid_released(
            journal,
            &root.output.origin,
            &root.output.producer,
        )
    {
        return Err("historical_root_release_unproven".into());
    }
    let stage = selection.stage.as_ref().ok_or("historical_stage_missing")?;
    if !journal
        .composition
        .staged_outputs()
        .any(|saved| saved == stage)
    {
        return Err("historical_stage_mismatch".into());
    }
    resolve_chain(journal, work, selection, stage.after.clone())
}

fn resolve_chain(
    journal: &Journal,
    work: &Work,
    selection: &FinalizeSelectionSnapshot,
    mut current: ResourceOutput,
) -> ModuleResult<(ResourceOutput, Vec<ConfirmedControl>)> {
    let graph = work
        .provider_graph
        .as_ref()
        .ok_or("provider_graph_missing")?;
    let (epoch, session_id, Some(stream_id), generation) =
        super::operations::target_parts(&work.target)?
    else {
        return Err("composition_target_invalid".into());
    };
    let session = journal.sessions.get(session_id).ok_or("unknown_session")?;
    let stream = session.streams.get(stream_id).ok_or("unknown_stream")?;
    if work.phase != LifecyclePhase::StreamPrepare
        || session.host_epoch != epoch
        || stream.generation != generation
        || serde_json::to_value(&stream.provider_graph).ok()
            != serde_json::to_value(&work.provider_graph).ok()
        || work.target != selection.target
        || work.transition_id != selection.transition_id
        || work.index < work.providers.len()
        || graph.order != work.providers
        || !selection.validate_structure()
        || !journal.control_settlements.validate_structure()
    {
        return Err("composition_chain_context_invalid".into());
    }
    let progress = work
        .composition_progress
        .as_ref()
        .ok_or("composition_progress_missing")?;
    let owner = &selection.policy.owner_provider;
    let mut expected = Vec::new();
    for provider in &graph.order {
        let state = work
            .states
            .get(provider)
            .ok_or("composition_participant_missing")?;
        if state.provider != *provider || state.phase != PreparePhase::Prepared {
            return Err("composition_participant_not_prepared".into());
        }
        let plan = state.plan.as_ref().ok_or("composition_plan_missing")?;
        for proposal in &plan.control_proposals {
            if proposal.owner_provider != *owner || proposal.output_id != current.output_id {
                return Err("composition_control_outside_owner".into());
            }
            expected.push((state, proposal));
        }
    }
    if expected.len() != progress.controls.len() {
        return Err("composition_control_count_mismatch".into());
    }
    let mut results = Vec::new();
    for ((state, proposal), control) in expected.into_iter().zip(&progress.controls) {
        let request = &control.dispatch.request;
        if request.proposal != *proposal
            || state.admission_id.as_deref() != Some(request.admission_id.as_str())
            || request.target != work.target
            || request.transition_id != work.transition_id
            || control.dispatch.dependency != current
        {
            return Err("composition_control_order_mismatch".into());
        }
        let super::control_dispatch::ControlPhase::Applied { result } = &control.dispatch.phase
        else {
            return Err("control_not_applied".into());
        };
        let settlement = control.settlement.as_ref().ok_or("control_not_settled")?;
        if !matches!(
            settlement.phase,
            super::settlement_dispatch::SettlementPhase::Acknowledged
        ) || settlement.request.request != *request
            || settlement.request.output != result.output
        {
            return Err("control_ack_mismatch".into());
        }
        let record = journal
            .control_settlements
            .get(&settlement.request.settlement_id)
            .ok_or("control_settlement_unavailable")?;
        let permit = journal
            .authority
            .control_permits
            .get(&request.permit_id)
            .ok_or("control_permit_unavailable")?;
        if record.settlement != settlement.request
            || record.confirmed != **result
            || record.owner_ack != settlement.owner_ack
            || record.owner_dispatch != settlement.dispatch
            || record.selection_transition_id != work.transition_id
            || permit.context.target != work.target
            || permit.context.transition_id != work.transition_id
            || permit.context.policy.dependent_provider != state.provider
            || !permit.consumed
            || !permit.settled
            || permit.request != *request
            || permit.context.output != current
            || !record.valid_under_permit(permit)
        {
            return Err("control_settlement_mismatch".into());
        }
        let ack = record.owner_ack.as_ref().ok_or("control_ack_missing")?;
        amaterasu_extension_api::validate_resource_control_settlement_response(
            &record.settlement,
            ack,
        )?;
        amaterasu_extension_api::validate_output_refinement(&current, &result.output)?;
        current = result.output.clone();
        results.push((**result).clone());
    }
    // Each dependent callback must have consumed its own acknowledged controls.
    for provider in &graph.order {
        let state = &work.states[provider];
        let plan = state.plan.as_ref().ok_or("composition_plan_missing")?;
        if plan.control_proposals.is_empty() {
            continue;
        }
        let own: Vec<_> = progress
            .controls
            .iter()
            .filter(|control| {
                state.admission_id.as_deref()
                    == Some(control.dispatch.request.admission_id.as_str())
            })
            .map(|control| match &control.dispatch.phase {
                super::control_dispatch::ControlPhase::Applied { result } => Ok((**result).clone()),
                _ => Err("control_not_applied"),
            })
            .collect::<Result<_, _>>()?;
        let response = state
            .response
            .as_ref()
            .ok_or("dependent_callback_missing")?;
        let terminal = state
            .terminal_dispatch
            .as_ref()
            .ok_or("dependent_callback_unproven")?;
        let proof = super::composition::ProducerCallbackProof {
            participant: state.participant.clone(),
            request: state.request.clone(),
            terminal: terminal.clone(),
            pending_receipt: state.pending_receipt.clone(),
            pending_dispatch: state.pending_dispatch.clone(),
        };
        let admission_id = state
            .admission_id
            .as_ref()
            .ok_or("dependent_admission_missing")?;
        let admission = journal
            .authority
            .settled
            .get(admission_id)
            .or_else(|| journal.authority.admissions.get(admission_id))
            .ok_or("dependent_admission_missing")?;
        if admission.provider != *provider
            || admission.plan != *plan
            || !session.admission_ids.contains(admission_id)
            || session.admission_targets.get(admission_id) != Some(&work.target)
            || session.admission_phases.get(admission_id) != Some(&work.phase)
            || journal.admission_transitions.get(admission_id) != Some(&work.transition_id)
            || state.request.admission_id.as_ref() != Some(admission_id)
            || state.request.approved_plan.as_ref() != Some(plan)
            || admission.selection.participant != state.participant
            || !super::composition::callback_approvals_valid(
                &proof,
                &admission.selection.approvals,
                provider,
            )
            || state.request.control_results != own
            || state.request.target != work.target
            || state.request.transition_id != work.transition_id
            || !matches!(response, ProviderResponse::Prepared { .. })
            || amaterasu_extension_api::validate_provider_response(&state.request, response)
                .is_err()
            || !super::composition::valid_callback_proof(&proof, response)
        {
            return Err("dependent_callback_unproven".into());
        }
    }
    Ok((current, results))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn incomplete_or_unjournaled_graph_cannot_authorize_finalization() {
        let selection = super::super::finalize_selection::tests::fixture();
        let journal: Journal = serde_json::from_value(json!({
            "version": 3, "host_epoch": "epoch", "receipts": {}, "sessions": {}
        }))
        .unwrap();
        let mut work: Work = serde_json::from_value(json!({
            "phase": "stream.prepare", "target": selection.target,
            "intent": "initial", "reason": null, "video": null,
            "transition_id": selection.transition_id,
            "providers": ["owner", "dependent"],
            "provider_graph": {"order": ["owner", "dependent"], "dependencies": {"owner": [], "dependent": ["owner"]}},
            "index": 2, "states": {}, "candidate": null, "waiting_retirement": false
        }))
        .unwrap();
        assert!(resolve_control_chain(&journal, &work, &selection).is_err());
        work.index = 1;
        assert!(resolve_control_chain(&journal, &work, &selection).is_err());
        work.target = LifecycleTarget::Stream {
            host_epoch: "other-epoch".into(),
            session_id: "session".into(),
            stream_id: "stream".into(),
            generation: 1,
        };
        assert!(resolve_control_chain(&journal, &work, &selection).is_err());
    }
}
