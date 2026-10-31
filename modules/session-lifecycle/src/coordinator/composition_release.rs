//! Reconstruct and freeze the owner input for reverse-graph stream restoration.
//!
//! The retained finalize result and acknowledged restoration records are proof
//! inputs only. Fresh observations, admission, permit and owner verification
//! are still required before a provider can perform a restoration effect.
use super::*;
use amaterasu_extension_api::{ResourceControlResponse, ResourceOutput};

fn validate_prior_cleanup(
    journal: &Journal,
    work: &Work,
    session: &SessionRecord,
) -> ModuleResult<()> {
    for provider in work.providers.iter().take(work.index) {
        let state = work
            .states
            .get(provider)
            .ok_or("release_predecessor_missing")?;
        let admission_id = state
            .admission_id
            .as_deref()
            .ok_or("release_admission_missing")?;
        let admission = journal
            .authority
            .admissions
            .get(admission_id)
            .or_else(|| journal.authority.settled.get(admission_id))
            .ok_or("release_admission_missing")?;
        let plan = state.plan.as_ref().ok_or("release_plan_missing")?;
        let response = state.response.as_ref().ok_or("release_cleanup_missing")?;
        let amaterasu_extension_api::ProviderResponse::Cleanup { outcomes, .. } = response else {
            return Err("release_cleanup_missing".into());
        };
        let proof = super::composition::ProducerCallbackProof {
            participant: state.participant.clone(),
            request: state.request.clone(),
            terminal: state
                .terminal_dispatch
                .clone()
                .ok_or("release_callback_unproven")?,
            pending_receipt: state.pending_receipt.clone(),
            pending_dispatch: state.pending_dispatch.clone(),
        };
        let controls: Vec<_> = work
            .composition_progress
            .as_ref()
            .into_iter()
            .flat_map(|progress| progress.controls.iter())
            .filter(|control| control.dispatch.request.admission_id == admission_id)
            .collect();
        if controls.len() != plan.control_proposals.len()
            || controls.len() != state.request.control_results.len()
            || controls.iter().zip(&plan.control_proposals).zip(&state.request.control_results)
                .any(|((control, proposal), confirmed)| {
                    control.dispatch.request.proposal != *proposal
                        || !matches!(&control.dispatch.phase, super::control_dispatch::ControlPhase::Applied { result } if result.as_ref() == confirmed)
                })
        {
            return Err("release_predecessor_controls_mismatch".into());
        }
        if state.provider != *provider
            || state.phase != crate::prepare_driver::PreparePhase::CleanupSettled
            || admission.provider != *provider
            || admission.plan != *plan
            || admission.selection.participant != state.participant
            || state.request.target != work.target
            || state.request.transition_id != work.transition_id
            || state.request.phase != LifecyclePhase::StreamRelease
            || state.request.approved_plan.as_ref() != Some(plan)
            || !session.admission_ids.contains(admission_id)
            || session.admission_targets.get(admission_id) != Some(&work.target)
            || session.admission_phases.get(admission_id) != Some(&LifecyclePhase::StreamRelease)
            || journal.admission_transitions.get(admission_id) != Some(&work.transition_id)
            || !super::composition::callback_approvals_valid(
                &proof,
                &admission.selection.approvals,
                provider,
            )
            || !super::composition::valid_callback_proof(&proof, response)
            || outcomes.len() != plan.obligations.len()
            || plan.obligations.iter().any(|obligation| {
                outcomes
                    .iter()
                    .filter(|outcome| {
                        outcome.obligation_id == obligation.obligation_id
                            && outcome.state == amaterasu_extension_api::CleanupState::Released
                    })
                    .count()
                    != 1
            })
        {
            return Err("release_predecessor_unproven".into());
        }
    }
    Ok(())
}

impl Coordinator {
    /// Reconstruct the exact authenticated owner binding for one release step.
    fn resolve_release_state(
        &self,
        work: &Work,
        dependent: &str,
        require_fresh_observation: bool,
    ) -> ModuleResult<(
        Option<super::composition::CompositionSelectionSnapshot>,
        ResourceOutput,
    )> {
        if work.phase != LifecyclePhase::StreamRelease || work.waiting_retirement {
            return Err("release_target_not_ready".into());
        }
        let graph = work
            .provider_graph
            .as_ref()
            .ok_or("provider_graph_missing")?;
        let reverse: Vec<_> = graph.order.iter().rev().cloned().collect();
        if graph.order.len() < 2
            || reverse != work.providers
            || work.providers.get(work.index).map(String::as_str) != Some(dependent)
        {
            return Err("release_graph_mismatch".into());
        }
        let dependencies = graph
            .dependencies
            .get(dependent)
            .ok_or("dependent_not_in_graph")?;
        let owner_step =
            dependencies.is_empty() && graph.order.first().is_some_and(|owner| owner == dependent);
        if dependencies.is_empty() && !owner_step {
            return Err("dependent_has_no_inputs".into());
        }
        let journal = self.shared.borrow().value.clone();
        let (epoch, session_id, stream_id, generation) = operations::target_parts(&work.target)?;
        let session = journal.sessions.get(session_id).ok_or("unknown_session")?;
        let stream = stream_id
            .and_then(|id| session.streams.get(id))
            .ok_or("unknown_stream")?;
        if epoch != journal.host_epoch
            || session.host_epoch != epoch
            || !matches!(
                session.phase,
                SessionPhase::Ready | SessionPhase::Paused | SessionPhase::Ending
            )
            || stream.generation != generation
            || stream.phase != StreamPhase::Quiescing
            || !stream.required.is_subset(&stream.retirements)
            || serde_json::to_value(&stream.provider_graph).ok()
                != serde_json::to_value(&work.provider_graph).ok()
        {
            return Err("release_consumers_not_retired".into());
        }
        validate_prior_cleanup(&journal, work, session)?;
        let mut results = journal.finalize_results.records().filter(|result| {
            result.request.target == work.target
                && if owner_step {
                    result.selection.policy.owner_provider == dependent
                } else {
                    dependencies.contains(&result.selection.policy.owner_provider)
                }
        });
        let result = results.next().ok_or("release_finalize_missing")?.clone();
        if results.next().is_some()
            || !result.validate_structure()
            || !result.validate_control_chain(&journal)
        {
            return Err("release_finalize_invalid_or_ambiguous".into());
        }
        drop(results);
        let finalized_permit = journal
            .authority
            .finalize_permits
            .get(&result.request.permit_id)
            .ok_or("release_finalize_permit_missing")?;
        let finalized_admission = journal
            .authority
            .admissions
            .get(&result.request.admission_id)
            .or_else(|| journal.authority.settled.get(&result.request.admission_id))
            .ok_or("release_finalize_admission_missing")?;
        let frozen = &result.selection;
        let resource_generation =
            super::resource_generation::current_root_generation(&journal, &frozen.input)?;
        if !finalized_permit.consumed
            || !finalized_permit.settled
            || finalized_permit.request != result.request
            || finalized_permit.context.policy != frozen.policy
            || finalized_permit.context.target != result.request.target
            || finalized_permit.context.transition_id != result.request.transition_id
            || finalized_permit.context.output != result.request.output
            || finalized_permit.context.control_results != result.request.control_results
            || finalized_permit.context.parent_generation != frozen.parent_generation
            || finalized_admission.provider != frozen.policy.owner_provider
            || finalized_admission.plan != result.plan
            || result.plan_approval.as_ref().is_none_or(|approval| {
                !finalized_admission.selection.approvals.contains(approval)
                    || approval.method != finalized_admission.selection.participant.plan_method
            })
            || !journal
                .authority
                .output_admissions
                .contains(&result.request.admission_id)
            || !session.admission_ids.contains(&result.request.admission_id)
            || session.admission_targets.get(&result.request.admission_id) != Some(&work.target)
            || session.admission_phases.get(&result.request.admission_id)
                != Some(&LifecyclePhase::StreamPrepare)
            || journal
                .admission_transitions
                .get(&result.request.admission_id)
                != Some(&result.request.transition_id)
            || journal.composition.finalize_selection(
                &frozen.target,
                &frozen.transition_id,
                &frozen.policy.owner_provider,
                &frozen.input.output.output_id,
            ) != Some(frozen)
            || !self.settings.finalize_policies.contains(&frozen.policy)
            || frozen
                .schema_documents
                .iter()
                .any(|document| !self.settings.schema_documents.contains(document))
            || result.plan_request.resource_generation != Some(resource_generation)
            || frozen.stage.as_ref().is_some_and(|stage| {
                stage.callback_proof.request.resource_generation != Some(resource_generation)
            })
        {
            return Err("release_finalize_authority_mismatch".into());
        }
        let ResourceControlResponse::Applied {
            output,
            candidate: Some(candidate),
        } = &result.applied
        else {
            return Err("release_finalize_not_applied".into());
        };
        if stream.binding_owner != frozen.policy.owner_provider
            || stream.candidate.as_ref() != Some(candidate.as_ref())
            || session.streams.iter().any(|(id, other)| {
                Some(id.as_str()) != stream_id
                    && other.binding_owner == frozen.policy.owner_provider
                    && other.phase != StreamPhase::Retired
                    && !(other.phase == StreamPhase::Quiescing
                        && other.required.is_subset(&other.retirements))
            })
        {
            return Err("release_finalize_binding_changed".into());
        }
        let mut current = output.as_ref().clone();
        let source = result
            .chain_work
            .as_ref()
            .ok_or("release_graph_proof_missing")?;
        let source_progress = source
            .composition_progress
            .as_ref()
            .ok_or("release_graph_proof_missing")?;
        if serde_json::to_value(&source.provider_graph).ok()
            != serde_json::to_value(&work.provider_graph).ok()
        {
            return Err("release_graph_mismatch".into());
        }
        if let Some(progress) = &work.composition_progress {
            for control in &progress.controls {
                let prior = work
                    .providers
                    .iter()
                    .take(work.index)
                    .filter_map(|provider| work.states.get(provider))
                    .find(|state| {
                        state.admission_id.as_deref()
                            == Some(control.dispatch.request.admission_id.as_str())
                    });
                let Some(prior) = prior else {
                    if work
                        .states
                        .get(dependent)
                        .and_then(|state| state.admission_id.as_deref())
                        == Some(control.dispatch.request.admission_id.as_str())
                    {
                        continue;
                    }
                    return Err("release_control_predecessor_missing".into());
                };
                if !prior.plan.as_ref().is_some_and(|plan| {
                    plan.control_proposals
                        .contains(&control.dispatch.request.proposal)
                }) {
                    return Err("release_control_predecessor_mismatch".into());
                }
                let super::control_dispatch::ControlPhase::Applied { result } =
                    &control.dispatch.phase
                else {
                    return Err("release_control_unsettled".into());
                };
                if prior
                    .request
                    .control_results
                    .iter()
                    .filter(|confirmed| *confirmed == result.as_ref())
                    .count()
                    != 1
                {
                    return Err("release_control_predecessor_mismatch".into());
                }
                let settlement = control
                    .settlement
                    .as_ref()
                    .ok_or("release_control_unsettled")?;
                let record = journal
                    .control_settlements
                    .get(&settlement.request.settlement_id)
                    .ok_or("release_settlement_missing")?;
                let permit = journal
                    .authority
                    .control_permits
                    .get(&control.dispatch.request.permit_id)
                    .ok_or("release_permit_missing")?;
                if !matches!(
                    settlement.phase,
                    super::settlement_dispatch::SettlementPhase::Acknowledged
                ) || control.dispatch.dependency != current
                    || settlement.request.request != control.dispatch.request
                    || settlement.request.output != result.output
                    || record.settlement != settlement.request
                    || record.confirmed != **result
                    || record.owner_ack != settlement.owner_ack
                    || record.owner_dispatch != settlement.dispatch
                    || !permit.consumed
                    || !permit.settled
                    || permit.request != control.dispatch.request
                    || permit.context.output != current
                    || !record.valid_under_permit(permit)
                {
                    return Err("release_settlement_mismatch".into());
                }
                amaterasu_extension_api::validate_resource_control_settlement_response(
                    &record.settlement,
                    record.owner_ack.as_ref().ok_or("release_ack_missing")?,
                )?;
                amaterasu_extension_api::validate_output_refinement(&current, &result.output)?;
                current = result.output.clone();
            }
        }
        let mut selected = source_progress.selections.iter().filter(|selection| {
            selection.control_policy.dependent_provider == dependent
                && selection.control_policy.owner_provider == current.producer
                && selection.input.output.output_id == current.output_id
        });
        let selection = if owner_step {
            None
        } else {
            let selection = selected.next().ok_or("release_selection_missing")?.clone();
            if selected.next().is_some()
                || !selection.validate_structure()
                || !dependencies.contains(&selection.control_policy.owner_provider)
                || !self
                    .settings
                    .control_policies
                    .contains(&selection.control_policy)
                || journal.composition.selection(
                    &selection.target,
                    &selection.transition_id,
                    dependent,
                    &selection.control_policy.owner_provider,
                    &selection.input.output.output_id,
                ) != Some(&selection)
            {
                return Err("release_selection_invalid".into());
            }
            Some(selection)
        };
        let owner = self
            .selections
            .get(&current.producer)
            .ok_or("release_owner_unavailable")?;
        if selection.as_ref().is_some_and(|selection| {
            !owner.approvals.contains(&selection.control_approval)
                || !owner.approvals.contains(&selection.control_status_approval)
                || !owner.approvals.contains(&selection.settlement_approval)
        }) || !owner.approvals.contains(&frozen.finalize_approval)
            || !owner.approvals.contains(&frozen.finalize_status_approval)
        {
            return Err("release_approval_changed".into());
        }
        if require_fresh_observation {
            let observed = journal
                .authority
                .snapshot
                .resources
                .get(&current.producer)
                .and_then(|resources| resources.get(&current.resource.resource_id))
                .ok_or("release_observation_missing")?;
            let domains: BTreeSet<_> = current
                .resource
                .mutation_domains
                .iter()
                .cloned()
                .chain(current.resource.mutation_domain.iter().cloned())
                .collect();
            if observed.provider != current.resource.provider
                || observed.incarnation != current.resource.incarnation
                || observed.revision != current.resource.revision
                || observed.execution_domain != current.resource.execution_domain
                || observed.mutation_domains != domains
            {
                return Err("release_observation_stale".into());
            }
            for observation in &current.domain_observations {
                let live = journal
                    .authority
                    .snapshot
                    .domains
                    .get(&observation.domain)
                    .ok_or("release_domain_missing")?;
                if !observation.complete
                    || journal
                        .authority
                        .refresh_required
                        .contains(&observation.domain)
                    || live.revision != observation.revision
                    || live.borrowers != observation.borrowers
                {
                    return Err("release_domain_stale".into());
                }
            }
        }
        self.settings
            .schema_catalog()?
            .validate(&current.schema, &current.data)?;
        Ok((selection, current))
    }

    /// Reconstruct a dependent's frozen policy and current input.
    pub(super) fn resolve_release_binding(
        &self,
        work: &Work,
        dependent: &str,
    ) -> ModuleResult<(
        super::composition::CompositionSelectionSnapshot,
        ResourceOutput,
    )> {
        let (selection, output) = self.resolve_release_state(work, dependent, true)?;
        Ok((selection.ok_or("release_selection_missing")?, output))
    }

    /// Reconstruct the final acknowledged input for the selected owner cleanup.
    pub(super) fn resolve_owner_release_input(
        &self,
        work: &Work,
        owner: &str,
    ) -> ModuleResult<ResourceOutput> {
        let (selection, output) = self.resolve_release_state(work, owner, true)?;
        if selection.is_some() || output.producer != owner {
            return Err("release_owner_mismatch".into());
        }
        Ok(output)
    }

    /// Reconstruct provenance for a read-only owner observation, before freshness exists.
    /// This result alone must never authorize an effect or freeze a dependent plan.
    pub(super) fn resolve_release_observation_binding(
        &self,
        work: &Work,
        participant: &str,
    ) -> ModuleResult<(
        Option<super::composition::CompositionSelectionSnapshot>,
        ResourceOutput,
    )> {
        self.resolve_release_state(work, participant, false)
    }

    /// Atomically freeze the reconstructed binding before the dependent plan.
    pub(super) fn freeze_release_inputs(
        &mut self,
        key: &str,
        work: &Work,
        dependent: &str,
    ) -> ModuleResult<Vec<ResourceOutput>> {
        let (selection, current) = self.resolve_release_binding(work, dependent)?;
        let mut journal = self.shared.borrow().value.clone();
        let saved = journal
            .receipts
            .get_mut(key)
            .and_then(|receipt| receipt.work.as_mut())
            .ok_or("transition_no_longer_active")?;
        if serde_json::to_value(&*saved).ok() != serde_json::to_value(work).ok()
            || saved.states.contains_key(dependent)
        {
            return Err("release_work_changed".into());
        }
        let progress = saved.composition_progress.get_or_insert_with(|| {
            super::composition_progress::CompositionProgress {
                inputs: vec![],
                selections: vec![],
                controls: vec![],
                finalize: None,
            }
        });
        if progress.finalize.is_some() {
            return Err("release_progress_invalid".into());
        }
        if !progress.inputs.contains(&selection.input) {
            progress.inputs.push(selection.input.clone());
        }
        if !progress.selections.contains(&selection) {
            progress.selections.push(selection);
        }
        save(&self.shared, journal)?;
        Ok(vec![current])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_freeze_rejects_wrong_phase_and_unordered_graph_without_mutation() {
        let root = std::env::temp_dir().join(format!(
            "amaterasu-release-freeze-{}-{}",
            std::process::id(),
            identifier().unwrap()
        ));
        std::fs::create_dir(&root).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let mut coordinator = Coordinator::open(&root.join("journal.json")).unwrap();
        let mut work: Work = serde_json::from_value(json!({
            "phase":"stream.prepare",
            "target":{"kind":"stream","host_epoch":"epoch","session_id":"session","stream_id":"stream","generation":1},
            "intent":null,"reason":null,"video":null,"transition_id":"transition",
            "providers":["owner","dependent"],"index":0,"states":{},"candidate":null,"waiting_retirement":false,
            "provider_graph":{"order":["owner","dependent"],"dependencies":{"owner":[],"dependent":["owner"]}}
        })).unwrap();
        let before = serde_json::to_value(&coordinator.shared.borrow().value).unwrap();
        assert!(
            coordinator
                .freeze_release_inputs("missing", &work, "dependent")
                .is_err()
        );
        assert!(
            coordinator
                .resolve_release_observation_binding(&work, "dependent")
                .is_err()
        );
        work.phase = LifecyclePhase::StreamRelease;
        assert!(
            coordinator
                .resolve_release_observation_binding(&work, "dependent")
                .is_err()
        );
        assert!(
            coordinator
                .freeze_release_inputs("missing", &work, "dependent")
                .is_err()
        );
        assert_eq!(
            before,
            serde_json::to_value(&coordinator.shared.borrow().value).unwrap()
        );
        drop(coordinator);
        std::fs::remove_dir_all(root).unwrap();
    }
}
