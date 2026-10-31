//! Retained, authenticated proof of standalone stream cleanup. Root session
//! output and all source admissions stay retained until separately retired.
use super::*;
use crate::prepare_driver::PreparePhase;
use amaterasu_extension_api::{CleanupState, ProviderResponse};

const MAX_RELEASES: usize = 64;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct StreamReleaseProof {
    target: LifecycleTarget,
    transition_id: String,
    provider: String,
    stage_admission_id: String,
    finalize_permit_id: String,
    required: BTreeSet<String>,
    retirements: BTreeSet<String>,
    callback: composition::ProducerCallbackProof,
    #[serde(default)]
    released_output: Option<amaterasu_extension_api::ResourceOutput>,
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct StreamReleaseBook {
    proofs: Vec<StreamReleaseProof>,
}
impl StreamReleaseBook {
    pub(super) fn targets(&self) -> impl Iterator<Item = &LifecycleTarget> {
        self.proofs.iter().map(|proof| &proof.target)
    }

    pub fn proofs(&self) -> impl Iterator<Item = &StreamReleaseProof> {
        self.proofs.iter()
    }
    pub fn validate(&self, journal: &Journal) -> bool {
        self.proofs.len() <= MAX_RELEASES
            && self.proofs.iter().enumerate().all(|(index, proof)| {
                validate_proof(journal, proof)
                    && !self.proofs[..index]
                        .iter()
                        .any(|old| old.target == proof.target)
            })
    }
    pub fn released_output_for_admission(
        &self,
        admission_id: &str,
    ) -> Option<&amaterasu_extension_api::ResourceOutput> {
        self.proofs
            .iter()
            .find(|proof| proof.callback.request.admission_id.as_deref() == Some(admission_id))?
            .released_output
            .as_ref()
    }
    pub fn released_output_for_target(
        &self,
        target: &LifecycleTarget,
    ) -> Option<(&str, &amaterasu_extension_api::ResourceOutput)> {
        let proof = self.proofs.iter().find(|proof| &proof.target == target)?;
        Some((
            proof.callback.request.admission_id.as_deref()?,
            proof.released_output.as_ref()?,
        ))
    }
    pub fn references_admission(&self, admission_id: &str) -> bool {
        self.proofs
            .iter()
            .any(|proof| proof.callback.request.admission_id.as_deref() == Some(admission_id))
    }
    pub fn has_released_for(
        &self,
        target: &LifecycleTarget,
        provider: &str,
        transition_id: &str,
    ) -> bool {
        self.proofs.iter().any(|proof| {
            &proof.target == target
                && proof.provider == provider
                && proof.transition_id == transition_id
        })
    }
    pub fn has_valid_released(&self, journal: &Journal, target: &LifecycleTarget) -> bool {
        let mut matches = self.proofs.iter().filter(|proof| &proof.target == target);
        let Some(proof) = matches.next() else {
            return false;
        };
        matches.next().is_none() && validate_proof(journal, proof)
    }
    pub fn has_released(&self, target: &LifecycleTarget) -> bool {
        self.proofs.iter().any(|proof| &proof.target == target)
    }
}

fn all_outcomes_released(
    plan: &amaterasu_extension_api::ResourcePlan,
    outcomes: &[amaterasu_extension_api::CleanupOutcome],
) -> bool {
    outcomes.len() == plan.obligations.len()
        && outcomes
            .iter()
            .all(|outcome| outcome.state == CleanupState::Released)
        && plan.obligations.iter().all(|obligation| {
            outcomes
                .iter()
                .filter(|outcome| outcome.obligation_id == obligation.obligation_id)
                .count()
                == 1
        })
}

fn validate_proof(journal: &Journal, proof: &StreamReleaseProof) -> bool {
    let Ok((epoch, session_id, Some(stream_id), generation)) =
        operations::target_parts(&proof.target)
    else {
        return false;
    };
    let Some(session) = journal.sessions.get(session_id) else {
        return false;
    };
    let Some(stream) = session.streams.get(stream_id) else {
        return false;
    };
    let Some(stage) = journal.composition.staged_outputs().find(|stage| {
        stage.target == proof.target && stage.admission_id == proof.stage_admission_id
    }) else {
        return false;
    };
    let Some(finalized) = journal.finalize_results.records().find(|result| {
        result.request.permit_id == proof.finalize_permit_id
            && result.request.target == proof.target
    }) else {
        return false;
    };
    let Some(admission_id) = proof.callback.request.admission_id.as_deref() else {
        return false;
    };
    let Some(admission) = journal
        .authority
        .admissions
        .get(admission_id)
        .or_else(|| journal.authority.settled.get(admission_id))
    else {
        return false;
    };
    let crate::provider_dispatch::DispatchState::Completed { payload } =
        &proof.callback.terminal.state
    else {
        return false;
    };
    let Ok(response) = serde_json::from_value::<ProviderResponse>(payload.clone()) else {
        return false;
    };
    let ProviderResponse::Cleanup { outcomes, outputs } = &response else {
        return false;
    };
    let Some(released_output) = &proof.released_output else {
        return false;
    };
    let amaterasu_extension_api::ResourceControlResponse::Applied {
        output: finalized_output,
        ..
    } = &finalized.applied
    else {
        return false;
    };
    let catalog = amaterasu_extension_api::schemas::SchemaCatalog::compile(
        finalized
            .selection
            .schema_documents
            .iter()
            .map(|doc| (doc.reference.clone(), doc.document.as_str())),
    );
    let required_obligations = stage
        .plan
        .obligations
        .iter()
        .chain(&finalized.plan.obligations);
    session.host_epoch == epoch
        && stream.generation == generation
        && outputs.as_slice() == [released_output.clone()]
        && amaterasu_extension_api::validate_cleanup_output(
            &proof.provider,
            finalized_output,
            released_output,
        )
        .is_ok()
        && catalog.is_ok_and(|catalog| {
            catalog
                .validate(&released_output.schema, &released_output.data)
                .is_ok()
        })
        && stream.binding_owner == proof.provider
        && stream.required == proof.required
        && proof.required.is_subset(&proof.retirements)
        && proof.retirements.is_subset(&stream.retirements)
        && proof.callback.participant == admission.selection.participant
        && admission.provider == proof.provider
        && stage.after.producer == proof.provider
        && finalized.selection.policy.owner_provider == proof.provider
        && finalized.selection.stage.as_ref() == Some(stage)
        && proof.callback.request.version == amaterasu_extension_api::COMPOSITION_VERSION
        && proof.callback.request.phase == LifecyclePhase::StreamRelease
        && proof.callback.request.target == proof.target
        && proof.callback.request.transition_id == proof.transition_id
        && proof.callback.request.dependency_inputs.as_slice()
            == [finalized_output.as_ref().clone()]
        && proof.callback.request.approved_plan.as_ref() == Some(&admission.plan)
        && session.admission_ids.contains(admission_id)
        && session.admission_targets.get(admission_id) == Some(&proof.target)
        && session.admission_phases.get(admission_id) == Some(&LifecyclePhase::StreamRelease)
        && journal.admission_transitions.get(admission_id) == Some(&proof.transition_id)
        && composition::callback_approvals_valid(
            &proof.callback,
            &admission.selection.approvals,
            &admission.provider,
        )
        && composition::valid_callback_proof(&proof.callback, &response)
        && all_outcomes_released(&admission.plan, outcomes)
        && required_obligations
            .clone()
            .all(|obligation| admission.plan.obligations.contains(obligation))
        && required_obligations.into_iter().all(|obligation| {
            outcomes.iter().any(|outcome| {
                outcome.obligation_id == obligation.obligation_id
                    && outcome.state == CleanupState::Released
            })
        })
}

/// Check trusted stream and owner lineage before any release effect is sent.
pub(super) fn validate_release_preflight(
    journal: &Journal,
    work: &Work,
    provider: &str,
) -> ModuleResult<()> {
    if work.phase != LifecyclePhase::StreamRelease {
        return Err("stream_release_required".into());
    }
    let (epoch, session_id, Some(stream_id), generation) = operations::target_parts(&work.target)?
    else {
        return Err("stream_target_required".into());
    };
    let session = journal.sessions.get(session_id).ok_or("unknown_session")?;
    let stream = session.streams.get(stream_id).ok_or("unknown_stream")?;
    if session.host_epoch != epoch
        || stream.generation != generation
        || stream.phase != StreamPhase::Quiescing
        || !stream.required.is_subset(&stream.retirements)
        || stream.binding_owner != provider
    {
        return Err("consumers_not_retired_or_stale".into());
    }
    let graph = work
        .provider_graph
        .as_ref()
        .ok_or("provider_graph_missing")?;
    if graph.order.len() != 1
        || graph.order[0] != provider
        || graph
            .dependencies
            .get(provider)
            .is_none_or(|deps| !deps.is_empty())
    {
        return Err("standalone_release_required".into());
    }
    let mut stages = journal
        .composition
        .staged_outputs()
        .filter(|stage| stage.target == work.target && stage.after.producer == provider);
    let stage = stages.next().ok_or("stage_missing")?;
    if stages.next().is_some()
        || journal
            .composition
            .staged_for(&work.target, provider, &stage.after.output_id)
            .ok()
            != Some(stage)
    {
        return Err("stage_ambiguous_or_stale".into());
    }
    let mut results = journal.finalize_results.records().filter(|record| {
        record.request.target == work.target && record.selection.policy.owner_provider == provider
    });
    let result = results.next().ok_or("finalize_result_missing")?;
    if results.next().is_some() || result.selection.stage.as_ref() != Some(stage) {
        return Err("finalize_result_ambiguous".into());
    }
    if matches!(&result.applied, amaterasu_extension_api::ResourceControlResponse::Applied { candidate: Some(candidate), .. } if stream.candidate.as_ref() == Some(candidate.as_ref()))
    {
        Ok(())
    } else {
        Err("finalize_candidate_mismatch".into())
    }
}

/// Mutates a private journal clone; caller atomically commits it with authority
/// and Work progress. References remain retained and therefore capacity-bounded.
pub(super) fn record_release_proof(
    journal: &mut Journal,
    work: &Work,
    provider: &str,
    state: &PrepareState,
) -> ModuleResult<()> {
    validate_release_preflight(journal, work, provider)?;
    if work.phase != LifecyclePhase::StreamRelease
        || state.phase != PreparePhase::CleanupSettled
        || state.provider != provider
        || state.request.target != work.target
        || state.request.transition_id != work.transition_id
        || !journal.receipts.values().any(|receipt| {
            receipt.work.as_ref().is_some_and(|saved| {
                serde_json::to_value(saved).ok() == serde_json::to_value(work).ok()
            })
        })
    {
        return Err("stream_release_context_mismatch".into());
    }
    let (_, session_id, Some(stream_id), _) = operations::target_parts(&work.target)? else {
        return Err("stream_target_required".into());
    };
    let session = journal.sessions.get(session_id).ok_or("unknown_session")?;
    let stream = session.streams.get(stream_id).ok_or("unknown_stream")?;
    if stream.phase != StreamPhase::Quiescing || !stream.required.is_subset(&stream.retirements) {
        return Err("consumers_not_retired".into());
    }
    let graph = work
        .provider_graph
        .as_ref()
        .ok_or("provider_graph_missing")?;
    if graph.order.len() != 1
        || graph.order[0] != provider
        || graph
            .dependencies
            .get(provider)
            .is_none_or(|deps| !deps.is_empty())
    {
        return Err("standalone_release_required".into());
    }
    let mut stages = journal
        .composition
        .staged_outputs()
        .filter(|stage| stage.target == work.target && stage.after.producer == provider);
    let stage = stages.next().ok_or("stage_missing")?;
    if stages.next().is_some() {
        return Err("stage_ambiguous".into());
    }
    let mut results = journal.finalize_results.records().filter(|record| {
        record.request.target == work.target && record.selection.policy.owner_provider == provider
    });
    let finalized = results.next().ok_or("finalize_result_missing")?;
    if results.next().is_some() {
        return Err("finalize_result_ambiguous".into());
    }
    let Some(ProviderResponse::Cleanup { outputs, .. }) = &state.response else {
        return Err("cleanup_response_missing".into());
    };
    let [released_output] = outputs.as_slice() else {
        return Err("cleanup_readback_required".into());
    };
    let proof = StreamReleaseProof {
        target: work.target.clone(),
        transition_id: work.transition_id.clone(),
        provider: provider.into(),
        stage_admission_id: stage.admission_id.clone(),
        finalize_permit_id: finalized.request.permit_id.clone(),
        required: stream.required.clone(),
        retirements: stream.retirements.clone(),
        released_output: Some(released_output.clone()),
        callback: composition::ProducerCallbackProof {
            participant: state.participant.clone(),
            request: state.request.clone(),
            terminal: state
                .terminal_dispatch
                .clone()
                .ok_or("cleanup_terminal_proof_missing")?,
            pending_receipt: state.pending_receipt.clone(),
            pending_dispatch: state.pending_dispatch.clone(),
        },
    };
    if !validate_proof(journal, &proof) {
        return Err("invalid_stream_release_proof".into());
    }
    if let Some(existing) = journal
        .stream_releases
        .proofs
        .iter()
        .find(|item| item.target == proof.target)
    {
        return if serde_json::to_value(existing).ok() == serde_json::to_value(&proof).ok() {
            Ok(())
        } else {
            Err("stream_release_conflict".into())
        };
    }
    if journal.stream_releases.proofs.len() >= MAX_RELEASES {
        return Err("stream_release_capacity".into());
    }
    journal.stream_releases.proofs.push(proof);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn cleanup_requires_every_original_obligation_once_and_released() {
        let plan: amaterasu_extension_api::ResourcePlan = serde_json::from_value(json!({"plan_id":"plan","revision":1,"resources":[],"mutation_domains":[],"required_capabilities":[],"constraints":{},"obligations":[{"obligation_id":"cleanup","provider":"owner","resource_id":"output","mutation_domain":null,"cleanup_method":{"service":"owner/2","method":"cleanup","interface_major":2,"interface_minor":0,"schema_revision":1},"payload":{},"depends_on":[]}],"affected_consumers":[],"consumer_change":"live","candidate_constraint":null,"allowed_degradation":[]})).unwrap();
        let released = amaterasu_extension_api::CleanupOutcome {
            obligation_id: "cleanup".into(),
            state: CleanupState::Released,
            reason: None,
        };
        assert!(all_outcomes_released(
            &plan,
            std::slice::from_ref(&released)
        ));
        assert!(!all_outcomes_released(&plan, &[]));
        let mut deferred = released.clone();
        deferred.state = CleanupState::DeferredRestoration;
        assert!(!all_outcomes_released(&plan, &[deferred]));
        let mut foreign = released;
        foreign.obligation_id = "foreign".into();
        assert!(!all_outcomes_released(&plan, &[foreign]));
    }
}
