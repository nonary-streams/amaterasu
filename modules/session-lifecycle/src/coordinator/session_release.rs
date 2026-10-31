//! Standalone session teardown proof. Historical lineage remains retained;
//! native root output is marked released only with exact terminal evidence.
use super::*;
use crate::prepare_driver::PreparePhase;
use amaterasu_extension_api::{CleanupState, ProviderResponse};

const MAX_RELEASES: usize = 64;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SessionReleaseProof {
    target: LifecycleTarget,
    transition_id: String,
    provider: String,
    root_admission_id: String,
    callback: composition::ProducerCallbackProof,
}
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SessionReleaseBook {
    proofs: Vec<SessionReleaseProof>,
    #[serde(default)]
    noop_proofs: Vec<NoopReleaseProof>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NoopReleaseProof {
    target: LifecycleTarget,
    transition_id: String,
    provider: String,
    callback: composition::ProducerCallbackProof,
}
impl SessionReleaseBook {
    pub fn has_valid_released(
        &self,
        journal: &Journal,
        target: &LifecycleTarget,
        provider: &str,
    ) -> bool {
        self.proofs.iter().any(|proof| {
            &proof.target == target && proof.provider == provider && validate_proof(journal, proof)
        })
    }

    /// Direct terminal owner evidence only. Full book validation separately checks
    /// stream lineage and dependent cleanup; this avoids cyclic history checks.
    pub fn has_valid_owner_completion(
        &self,
        journal: &Journal,
        target: &LifecycleTarget,
        provider: &str,
    ) -> bool {
        let mut matches = self
            .proofs
            .iter()
            .filter(|proof| &proof.target == target && proof.provider == provider);
        let Some(proof) = matches.next() else {
            return false;
        };
        matches.next().is_none() && valid_owner_completion(journal, proof)
    }

    pub fn validate(&self, journal: &Journal) -> bool {
        self.proofs.len() <= MAX_RELEASES
            && self.noop_proofs.len() <= MAX_RELEASES
            && self.noop_proofs.iter().enumerate().all(|(index, proof)| {
                valid_noop_proof(journal, proof)
                    && !self.noop_proofs[..index]
                        .iter()
                        .any(|old| old.target == proof.target && old.provider == proof.provider)
            })
            && self.proofs.iter().enumerate().all(|(index, proof)| {
                validate_proof(journal, proof)
                    && !self.proofs[..index]
                        .iter()
                        .any(|old| old.target == proof.target)
            })
    }
    pub fn references_admission(&self, id: &str) -> bool {
        self.noop_proofs
            .iter()
            .any(|proof| proof.callback.request.admission_id.as_deref() == Some(id))
            || self.proofs.iter().any(|proof| {
                proof.root_admission_id == id
                    || proof.callback.request.admission_id.as_deref() == Some(id)
            })
    }
}

/// Exact retained release lineage used by the coordinator's closure transaction.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct ReleaseClosureEvidence {
    pub target: LifecycleTarget,
    pub graph: Value,
    pub root_output: amaterasu_extension_api::ResourceOutput,
    pub resource_generation: u64,
    pub owner: String,
    pub root_admission: String,
    pub owner_release_admission: String,
    pub transition_id: String,
    pub dependents: Vec<DependentReleaseEvidence>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct DependentReleaseEvidence {
    pub provider: String,
    pub prepare_admission: String,
    pub prepare_transition: String,
    pub release_admission: String,
    pub transition_id: String,
}

/// Validate all exact retained release records before any closure mutation.
pub(super) fn validated_release_closure(
    journal: &Journal,
    target: &LifecycleTarget,
) -> ModuleResult<ReleaseClosureEvidence> {
    let mut owners = journal
        .session_releases
        .proofs
        .iter()
        .filter(|proof| &proof.target == target);
    let owner = owners.next().ok_or("session_release_proof_missing")?;
    if owners.next().is_some() || !validate_proof(journal, owner) {
        return Err("session_release_proof_invalid".into());
    }
    let owner_release_admission = owner
        .callback
        .request
        .admission_id
        .clone()
        .ok_or("release_admission_missing")?;
    let (_, session_id, None, _) = operations::target_parts(target)? else {
        return Err("session_target_required".into());
    };
    let session = journal.sessions.get(session_id).ok_or("unknown_session")?;
    if session.phase != SessionPhase::Ended {
        return Err("session_not_ended".into());
    }
    let graph = session
        .provider_graph
        .as_ref()
        .ok_or("provider_graph_missing")?;
    let mut dependents = Vec::new();
    for proof in journal
        .session_releases
        .noop_proofs
        .iter()
        .filter(|proof| &proof.target == target)
    {
        if !valid_noop_proof(journal, proof)
            || proof.transition_id != owner.transition_id
            || !graph
                .order
                .iter()
                .skip(1)
                .any(|provider| provider == &proof.provider)
            || dependents
                .iter()
                .any(|entry: &DependentReleaseEvidence| entry.provider == proof.provider)
        {
            return Err("dependent_release_proof_invalid".into());
        }
        let (prepare_admission, prepare_transition) =
            session_prepare_evidence::admission_for(journal, target, &proof.provider)
                .ok_or("dependent_prepare_proof_missing")?;
        dependents.push(DependentReleaseEvidence {
            provider: proof.provider.clone(),
            prepare_admission: prepare_admission.into(),
            prepare_transition: prepare_transition.into(),
            release_admission: proof
                .callback
                .request
                .admission_id
                .clone()
                .ok_or("release_admission_missing")?,
            transition_id: proof.transition_id.clone(),
        });
    }
    if dependents.len() != graph.order.len().saturating_sub(1) {
        return Err("dependent_release_proof_missing".into());
    }
    dependents.sort_by(|a, b| a.provider.cmp(&b.provider));
    let mut roots = journal.composition.records().filter(|root| {
        root.admission_id == owner.root_admission_id
            && root.output.origin == *target
            && root.output.producer == owner.provider
    });
    let root = roots.next().ok_or("session_root_missing")?;
    if roots.next().is_some() {
        return Err("session_root_ambiguous".into());
    }
    let resource_generation = root
        .callback_proof
        .as_ref()
        .and_then(|proof| proof.request.resource_generation)
        .filter(|generation| *generation > 0)
        .ok_or("resource_generation_missing")?;
    Ok(ReleaseClosureEvidence {
        target: target.clone(),
        graph: serde_json::to_value(graph).map_err(|_| "invalid_provider_graph")?,
        root_output: root.output.clone(),
        resource_generation,
        owner: owner.provider.clone(),
        root_admission: owner.root_admission_id.clone(),
        owner_release_admission,
        transition_id: owner.transition_id.clone(),
        dependents,
    })
}

/// Remove proof records only as part of the caller's atomic closure transaction.
/// The caller must validate every other reference and mutate a private Journal clone.
pub(super) fn retire_closed_release(
    journal: &mut Journal,
    target: &LifecycleTarget,
    expected: &ReleaseClosureEvidence,
) -> ModuleResult<()> {
    if validated_release_closure(journal, target)? != *expected {
        return Err("session_release_closure_changed".into());
    }
    journal
        .session_releases
        .proofs
        .retain(|proof| &proof.target != target);
    journal
        .session_releases
        .noop_proofs
        .retain(|proof| &proof.target != target);
    Ok(())
}

/// Copy exact terminal callbacks only after the complete retained release
/// lineage has been revalidated against the private journal.
#[allow(
    dead_code,
    reason = "used by the ended-session certificate constructor"
)]
pub(super) fn validated_release_callbacks(
    journal: &Journal,
    target: &LifecycleTarget,
    expected: &ReleaseClosureEvidence,
) -> ModuleResult<(
    composition::ProducerCallbackProof,
    Vec<(String, composition::ProducerCallbackProof)>,
)> {
    if validated_release_closure(journal, target)? != *expected {
        return Err("session_release_closure_changed".into());
    }
    let owner = journal
        .session_releases
        .proofs
        .iter()
        .find(|proof| &proof.target == target && proof.provider == expected.owner)
        .ok_or("session_release_proof_missing")?;
    let mut dependents = Vec::new();
    for dependent in &expected.dependents {
        let proof = journal
            .session_releases
            .noop_proofs
            .iter()
            .find(|proof| {
                &proof.target == target
                    && proof.provider == dependent.provider
                    && proof.callback.request.admission_id.as_deref()
                        == Some(dependent.release_admission.as_str())
                    && proof.transition_id == dependent.transition_id
            })
            .ok_or("dependent_release_proof_missing")?;
        dependents.push((dependent.provider.clone(), proof.callback.clone()));
    }
    Ok((owner.callback.clone(), dependents))
}

fn session_lineage_closed(journal: &Journal, target: &LifecycleTarget, provider: &str) -> bool {
    let Ok((epoch, session_id, None, generation)) = operations::target_parts(target) else {
        return false;
    };
    let Some(session) = journal.sessions.get(session_id) else {
        return false;
    };
    if session.host_epoch != epoch
        || session.generation != generation
        || !session.application_retired
    {
        return false;
    }
    let stream_closed =
        |target: &LifecycleTarget| journal.verified_released_output(target).is_some();
    if session.streams.iter().any(|(id, stream)| {
        let target = LifecycleTarget::Stream {
            host_epoch: epoch.into(),
            session_id: session_id.into(),
            stream_id: id.clone(),
            generation: stream.generation,
        };
        stream.phase != StreamPhase::Retired || !stream_closed(&target)
    }) {
        return false;
    }
    if journal.composition.staged_outputs().any(|stage| {
        operations::target_parts(&stage.target).is_ok_and(|(_, id, _, _)| id == session_id)
            && !stream_closed(&stage.target)
    }) {
        return false;
    }
    if journal.finalize_results.records().any(|result| {
        operations::target_parts(&result.request.target).is_ok_and(|(_, id, _, _)| id == session_id)
            && !stream_closed(&result.request.target)
    }) {
        return false;
    }
    if journal.control_settlements.records().any(|record| {
        operations::target_parts(&record.settlement.request.target)
            .is_ok_and(|(_, id, _, _)| id == session_id)
            && !stream_closed(&record.settlement.request.target)
    }) {
        return false;
    }
    session
        .provider_graph
        .as_ref()
        .is_some_and(|graph| valid_release_graph(graph, provider))
}

// Only the qualified single owner or one explicit no-op dependent is supported.
fn valid_release_graph(graph: &ProviderGraph, owner: &str) -> bool {
    if graph.dependencies.len() != graph.order.len()
        || graph.order.first().map(String::as_str) != Some(owner)
        || graph
            .dependencies
            .get(owner)
            .is_none_or(|deps| !deps.is_empty())
    {
        return false;
    }
    match graph.order.as_slice() {
        [_] => true,
        [_, dependent] => {
            dependent != owner
                && graph
                    .dependencies
                    .get(dependent)
                    .is_some_and(|deps| deps == &[owner])
        }
        _ => false,
    }
}

fn valid_noop_proof(journal: &Journal, proof: &NoopReleaseProof) -> bool {
    let Ok((_, session_id, None, _)) = operations::target_parts(&proof.target) else {
        return false;
    };
    let Some(session) = journal.sessions.get(session_id) else {
        return false;
    };
    let Some(graph) = &session.provider_graph else {
        return false;
    };
    let [owner, dependent] = graph.order.as_slice() else {
        return false;
    };
    let Some(id) = proof.callback.request.admission_id.as_deref() else {
        return false;
    };
    let Some(admission) = journal
        .authority
        .settled
        .get(id)
        .or_else(|| journal.authority.admissions.get(id))
    else {
        return false;
    };
    let plan = &admission.plan;
    let crate::provider_dispatch::DispatchState::Completed { payload } =
        &proof.callback.terminal.state
    else {
        return false;
    };
    let Ok(response) = serde_json::from_value::<ProviderResponse>(payload.clone()) else {
        return false;
    };
    valid_release_graph(graph, owner)
        && dependent == &proof.provider
        && session_lineage_closed(journal, &proof.target, owner)
        && session_prepare_evidence::all_dependent_session_prepares_noop(
            journal,
            &proof.target,
            graph,
            owner,
        )
        && admission.provider == proof.provider
        && admission.selection.participant == proof.callback.participant
        && proof.callback.request.version == amaterasu_extension_api::COMPOSITION_VERSION
        && proof.callback.request.operation
            == amaterasu_extension_api::ProviderOperation::LifecycleCallback
        && proof.callback.request.phase == LifecyclePhase::SessionRelease
        && proof.callback.request.dependency_inputs.is_empty()
        && proof.callback.request.control_results.is_empty()
        && proof.callback.request.target == proof.target
        && proof.callback.request.transition_id == proof.transition_id
        && proof.callback.request.approved_plan.as_ref() == Some(plan)
        && session.admission_ids.contains(id)
        && session.admission_targets.get(id) == Some(&proof.target)
        && session.admission_phases.get(id) == Some(&LifecyclePhase::SessionRelease)
        && journal.admission_transitions.get(id) == Some(&proof.transition_id)
        && plan.resources.is_empty()
        && plan.mutation_domains.is_empty()
        && plan.obligations.is_empty()
        && plan.control_proposals.is_empty()
        && plan.candidate_constraint.is_none()
        && plan.affected_consumers.is_empty()
        && plan.consumer_change == amaterasu_extension_api::ConsumerChange::Live
        && matches!(&response, ProviderResponse::Cleanup { outcomes, outputs } if outcomes.is_empty() && outputs.is_empty())
        && composition::callback_approvals_valid(
            &proof.callback,
            &admission.selection.approvals,
            &proof.provider,
        )
        && composition::valid_callback_proof(&proof.callback, &response)
}

/// Match an exact original stream obligation to its authenticated owner cleanup.
/// This never infers a release from an obligation ID or a different target.
pub(super) fn proven_stream_obligation_released(
    journal: &Journal,
    session_target: &LifecycleTarget,
    obligation: &amaterasu_extension_api::ResourceObligation,
) -> bool {
    proven_stream_obligation_with(journal, session_target, obligation, |target| {
        journal
            .verified_released_output(target)
            .map(|(id, _)| id.to_owned())
    })
}

fn proven_stream_obligation_with(
    journal: &Journal,
    session_target: &LifecycleTarget,
    obligation: &amaterasu_extension_api::ResourceObligation,
    verified_release: impl Fn(&LifecycleTarget) -> Option<String>,
) -> bool {
    let Ok((epoch, session_id, None, generation)) = operations::target_parts(session_target) else {
        return false;
    };
    let Some(session) = journal.sessions.get(session_id) else {
        return false;
    };
    if session.host_epoch != epoch || session.generation != generation {
        return false;
    }
    let mut matches = 0;
    for (stream_id, stream) in &session.streams {
        if stream.phase != StreamPhase::Retired {
            continue;
        }
        let target = LifecycleTarget::Stream {
            host_epoch: epoch.into(),
            session_id: session_id.into(),
            stream_id: stream_id.clone(),
            generation: stream.generation,
        };
        let original = session.admission_ids.iter().any(|id| {
            session.admission_targets.get(id) == Some(&target)
                && session.admission_phases.get(id) == Some(&LifecyclePhase::StreamPrepare)
                && journal.authority.settled.get(id).is_some_and(|admission| {
                    admission.provider == obligation.provider
                        && admission.plan.obligations.contains(obligation)
                })
        });
        let released = verified_release(&target).is_some_and(|id| {
            (stream.binding_owner == obligation.provider
                && session.admission_ids.contains(&id)
                && session.admission_targets.get(&id) == Some(&target)
                && session.admission_phases.get(&id) == Some(&LifecyclePhase::StreamRelease)
                && journal.authority.settled.get(&id).is_some_and(|admission| {
                    admission.provider == obligation.provider
                        && admission.plan.obligations.contains(obligation)
                }))
                || journal
                    .composed_releases
                    .has_valid_released_obligation(journal, &target, obligation)
        });
        if original && released {
            matches += 1;
        }
    }
    matches == 1
}

fn valid_root_provenance(journal: &Journal, root: &composition::StoredOutput) -> bool {
    let Ok((_, session_id, None, _)) = operations::target_parts(&root.output.origin) else {
        return false;
    };
    let Some(session) = journal.sessions.get(session_id) else {
        return false;
    };
    let Some(admission) = journal
        .authority
        .admissions
        .get(&root.admission_id)
        .or_else(|| journal.authority.settled.get(&root.admission_id))
    else {
        return false;
    };
    session.admission_ids.contains(&root.admission_id)
        && session.admission_targets.get(&root.admission_id) == Some(&root.output.origin)
        && session.admission_phases.get(&root.admission_id) == Some(&LifecyclePhase::SessionPrepare)
        && journal.admission_transitions.get(&root.admission_id) == Some(&root.transition_id)
        && admission.provider == root.output.producer
        && admission.plan == root.plan
        && composition::validate_callback_provenance(root, admission)
}

/// Resolve the latest authenticated stream-release readback for an exact root.
/// This is called only from full validation or before dispatch. The direct owner
/// completion verifier deliberately avoids this lookup to prevent proof cycles.
pub(super) fn session_release_input(
    journal: &Journal,
    root: &composition::StoredOutput,
) -> ModuleResult<amaterasu_extension_api::ResourceOutput> {
    if !journal.composition.records().any(|record| record == root)
        || !valid_root_provenance(journal, root)
    {
        return Err("session_root_provenance_invalid".into());
    }
    if !journal.validate_composition() {
        return Err("composition_history_invalid".into());
    }
    let has_live_stage = journal.composition.staged_outputs().any(|stage| {
        stage.after.producer == root.output.producer
            && stage.after.output_id == root.output.output_id
            && stage.after.origin == root.output.origin
    });
    if !has_live_stage && let Some(anchor) = journal.composition.stage_anchor(&root.output) {
        let (admission, output) = journal
            .verified_released_output(&anchor.target)
            .ok_or("prior_stream_release_readback_missing")?;
        return validated_anchor_input(root, anchor, admission, output);
    }
    select_release_input(root, journal.composition.staged_outputs().rev(), |target| {
        journal
            .verified_released_output(target)
            .map(|(_, output)| output.clone())
    })
}

fn validated_anchor_input(
    root: &composition::StoredOutput,
    anchor: &composition::StageAnchor,
    admission: &str,
    output: &amaterasu_extension_api::ResourceOutput,
) -> ModuleResult<amaterasu_extension_api::ResourceOutput> {
    if admission != anchor.release_admission || output != &anchor.released {
        return Err("prior_stream_release_anchor_mismatch".into());
    }
    amaterasu_extension_api::validate_output_refinement(&root.output, output)
        .map_err(|_| "session_release_readback_mismatch")?;
    Ok(output.clone())
}

fn select_release_input<'a>(
    root: &composition::StoredOutput,
    stages: impl Iterator<Item = &'a composition::StagedOutputRefinement>,
    released: impl Fn(&LifecycleTarget) -> Option<amaterasu_extension_api::ResourceOutput>,
) -> ModuleResult<amaterasu_extension_api::ResourceOutput> {
    let matching: Vec<_> = stages
        .filter(|stage| {
            stage.after.producer == root.output.producer
                && stage.after.output_id == root.output.output_id
                && stage.after.origin == root.output.origin
        })
        .collect();
    if matching.iter().enumerate().any(|(index, stage)| {
        matching[..index]
            .iter()
            .any(|old| old.target == stage.target)
    }) {
        return Err("session_release_stage_ambiguous".into());
    }
    let Some(stage) = matching.first() else {
        return Ok(root.output.clone());
    };
    let output = released(&stage.target).ok_or("prior_stream_release_readback_missing")?;
    amaterasu_extension_api::validate_output_refinement(&root.output, &output)
        .map_err(|_| "session_release_readback_mismatch")?;
    Ok(output)
}

fn validate_proof(journal: &Journal, proof: &SessionReleaseProof) -> bool {
    let Ok((_, session_id, None, _)) = operations::target_parts(&proof.target) else {
        return false;
    };
    let Some(session) = journal.sessions.get(session_id) else {
        return false;
    };
    session_lineage_closed(journal, &proof.target, &proof.provider)
        && session
            .provider_graph
            .as_ref()
            .is_some_and(|graph| match graph.order.as_slice() {
                [_] => true,
                [_, dependent] => journal.session_releases.noop_proofs.iter().any(|item| {
                    item.target == proof.target
                        && item.provider == *dependent
                        && item.transition_id == proof.transition_id
                        && valid_noop_proof(journal, item)
                }),
                _ => false,
            })
        && valid_owner_completion(journal, proof)
        && journal
            .composition
            .records()
            .find(|root| {
                root.admission_id == proof.root_admission_id
                    && root.output.origin == proof.target
                    && root.output.producer == proof.provider
            })
            .and_then(|root| session_release_input(journal, root).ok())
            .is_some_and(|input| proof.callback.request.dependency_inputs == [input])
}

fn valid_owner_completion(journal: &Journal, proof: &SessionReleaseProof) -> bool {
    let Ok((_, session_id, None, _)) = operations::target_parts(&proof.target) else {
        return false;
    };
    let Some(session) = journal.sessions.get(session_id) else {
        return false;
    };
    let Some(root) = journal.composition.records().find(|record| {
        record.admission_id == proof.root_admission_id
            && record.output.origin == proof.target
            && record.output.producer == proof.provider
    }) else {
        return false;
    };
    let Some(release_id) = proof.callback.request.admission_id.as_deref() else {
        return false;
    };
    let Some(admission) = journal
        .authority
        .admissions
        .get(release_id)
        .or_else(|| journal.authority.settled.get(release_id))
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
    let ProviderResponse::Cleanup { outcomes, .. } = &response else {
        return false;
    };
    root.state == composition::OutputState::Released
        && valid_root_provenance(journal, root)
        && proof.callback.participant == admission.selection.participant
        && admission.provider == proof.provider
        && amaterasu_extension_api::validate_provider_request(&proof.callback.request).is_ok()
        && proof.callback.request.operation
            == amaterasu_extension_api::ProviderOperation::LifecycleCallback
        && proof.callback.request.version == amaterasu_extension_api::COMPOSITION_VERSION
        && proof.callback.request.phase == LifecyclePhase::SessionRelease
        && proof.callback.request.target == proof.target
        && proof.callback.request.transition_id == proof.transition_id
        && proof.callback.request.approved_plan.as_ref() == Some(&admission.plan)
        && matches!(proof.callback.request.dependency_inputs.as_slice(), [input] if amaterasu_extension_api::validate_output_refinement(&root.output, input).is_ok())
        && proof
            .callback
            .request
            .resource_generation
            .is_some_and(|generation| {
                generation > 0
                    && root
                        .callback_proof
                        .as_ref()
                        .and_then(|source| source.request.resource_generation)
                        == Some(generation)
            })
        && proof.callback.request.control_results.is_empty()
        && session.admission_ids.contains(release_id)
        && session.admission_targets.get(release_id) == Some(&proof.target)
        && session.admission_phases.get(release_id) == Some(&LifecyclePhase::SessionRelease)
        && journal.admission_transitions.get(release_id) == Some(&proof.transition_id)
        && composition::callback_approvals_valid(
            &proof.callback,
            &admission.selection.approvals,
            &admission.provider,
        )
        && composition::valid_callback_proof(&proof.callback, &response)
        && outcomes.len() == admission.plan.obligations.len()
        && outcomes
            .iter()
            .all(|outcome| outcome.state == CleanupState::Released)
        && admission.plan.obligations.iter().all(|obligation| {
            outcomes
                .iter()
                .filter(|outcome| outcome.obligation_id == obligation.obligation_id)
                .count()
                == 1
        })
        && root.plan.obligations.iter().all(|obligation| {
            admission.plan.obligations.contains(obligation)
                && outcomes.iter().any(|outcome| {
                    outcome.obligation_id == obligation.obligation_id
                        && outcome.state == CleanupState::Released
                })
        })
}

pub(super) fn validate_session_release_preflight(
    journal: &Journal,
    work: &Work,
    provider: &str,
) -> ModuleResult<()> {
    let (_, session_id, None, _) = operations::target_parts(&work.target)? else {
        return Err("session_target_required".into());
    };
    let session = journal.sessions.get(session_id).ok_or("unknown_session")?;
    let graph = work
        .provider_graph
        .as_ref()
        .ok_or("provider_graph_missing")?;
    let owner = graph.order.first().ok_or("provider_graph_missing")?;
    if work.phase != LifecyclePhase::SessionRelease
        || !session_lineage_closed(journal, &work.target, owner)
        || serde_json::to_value(&session.provider_graph).ok()
            != serde_json::to_value(Some(graph)).ok()
        || work.providers != graph.order.iter().rev().cloned().collect::<Vec<_>>()
        || work.providers.get(work.index).map(String::as_str) != Some(provider)
        || work.cleanup_blocked(provider)
    {
        return Err("session_lineage_not_retired".into());
    }
    if session.phase != SessionPhase::Ending {
        return Err("session_not_ending".into());
    }
    let mut roots = journal.composition.records().filter(|record| {
        record.output.origin == work.target
            && record.output.producer == *owner
            && record.state == composition::OutputState::Retained
    });
    let root = roots.next().ok_or("session_root_missing_or_ambiguous")?;
    if roots.next().is_some() || !valid_root_provenance(journal, root) {
        return Err("session_root_missing_or_ambiguous".into());
    }
    resource_generation::current_root_generation(journal, root)?;
    if graph.order.len() == 2 {
        if !session_prepare_evidence::all_dependent_session_prepares_noop(
            journal,
            &work.target,
            graph,
            owner,
        ) {
            return Err("dependent_session_prepare_unproven".into());
        }
        if provider != owner
            && work
                .states
                .get(provider)
                .and_then(|state| state.plan.as_ref())
                .is_some_and(|plan| {
                    !plan.resources.is_empty()
                        || !plan.mutation_domains.is_empty()
                        || !plan.obligations.is_empty()
                        || !plan.control_proposals.is_empty()
                        || plan.candidate_constraint.is_some()
                        || !plan.affected_consumers.is_empty()
                        || plan.consumer_change != amaterasu_extension_api::ConsumerChange::Live
                })
        {
            return Err("dependent_session_release_not_noop".into());
        }
        if provider == owner
            && !journal.session_releases.noop_proofs.iter().any(|proof| {
                proof.target == work.target
                    && proof.provider == graph.order[1]
                    && proof.transition_id == work.transition_id
                    && valid_noop_proof(journal, proof)
            })
        {
            return Err("dependent_session_release_unproven".into());
        }
    }
    Ok(())
}

pub(super) fn record_noop_session_release(
    journal: &mut Journal,
    work: &Work,
    provider: &str,
    state: &PrepareState,
) -> ModuleResult<()> {
    validate_session_release_preflight(journal, work, provider)?;
    let graph = work
        .provider_graph
        .as_ref()
        .ok_or("provider_graph_missing")?;
    if graph.order.len() != 2
        || graph.order[1] != provider
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
        return Err("session_noop_release_context_mismatch".into());
    }
    let proof = NoopReleaseProof {
        target: work.target.clone(),
        transition_id: work.transition_id.clone(),
        provider: provider.into(),
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
    if journal.session_releases.noop_proofs.len() >= MAX_RELEASES
        || journal
            .session_releases
            .noop_proofs
            .iter()
            .any(|old| old.target == proof.target && old.provider == proof.provider)
        || !valid_noop_proof(journal, &proof)
    {
        return Err("invalid_session_noop_release_proof".into());
    }
    journal.session_releases.noop_proofs.push(proof);
    Ok(())
}

pub(super) fn record_session_release_proof(
    journal: &mut Journal,
    work: &Work,
    provider: &str,
    state: &PrepareState,
) -> ModuleResult<()> {
    validate_session_release_preflight(journal, work, provider)?;
    if state.phase != PreparePhase::CleanupSettled
        || state.provider != provider
        || state.request.target != work.target
        || state.request.transition_id != work.transition_id
        || !journal.receipts.values().any(|receipt| {
            receipt.work.as_ref().is_some_and(|saved| {
                serde_json::to_value(saved).ok() == serde_json::to_value(work).ok()
            })
        })
    {
        return Err("session_release_context_mismatch".into());
    }
    let root = journal
        .composition
        .records()
        .find(|record| {
            record.output.origin == work.target
                && record.output.producer == provider
                && record.state == composition::OutputState::Retained
        })
        .ok_or("session_root_missing")?
        .clone();
    let proof = SessionReleaseProof {
        target: work.target.clone(),
        transition_id: work.transition_id.clone(),
        provider: provider.into(),
        root_admission_id: root.admission_id.clone(),
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
    if journal.session_releases.proofs.len() >= MAX_RELEASES
        || journal
            .session_releases
            .proofs
            .iter()
            .any(|old| old.target == proof.target)
    {
        return Err("session_release_capacity_or_conflict".into());
    }
    let mut next = journal.composition.clone();
    next.set_state(
        provider,
        &root.output.output_id,
        &root.output.origin,
        composition::OutputState::Released,
    )
    .map_err(str::to_owned)?;
    let previous = std::mem::replace(&mut journal.composition, next);
    journal.session_releases.proofs.push(proof);
    if !journal
        .session_releases
        .proofs
        .last()
        .is_some_and(|candidate| validate_proof(journal, candidate))
    {
        journal.session_releases.proofs.pop();
        journal.composition = previous;
        return Err("invalid_session_release_proof".into());
    }
    Ok(())
}

#[cfg(test)]
pub(in crate::coordinator) mod tests {
    use super::*;
    use serde_json::json;

    fn fixture() -> (Journal, Work) {
        let graph = json!({"order":["owner"],"dependencies":{"owner":[]}});
        let session = json!({"provider_graph":graph,"host_epoch":"epoch","id":"session","generation":1,"phase":"ending","owner":{"caller":"transport","initiator":"client"},"delegates":[],"application_ref":null,"settings_revision":"revision","application_retired":true,"streams":{}});
        let journal = serde_json::from_value(
            json!({"version":3,"host_epoch":"epoch","receipts":{},"sessions":{"session":session}}),
        )
        .unwrap();
        let work = serde_json::from_value(json!({"phase":"session.release","target":{"kind":"session","host_epoch":"epoch","session_id":"session","generation":1},"intent":null,"reason":null,"video":null,"transition_id":"release","providers":["owner"],"provider_graph":graph,"index":0,"states":{},"candidate":null,"waiting_retirement":false})).unwrap();
        (journal, work)
    }

    pub(in crate::coordinator) fn complete_fixture() -> (Journal, Work, PrepareState) {
        use crate::prepare_authority::{Admission, MethodApproval, ProviderSelection};
        use crate::provider_dispatch::{DispatchRecord, DispatchState, ProviderCall};
        let (mut journal, work) = fixture();
        let method = json!({"service":"owner/2","method":"callback","interface_major":2,"interface_minor":0,"schema_revision":1});
        let participant: crate::adapter::ParticipantSettings = serde_json::from_value(json!({"provider":"owner","contract_version":2,"plan_method":method,"callback_method":method,"status_method":method,"capabilities":[],"cleanup_methods":[],"settings_revision":"revision","settings":{}})).unwrap();
        let broker = json!({"provider":"owner","method":{"service":"owner/2","method":"callback","interface_major":2,"interface_minor":0}});
        let selection = ProviderSelection {
            participant: participant.clone(),
            approvals: vec![MethodApproval {
                method: participant.callback_method.clone(),
                broker_approval: broker.clone(),
            }],
            capabilities: BTreeSet::new(),
            cleanup_methods: vec![],
            authorized_borrowers: BTreeSet::new(),
            approved_control_bindings: Vec::new(),
        };
        let obligation = json!({"obligation_id":"cleanup","provider":"owner","resource_id":"display","mutation_domain":null,"cleanup_method":{"service":"owner/2","method":"cleanup","interface_major":2,"interface_minor":0,"schema_revision":1},"payload":{},"depends_on":[]});
        let plan: amaterasu_extension_api::ResourcePlan = serde_json::from_value(json!({"plan_id":"plan","revision":1,"resources":[{"provider":"owner","resource_id":"display","incarnation":null,"expected_revision":2,"execution_domain":"seat","mutation_domains":[],"access":"exclusive"}],"mutation_domains":[],"required_capabilities":[],"constraints":{},"obligations":[obligation],"affected_consumers":[],"consumer_change":"live","candidate_constraint":null,"allowed_degradation":[]})).unwrap();
        let output: amaterasu_extension_api::ResourceOutput = serde_json::from_value(json!({"output_id":"output","capability":"screen","schema":{"id":"screen/2","major":1,"minor":0,"digest":"a".repeat(64)},"producer":"owner","producer_service":"owner/2","origin":work.target,"resource":{"provider":"owner","resource_id":"display","incarnation":"native","revision":2,"execution_domain":"seat","mutation_domain":null,"mutation_domains":[]},"data":{}})).unwrap();
        let request = |phase,
                       admission,
                       transition,
                       operation|
         -> amaterasu_extension_api::ProviderRequest {
            serde_json::from_value(json!({"provider_sequence":1,"operation":"lifecycle_callback","version":2,"transition_id":transition,"operation_id":operation,"target":work.target,"phase":phase,"settings_revision":"revision","settings":{},"intent":null,"reason":null,"requested_video":null,"consumer_requirements":{"execution_domain":"seat","capture_providers":["capture"],"selector_schemas":["selector/1"]},"remaining_ms":1000,"plan_revision":1,"admission_id":admission,"approved_plan":plan})).unwrap()
        };
        let terminal = |request: &amaterasu_extension_api::ProviderRequest,
                        response: serde_json::Value| DispatchRecord {
            call: ProviderCall {
                service: "owner/2".into(),
                method: "callback".into(),
                interface_major: 2,
                interface_minor: 0,
                approval: broker.clone(),
                arguments: serde_json::to_value(request).unwrap(),
                timeout_ms: 1000,
                operation_id: request.operation_id.clone(),
            },
            state: DispatchState::Completed { payload: response },
        };
        let mut root_request = request("session.prepare", "root", "prepare", "root-call");
        root_request.resource_generation = Some(1);
        journal
            .sessions
            .get_mut("session")
            .unwrap()
            .resource_generations
            .insert("owner".into(), 1);
        let root_terminal = terminal(
            &root_request,
            json!({"status":"prepared","candidate":null,"obligations":plan.obligations,"outputs":[output]}),
        );
        let root = composition::StoredOutput {
            output: output.clone(),
            admission_id: "root".into(),
            transition_id: "prepare".into(),
            plan: plan.clone(),
            state: composition::OutputState::Retained,
            callback_proof: Some(composition::ProducerCallbackProof {
                participant: participant.clone(),
                request: root_request,
                terminal: root_terminal,
                pending_receipt: None,
                pending_dispatch: None,
            }),
        };
        let output_selection = composition::OutputSelection {
            producer: output.producer.clone(),
            service: output.producer_service.clone(),
            capability: output.capability.clone(),
            schema: output.schema.clone(),
        };
        journal
            .composition
            .record(root, &output_selection, &work.target, &|_| true)
            .unwrap();
        for (id, phase, transition) in [
            ("root", LifecyclePhase::SessionPrepare, "prepare"),
            (
                "release-admission",
                LifecyclePhase::SessionRelease,
                "release",
            ),
        ] {
            journal.authority.settled.insert(
                id.into(),
                Admission {
                    provider: "owner".into(),
                    selection: selection.clone(),
                    plan: plan.clone(),
                    rights: vec![],
                },
            );
            let session = journal.sessions.get_mut("session").unwrap();
            session.admission_ids.insert(id.into());
            session
                .admission_targets
                .insert(id.into(), work.target.clone());
            session.admission_phases.insert(id.into(), phase);
            journal
                .admission_transitions
                .insert(id.into(), transition.into());
        }
        let mut release_request = request(
            "session.release",
            "release-admission",
            "release",
            "release-call",
        );
        release_request.dependency_inputs = vec![output];
        release_request.resource_generation = Some(1);
        let response = json!({"status":"cleanup","outcomes":[{"obligation_id":"cleanup","state":"released","reason":null}],"outputs":[]});
        let release_terminal = terminal(&release_request, response.clone());
        let state = PrepareState {
            provider: "owner".into(),
            participant,
            request: release_request,
            phase: PreparePhase::CleanupSettled,
            plan: Some(plan),
            admission_id: Some("release-admission".into()),
            pending_receipt: None,
            pending_dispatch: None,
            status_polls: 0,
            last_sequence: 1,
            dispatch: None,
            terminal_dispatch: Some(release_terminal),
            response: Some(serde_json::from_value(response).unwrap()),
        };
        journal.receipts.insert(
            "receipt".into(),
            Receipt {
                caller: "transport".into(),
                initiator: "client".into(),
                request: Value::Null,
                response: Value::Null,
                work: Some(work.clone()),
                wait_target: None,
            },
        );
        (journal, work, state)
    }

    #[test]
    fn scheduler_dispatches_owner_cleanup_and_persists_release() {
        let (mut journal, mut work, mut state) = complete_fixture();
        let directory = std::env::temp_dir().join(format!(
            "amaterasu-session-release-{}",
            amaterasu_storage::random_identifier().unwrap()
        ));
        std::fs::create_dir(&directory).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let mut coordinator = Coordinator::open(&directory.join("journal")).unwrap();
        coordinator.configured = true;
        coordinator
            .settings
            .participants
            .push(state.participant.clone());
        coordinator.settings.consumer_requirements = state.request.consumer_requirements.clone();
        coordinator
            .settings
            .finalize_policies
            .push(super::super::finalize_selection::tests::fixture().policy);
        let mut admission = journal
            .authority
            .settled
            .remove("release-admission")
            .unwrap();
        admission.plan.resources[0].incarnation = Some("native".into());
        let cleanup = admission.plan.obligations[0].cleanup_method.clone();
        admission.selection.cleanup_methods.push(cleanup);
        state.request.approved_plan = Some(admission.plan.clone());
        state.request.dependency_inputs =
            vec![journal.composition.records().next().unwrap().output.clone()];
        state.request.resource_generation = Some(1);
        state.plan = Some(admission.plan.clone());
        state.phase = PreparePhase::Callback;
        state.response = None;
        state.terminal_dispatch = None;
        work.states.insert("owner".into(), state.clone());
        journal
            .authority
            .snapshot
            .resources
            .entry("owner".into())
            .or_default()
            .insert(
                "display".into(),
                crate::planning::ResourceRef {
                    provider: "owner".into(),
                    resource_id: "display".into(),
                    incarnation: "native".into(),
                    revision: 2,
                    execution_domain: "seat".into(),
                    mutation_domains: Default::default(),
                },
            );
        coordinator
            .selections
            .insert("owner".into(), admission.selection.clone());
        journal
            .authority
            .admissions
            .insert("release-admission".into(), admission);
        journal
            .sessions
            .get_mut("session")
            .unwrap()
            .resource_generations
            .insert("owner".into(), 1);
        journal
            .sessions
            .get_mut("session")
            .unwrap()
            .provider_settings
            .insert(
                "owner".into(),
                settings_snapshot::FrozenSettings {
                    settings: json!({}),
                    revision: "revision".into(),
                },
            );
        let key = receipt_key("transport", "client", "end");
        journal.receipts.clear();
        journal.receipts.insert(
            key.clone(),
            Receipt {
                caller: "transport".into(),
                initiator: "client".into(),
                request: json!({"operation_id":"end"}),
                response: json!({"status":"pending","operation_id":"end"}),
                work: Some(work),
                wait_target: None,
            },
        );
        journal.authority.sequences.insert("owner".into(), 1);
        coordinator.shared.borrow_mut().value = journal;
        {
            use crate::prepare_driver::PrepareAuthority;
            let work = coordinator.shared.borrow().value.receipts[&key]
                .work
                .clone()
                .unwrap();
            let mut authority = coordinator.authority_for_work(&work).unwrap();
            authority
                .validate_rights(
                    &state.participant,
                    state.plan.as_ref().unwrap(),
                    "release-admission",
                )
                .unwrap();
        }
        let mut submitted = 0;
        let mut send = |command| -> ModuleResult<Response> {
            match command {
                Command::QueryApiApproval { .. } => Ok(Response::Result {
                    payload: json!({"provider":"owner","method":{"service":"owner/2","method":"callback","interface_major":2,"interface_minor":0}}),
                }),
                Command::CallApi { payload, .. } => {
                    assert_eq!(payload["phase"], "session.release");
                    assert_eq!(payload["resource_generation"], 1);
                    assert_eq!(payload["dependency_inputs"][0]["output_id"], "output");
                    submitted += 1;
                    Ok(Response::Accepted { id: 1 })
                }
                Command::TakeResult { .. } => Ok(Response::Result {
                    payload: json!({"status":"cleanup","outcomes":[{"obligation_id":"cleanup","state":"released","reason":null}],"outputs":[]}),
                }),
                Command::ReleaseResult { .. } => Ok(Response::Ok),
                other => panic!("unexpected broker command: {other:?}"),
            }
        };
        for _ in 0..16 {
            coordinator.advance_work(&mut send).unwrap();
        }
        let shared = coordinator.shared.borrow();
        assert_eq!(
            submitted, 1,
            "receipt: {}",
            shared.value.receipts[&key].response
        );
        assert_eq!(shared.value.receipts[&key].response["status"], "succeeded");
        assert!(shared.value.session_releases.validate(&shared.value));
        assert_eq!(
            shared.value.composition.records().next().unwrap().state,
            composition::OutputState::Released
        );
        drop(shared);
        drop(coordinator);
        let restored: Journal =
            serde_json::from_slice(&std::fs::read(directory.join("journal")).unwrap()).unwrap();
        assert!(restored.session_releases.validate(&restored));
        assert_eq!(
            restored.composition.records().next().unwrap().state,
            composition::OutputState::Released
        );
        validate_journal(&restored).unwrap();
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn scheduler_plans_refreshes_and_releases_owner_resource() {
        let (mut journal, mut work, mut state) = complete_fixture();
        let directory = std::env::temp_dir().join(format!(
            "amaterasu-session-release-{}",
            amaterasu_storage::random_identifier().unwrap()
        ));
        std::fs::create_dir(&directory).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let mut coordinator = Coordinator::open(&directory.join("journal")).unwrap();
        coordinator.configured = true;
        coordinator
            .settings
            .participants
            .push(state.participant.clone());
        coordinator.settings.consumer_requirements = state.request.consumer_requirements.clone();
        coordinator
            .settings
            .finalize_policies
            .push(super::super::finalize_selection::tests::fixture().policy);
        // Reconstruct an authenticated root with an observed shared domain.
        let mut root = journal.composition.records().next().unwrap().clone();
        root.output.resource.mutation_domains = vec!["topology".into()];
        root.output.domain_observations = vec![
            serde_json::from_value(
                json!({"domain":"topology","revision":1,"borrowers":{},"complete":true}),
            )
            .unwrap(),
        ];
        root.plan.resources[0].mutation_domains = vec!["topology".into()];
        root.plan.mutation_domains = vec![serde_json::from_value(json!({"domain":"topology","expected_revision":1,"borrowers":{},"access":"exclusive"})).unwrap()];
        let proof = root.callback_proof.as_mut().unwrap();
        proof.request.approved_plan = Some(root.plan.clone());
        proof.terminal.call.arguments = serde_json::to_value(&proof.request).unwrap();
        proof.terminal.state = crate::provider_dispatch::DispatchState::Completed {
            payload: json!({"status":"prepared","candidate":null,"obligations":root.plan.obligations,"outputs":[root.output]}),
        };
        journal.authority.settled.get_mut("root").unwrap().plan = root.plan.clone();
        journal.composition = composition::CompositionBook::default();
        let selected = composition::OutputSelection {
            producer: root.output.producer.clone(),
            service: root.output.producer_service.clone(),
            capability: root.output.capability.clone(),
            schema: root.output.schema.clone(),
        };
        journal
            .composition
            .record(root.clone(), &selected, &work.target, &|_| true)
            .unwrap();
        let mut admission = journal
            .authority
            .settled
            .remove("release-admission")
            .unwrap();
        admission.plan = root.plan.clone();
        admission.plan.resources[0].incarnation = Some("native".into());
        let cleanup = admission.plan.obligations[0].cleanup_method.clone();
        admission.selection.cleanup_methods.push(cleanup);
        state.request.approved_plan = Some(admission.plan.clone());
        state.request.dependency_inputs =
            vec![journal.composition.records().next().unwrap().output.clone()];
        state.request.resource_generation = Some(1);
        state.plan = Some(admission.plan.clone());
        state.phase = PreparePhase::Callback;
        state.response = None;
        state.terminal_dispatch = None;
        // Begin before plan allocation and admission.
        work.states.clear();
        journal
            .authority
            .snapshot
            .resources
            .entry("owner".into())
            .or_default()
            .insert(
                "display".into(),
                crate::planning::ResourceRef {
                    provider: "owner".into(),
                    resource_id: "display".into(),
                    incarnation: "native".into(),
                    revision: 2,
                    execution_domain: "seat".into(),
                    mutation_domains: ["topology".into()].into_iter().collect(),
                },
            );
        coordinator
            .selections
            .insert("owner".into(), admission.selection.clone());
        let release_plan = admission.plan.clone();
        journal.authority.snapshot.domains.insert(
            "topology".into(),
            crate::planning::DomainState {
                revision: 1,
                borrowers: Default::default(),
            },
        );
        journal
            .authority
            .domain_owners
            .insert("topology".into(), "owner".into());
        journal.authority.refresh_required.insert("topology".into());
        journal.admission_transitions.remove("release-admission");
        let session = journal.sessions.get_mut("session").unwrap();
        session.admission_ids.remove("release-admission");
        session.admission_targets.remove("release-admission");
        session.admission_phases.remove("release-admission");
        journal
            .sessions
            .get_mut("session")
            .unwrap()
            .resource_generations
            .insert("owner".into(), 1);
        journal
            .sessions
            .get_mut("session")
            .unwrap()
            .provider_settings
            .insert(
                "owner".into(),
                settings_snapshot::FrozenSettings {
                    settings: json!({}),
                    revision: "revision".into(),
                },
            );
        let key = receipt_key("transport", "client", "end");
        journal.receipts.clear();
        journal.receipts.insert(
            key.clone(),
            Receipt {
                caller: "transport".into(),
                initiator: "client".into(),
                request: json!({"operation_id":"end"}),
                response: json!({"status":"pending","operation_id":"end"}),
                work: Some(work),
                wait_target: None,
            },
        );
        journal.authority.sequences.insert("owner".into(), 1);
        coordinator.shared.borrow_mut().value = journal;
        let mut submitted = 0;
        let mut current_response = Value::Null;
        let mut refreshed = false;
        for _ in 0..40 {
            let mut owner_plan = None;
            {
                let mut send = |command| -> ModuleResult<Response> {
                    match command {
                        Command::QueryApiApproval { .. } => Ok(Response::Result {
                            payload: json!({"provider":"owner","method":{"service":"owner/2","method":"callback","interface_major":2,"interface_minor":0}}),
                        }),
                        Command::CallApi { payload, .. } => {
                            assert_eq!(payload["phase"], "session.release");
                            assert_eq!(payload["resource_generation"], 1);
                            assert_eq!(payload["dependency_inputs"][0]["output_id"], "output");
                            submitted += 1;
                            if payload["operation"] == "resource_plan" {
                                owner_plan = Some(payload);
                                current_response = json!({"status":"planned","plan":release_plan});
                            } else {
                                current_response = json!({"status":"cleanup","outcomes":[{"obligation_id":"cleanup","state":"released","reason":null}],"outputs":[]});
                            }
                            Ok(Response::Accepted { id: 1 })
                        }
                        Command::TakeResult { .. } => Ok(Response::Result {
                            payload: current_response.clone(),
                        }),
                        Command::ReleaseResult { .. } => Ok(Response::Ok),
                        other => panic!("unexpected broker command: {other:?}"),
                    }
                };
                coordinator.advance_work(&mut send).unwrap();
            }
            if let Some(request) = owner_plan {
                let refresh = json!({"operation_id":"owner-refresh","target":request["target"],"provider_request":request,"domains":[{"domain":"topology","revision":1,"borrowers":{}}],"resources":[]});
                let response = coordinator
                    .refresh_provider_observations(
                        "owner",
                        "owner",
                        refresh,
                        receipt_key("owner", "owner", "owner-refresh"),
                    )
                    .unwrap();
                assert_eq!(response["status"], "succeeded");
                refreshed = true;
            }
        }
        assert!(refreshed);
        let shared = coordinator.shared.borrow();
        assert_eq!(
            submitted, 2,
            "receipt: {}",
            shared.value.receipts[&key].response
        );
        assert_eq!(shared.value.receipts[&key].response["status"], "succeeded");
        assert!(shared.value.session_releases.validate(&shared.value));
        assert_eq!(
            shared.value.composition.records().next().unwrap().state,
            composition::OutputState::Released
        );
        drop(shared);
        drop(coordinator);
        let restored: Journal =
            serde_json::from_slice(&std::fs::read(directory.join("journal")).unwrap()).unwrap();
        assert!(restored.session_releases.validate(&restored));
        assert_eq!(
            restored.composition.records().next().unwrap().state,
            composition::OutputState::Released
        );
        validate_journal(&restored).unwrap();
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn exact_completed_cleanup_atomically_releases_root_and_retains_proof() {
        let (mut journal, work, state) = complete_fixture();
        assert!(validate_session_release_preflight(&journal, &work, "owner").is_ok());
        record_session_release_proof(&mut journal, &work, "owner", &state).unwrap();
        assert_eq!(
            journal.composition.records().next().unwrap().state,
            composition::OutputState::Released
        );
        assert!(journal.session_releases.validate(&journal));
        let restored: Journal =
            serde_json::from_value(serde_json::to_value(journal).unwrap()).unwrap();
        assert!(restored.session_releases.validate(&restored));
    }

    #[test]
    fn failed_cleanup_evidence_never_changes_journal() {
        let (journal, work, state) = complete_fixture();
        for outcome in ["pending", "deferred_restoration", "effect_unknown"] {
            let mut next = journal.clone();
            let mut bad = state.clone();
            if let Some(crate::provider_dispatch::DispatchRecord {
                state: crate::provider_dispatch::DispatchState::Completed { payload },
                ..
            }) = bad.terminal_dispatch.as_mut()
            {
                payload["outcomes"][0]["state"] = json!(outcome);
            }
            let before = serde_json::to_value(&next).unwrap();
            assert!(record_session_release_proof(&mut next, &work, "owner", &bad).is_err());
            assert_eq!(serde_json::to_value(next).unwrap(), before);
        }
    }

    fn dependent_fixture() -> (Journal, Work, PrepareState, PrepareState) {
        let (mut journal, mut work, root_state) = complete_fixture();
        let graph = ProviderGraph {
            order: vec!["owner".into(), "settings".into()],
            dependencies: BTreeMap::from([
                ("owner".into(), vec![]),
                ("settings".into(), vec!["owner".into()]),
            ]),
        };
        journal.sessions.get_mut("session").unwrap().provider_graph = Some(graph.clone());
        work.provider_graph = Some(graph);
        work.providers = vec!["settings".into(), "owner".into()];
        work.index = 0;
        let mut participant = root_state.participant.clone();
        participant.provider = "settings".into();
        for method in [
            &mut participant.plan_method,
            &mut participant.callback_method,
            &mut participant.status_method,
        ] {
            method.service = "settings/2".into();
        }
        let broker = json!({"provider":"settings","method":{"service":"settings/2","method":"callback","interface_major":2,"interface_minor":0}});
        let mut selection = journal.authority.settled["root"].selection.clone();
        selection.participant = participant.clone();
        selection.approvals[0].method = participant.callback_method.clone();
        selection.approvals[0].broker_approval = broker.clone();
        let plan: amaterasu_extension_api::ResourcePlan = serde_json::from_value(json!({"plan_id":"noop","revision":1,"resources":[],"mutation_domains":[],"required_capabilities":[],"constraints":{},"obligations":[],"affected_consumers":[],"consumer_change":"live","candidate_constraint":null,"allowed_degradation":[]})).unwrap();
        let mut state = root_state.clone();
        state.provider = "settings".into();
        state.request.dependency_inputs.clear();
        state.request.resource_generation = None;
        state.participant = participant;
        state.plan = Some(plan.clone());
        state.request.approved_plan = Some(plan.clone());
        state.request.admission_id = Some("settings-prepare".into());
        state.admission_id = Some("settings-prepare".into());
        state.request.transition_id = "settings-prepare-transition".into();
        state.request.operation_id = "settings-prepare-call".into();
        state.request.phase = LifecyclePhase::SessionPrepare;
        state.phase = PreparePhase::Prepared;
        state.response = Some(
            serde_json::from_value(
                json!({"status":"prepared","candidate":null,"obligations":[],"outputs":[]}),
            )
            .unwrap(),
        );
        let terminal = state.terminal_dispatch.as_mut().unwrap();
        terminal.call.service = "settings/2".into();
        terminal.call.approval = broker.clone();
        terminal.call.operation_id = state.request.operation_id.clone();
        terminal.call.arguments = serde_json::to_value(&state.request).unwrap();
        terminal.state = crate::provider_dispatch::DispatchState::Completed {
            payload: serde_json::to_value(state.response.as_ref().unwrap()).unwrap(),
        };
        for (id, phase, transition) in [
            (
                "settings-prepare",
                LifecyclePhase::SessionPrepare,
                "settings-prepare-transition",
            ),
            (
                "settings-release",
                LifecyclePhase::SessionRelease,
                "release",
            ),
        ] {
            journal.authority.settled.insert(
                id.into(),
                crate::prepare_authority::Admission {
                    provider: "settings".into(),
                    selection: selection.clone(),
                    plan: plan.clone(),
                    rights: vec![],
                },
            );
            let session = journal.sessions.get_mut("session").unwrap();
            session.admission_ids.insert(id.into());
            session
                .admission_targets
                .insert(id.into(), work.target.clone());
            session.admission_phases.insert(id.into(), phase);
            journal
                .admission_transitions
                .insert(id.into(), transition.into());
        }
        let mut prepare_work = work.clone();
        prepare_work.phase = LifecyclePhase::SessionPrepare;
        prepare_work.transition_id = "settings-prepare-transition".into();
        prepare_work.states.insert("settings".into(), state.clone());
        journal.receipts.get_mut("receipt").unwrap().work = Some(prepare_work.clone());
        session_prepare_evidence::record(&mut journal, &prepare_work, "settings", &state).unwrap();
        state.request.phase = LifecyclePhase::SessionRelease;
        state.request.transition_id = work.transition_id.clone();
        state.request.operation_id = "settings-release-call".into();
        state.request.admission_id = Some("settings-release".into());
        state.admission_id = Some("settings-release".into());
        state.phase = PreparePhase::CleanupSettled;
        state.response = Some(
            serde_json::from_value(json!({"status":"cleanup","outcomes":[],"outputs":[]})).unwrap(),
        );
        let terminal = state.terminal_dispatch.as_mut().unwrap();
        terminal.call.operation_id = state.request.operation_id.clone();
        terminal.call.arguments = serde_json::to_value(&state.request).unwrap();
        terminal.state = crate::provider_dispatch::DispatchState::Completed {
            payload: serde_json::to_value(state.response.as_ref().unwrap()).unwrap(),
        };
        journal.receipts.get_mut("receipt").unwrap().work = Some(work.clone());
        (journal, work, root_state, state)
    }

    #[test]
    fn scheduler_releases_noop_dependent_before_session_owner() {
        let (mut journal, mut work, mut owner, mut dependent) = dependent_fixture();
        let directory = std::env::temp_dir().join(format!(
            "amaterasu-session-reverse-{}",
            amaterasu_storage::random_identifier().unwrap()
        ));
        std::fs::create_dir(&directory).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let mut coordinator = Coordinator::open(&directory.join("journal")).unwrap();
        coordinator.configured = true;
        coordinator.settings.participants =
            vec![owner.participant.clone(), dependent.participant.clone()];
        coordinator.settings.consumer_requirements = owner.request.consumer_requirements.clone();
        coordinator
            .settings
            .finalize_policies
            .push(super::super::finalize_selection::tests::fixture().policy);
        let mut owner_admission = journal
            .authority
            .settled
            .remove("release-admission")
            .unwrap();
        owner_admission.plan.resources[0].incarnation = Some("native".into());
        owner_admission
            .selection
            .cleanup_methods
            .push(owner_admission.plan.obligations[0].cleanup_method.clone());
        owner.request.approved_plan = Some(owner_admission.plan.clone());
        owner.request.dependency_inputs =
            vec![journal.composition.records().next().unwrap().output.clone()];
        owner.request.resource_generation = Some(1);
        owner.plan = Some(owner_admission.plan.clone());
        owner.phase = PreparePhase::Callback;
        owner.response = None;
        owner.terminal_dispatch = None;
        let dependent_admission = journal
            .authority
            .settled
            .remove("settings-release")
            .unwrap();
        dependent.phase = PreparePhase::Callback;
        dependent.response = None;
        dependent.terminal_dispatch = None;
        work.states = BTreeMap::from([
            ("settings".into(), dependent.clone()),
            ("owner".into(), owner.clone()),
        ]);
        journal
            .authority
            .snapshot
            .resources
            .entry("owner".into())
            .or_default()
            .insert(
                "display".into(),
                crate::planning::ResourceRef {
                    provider: "owner".into(),
                    resource_id: "display".into(),
                    incarnation: "native".into(),
                    revision: 2,
                    execution_domain: "seat".into(),
                    mutation_domains: Default::default(),
                },
            );
        coordinator
            .selections
            .insert("owner".into(), owner_admission.selection.clone());
        coordinator
            .selections
            .insert("settings".into(), dependent_admission.selection.clone());
        journal
            .authority
            .admissions
            .insert("release-admission".into(), owner_admission);
        journal
            .authority
            .admissions
            .insert("settings-release".into(), dependent_admission);
        journal.authority.sequences.insert("owner".into(), 1);
        journal.authority.sequences.insert("settings".into(), 1);
        let session = journal.sessions.get_mut("session").unwrap();
        session.resource_generations.insert("owner".into(), 1);
        for provider in ["owner", "settings"] {
            session.provider_settings.insert(
                provider.into(),
                settings_snapshot::FrozenSettings {
                    settings: json!({}),
                    revision: "revision".into(),
                },
            );
        }
        let key = receipt_key("transport", "client", "end");
        journal.receipts.clear();
        journal.receipts.insert(
            key.clone(),
            Receipt {
                caller: "transport".into(),
                initiator: "client".into(),
                request: json!({"operation_id":"end"}),
                response: json!({"status":"pending","operation_id":"end"}),
                work: Some(work),
                wait_target: None,
            },
        );
        coordinator.shared.borrow_mut().value = journal;
        let mut calls = Vec::new();
        let mut provider = String::new();
        let mut send = |command| -> ModuleResult<Response> {
            match command {
                Command::QueryApiApproval {
                    service,
                    method,
                    interface_major,
                    min_minor,
                } => Ok(Response::Result {
                    payload: json!({"provider":if service == "owner/2" {"owner"} else {"settings"},"method":{"service":service,"method":method,"interface_major":interface_major,"interface_minor":min_minor}}),
                }),
                Command::CallApi {
                    service, payload, ..
                } => {
                    assert_eq!(payload["phase"], "session.release");
                    provider = if service == "owner/2" {
                        "owner"
                    } else {
                        "settings"
                    }
                    .into();
                    calls.push(provider.clone());
                    Ok(Response::Accepted { id: 1 })
                }
                Command::TakeResult { .. } => Ok(Response::Result {
                    payload: if provider == "owner" {
                        json!({"status":"cleanup","outcomes":[{"obligation_id":"cleanup","state":"released","reason":null}],"outputs":[]})
                    } else {
                        json!({"status":"cleanup","outcomes":[],"outputs":[]})
                    },
                }),
                Command::ReleaseResult { .. } => Ok(Response::Ok),
                other => panic!("unexpected broker command: {other:?}"),
            }
        };
        for _ in 0..24 {
            coordinator.advance_work(&mut send).unwrap();
        }
        let shared = coordinator.shared.borrow();
        assert_eq!(
            calls,
            ["settings", "owner"],
            "receipt: {}",
            shared.value.receipts[&key].response
        );
        assert_eq!(shared.value.receipts[&key].response["status"], "succeeded");
        assert!(shared.value.session_releases.validate(&shared.value));
        drop(shared);
        drop(coordinator);
        let restored: Journal =
            serde_json::from_slice(&std::fs::read(directory.join("journal")).unwrap()).unwrap();
        assert!(restored.session_releases.validate(&restored));
        validate_journal(&restored).unwrap();
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn dependent_noop_proof_precedes_owner_release() {
        let (mut journal, work, root_state, state) = dependent_fixture();
        assert!(validate_session_release_preflight(&journal, &work, "settings").is_ok());
        let mut owner_work = work.clone();
        owner_work.index = 1;
        assert_eq!(
            validate_session_release_preflight(&journal, &owner_work, "owner").unwrap_err(),
            "dependent_session_release_unproven"
        );
        let before = serde_json::to_value(&journal).unwrap();
        let mut invalid = state.clone();
        invalid.terminal_dispatch.as_mut().unwrap().call.approval["provider"] = json!("foreign");
        assert!(record_noop_session_release(&mut journal, &work, "settings", &invalid).is_err());
        assert_eq!(serde_json::to_value(&journal).unwrap(), before);
        record_noop_session_release(&mut journal, &work, "settings", &state).unwrap();
        assert!(journal.session_releases.validate(&journal));
        assert!(validate_session_release_preflight(&journal, &owner_work, "owner").is_ok());
        let mut stale = journal.clone();
        stale
            .sessions
            .get_mut("session")
            .unwrap()
            .resource_generations
            .insert("owner".into(), 2);
        assert_eq!(
            validate_session_release_preflight(&stale, &owner_work, "owner").unwrap_err(),
            "resource_generation_stale"
        );
        journal.receipts.get_mut("receipt").unwrap().work = Some(owner_work.clone());
        record_session_release_proof(&mut journal, &owner_work, "owner", &root_state).unwrap();
        assert!(journal.session_releases.validate(&journal));
        assert!(journal.session_releases.has_valid_owner_completion(
            &journal,
            &owner_work.target,
            "owner"
        ));
        assert!(!journal.session_releases.has_valid_owner_completion(
            &journal,
            &owner_work.target,
            "settings"
        ));
        let mut altered = journal.clone();
        altered.session_releases.proofs[0]
            .callback
            .terminal
            .call
            .approval["provider"] = json!("foreign");
        assert!(!altered.session_releases.has_valid_owner_completion(
            &altered,
            &owner_work.target,
            "owner"
        ));
        assert!(!altered.session_releases.validate(&altered));
        let mut substituted = journal.clone();
        let callback = &mut substituted.session_releases.proofs[0].callback;
        callback.request.dependency_inputs[0].data = json!({"substituted":true});
        callback.terminal.call.arguments = serde_json::to_value(&callback.request).unwrap();
        // Direct completion checks immutable root identity; full proof also
        // resolves the exact latest authenticated readback and rejects this.
        assert!(!substituted.session_releases.validate(&substituted));
        let mut wrong_generation = journal.clone();
        let callback = &mut wrong_generation.session_releases.proofs[0].callback;
        callback.request.resource_generation = Some(2);
        callback.terminal.call.arguments = serde_json::to_value(&callback.request).unwrap();
        assert!(
            !wrong_generation
                .session_releases
                .has_valid_owner_completion(&wrong_generation, &owner_work.target, "owner")
        );
        assert!(
            !wrong_generation
                .session_releases
                .validate(&wrong_generation)
        );
        let mut advanced_cursor = journal.clone();
        advanced_cursor
            .sessions
            .get_mut("session")
            .unwrap()
            .resource_generations
            .insert("owner".into(), 2);
        assert!(advanced_cursor.session_releases.has_valid_owner_completion(
            &advanced_cursor,
            &owner_work.target,
            "owner"
        ));
        assert_eq!(
            validated_release_closure(&journal, &owner_work.target).unwrap_err(),
            "session_not_ended"
        );
        journal.sessions.get_mut("session").unwrap().phase = SessionPhase::Ended;
        let evidence = validated_release_closure(&journal, &owner_work.target).unwrap();
        assert_eq!(evidence.owner, "owner");
        assert_eq!(evidence.root_admission, "root");
        assert_eq!(evidence.owner_release_admission, "release-admission");
        assert_eq!(evidence.resource_generation, 1);
        assert_eq!(evidence.dependents.len(), 1);
        assert_eq!(evidence.dependents[0].provider, "settings");
        assert_eq!(evidence.dependents[0].prepare_admission, "settings-prepare");
        assert_eq!(evidence.dependents[0].release_admission, "settings-release");
        let mut next = journal.clone();
        let before = serde_json::to_value(&next).unwrap();
        let mut wrong = evidence.clone();
        wrong.root_admission = "other".into();
        assert!(retire_closed_release(&mut next, &owner_work.target, &wrong).is_err());
        assert_eq!(serde_json::to_value(&next).unwrap(), before);
        retire_closed_release(&mut next, &owner_work.target, &evidence).unwrap();
        assert!(!next.session_releases.references_admission("root"));
        assert!(
            !next
                .session_releases
                .references_admission("settings-release")
        );
        assert!(journal.session_releases.references_admission("root"));
        let restored: Journal =
            serde_json::from_value(serde_json::to_value(journal).unwrap()).unwrap();
        assert!(restored.session_releases.validate(&restored));
    }

    #[test]
    fn prior_stream_obligation_requires_exact_target_admissions_and_verified_release() {
        let (mut journal, work, _) = complete_fixture();
        let target: LifecycleTarget = serde_json::from_value(json!({"kind":"stream","host_epoch":"epoch","session_id":"session","stream_id":"stream","generation":3})).unwrap();
        let stream: StreamRecord = serde_json::from_value(json!({"generation":3,"phase":"retired","candidate":null,"required":[],"acknowledgements":[],"retirements":[],"binding_owner":"owner"})).unwrap();
        journal
            .sessions
            .get_mut("session")
            .unwrap()
            .streams
            .insert("stream".into(), stream);
        let admission = journal.authority.settled["root"].clone();
        let obligation = admission.plan.obligations[0].clone();
        for (id, phase) in [
            ("stream-prepare", LifecyclePhase::StreamPrepare),
            ("stream-release", LifecyclePhase::StreamRelease),
        ] {
            journal
                .authority
                .settled
                .insert(id.into(), admission.clone());
            let session = journal.sessions.get_mut("session").unwrap();
            session.admission_ids.insert(id.into());
            session.admission_targets.insert(id.into(), target.clone());
            session.admission_phases.insert(id.into(), phase);
        }
        let verified = |requested: &LifecycleTarget| {
            (requested == &target).then(|| "stream-release".to_string())
        };
        assert!(proven_stream_obligation_with(
            &journal,
            &work.target,
            &obligation,
            verified
        ));
        // The production accessor does not accept an unverified synthetic match.
        assert!(!proven_stream_obligation_released(
            &journal,
            &work.target,
            &obligation
        ));
        let mut substituted = obligation.clone();
        substituted.payload = json!({"different":true});
        assert!(!proven_stream_obligation_with(
            &journal,
            &work.target,
            &substituted,
            verified
        ));
        assert!(!proven_stream_obligation_with(
            &journal,
            &work.target,
            &obligation,
            |_| None
        ));
        journal
            .sessions
            .get_mut("session")
            .unwrap()
            .admission_targets
            .insert("stream-release".into(), work.target.clone());
        assert!(!proven_stream_obligation_with(
            &journal,
            &work.target,
            &obligation,
            verified
        ));
    }

    #[test]
    fn session_release_uses_newest_proven_readback_and_fences_missing() {
        let (journal, _, _) = complete_fixture();
        let root = journal.composition.records().next().unwrap().clone();
        let stage = |id: &str, revision| {
            let mut after = root.output.clone();
            after.resource.revision = revision;
            composition::StagedOutputRefinement {
                target: LifecycleTarget::Stream {
                    host_epoch: "epoch".into(),
                    session_id: "session".into(),
                    stream_id: id.into(),
                    generation: 1,
                },
                parent_generation: 1,
                prior_release_admission: None,
                admission_id: format!("admission-{id}"),
                transition_id: format!("transition-{id}"),
                plan: root.plan.clone(),
                before: root.output.clone(),
                after,
                callback_proof: root.callback_proof.clone().unwrap(),
            }
        };
        let stages = [stage("old", 3), stage("new", 4)];
        let mut released = root.output.clone();
        released.resource.revision = 5;
        let selected = select_release_input(&root, stages.iter().rev(), |target| {
            (target == &stages[1].target).then(|| released.clone())
        })
        .unwrap();
        assert_eq!(selected, released);
        assert_eq!(
            select_release_input(&root, stages.iter().rev(), |target| {
                (target == &stages[0].target).then(|| root.output.clone())
            })
            .unwrap_err(),
            "prior_stream_release_readback_missing"
        );
        let duplicate = [stages[1].clone(), stages[1].clone()];
        assert_eq!(
            select_release_input(&root, duplicate.iter(), |_| Some(released.clone())).unwrap_err(),
            "session_release_stage_ambiguous"
        );
        let mut foreign = released;
        foreign.resource.incarnation = "foreign".into();
        assert_eq!(
            select_release_input(&root, stages.iter().rev(), |_| Some(foreign.clone()))
                .unwrap_err(),
            "session_release_readback_mismatch"
        );
        assert_eq!(session_release_input(&journal, &root).unwrap(), root.output);
    }

    #[test]
    fn checkpoint_anchor_requires_authentication_and_exact_release_identity() {
        let (journal, _, _) = complete_fixture();
        let root = journal.composition.records().next().unwrap().clone();
        let mut released = root.output.clone();
        released.resource.revision += 2;
        let anchor = composition::StageAnchor {
            target: LifecycleTarget::Stream {
                host_epoch: "epoch".into(),
                session_id: "session".into(),
                stream_id: "closed".into(),
                generation: 1,
            },
            release_admission: "cleanup".into(),
            staged: root.output.clone(),
            released: released.clone(),
        };
        assert_eq!(
            validated_anchor_input(&root, &anchor, "cleanup", &released).unwrap(),
            released
        );
        assert_eq!(
            validated_anchor_input(&root, &anchor, "other", &released).unwrap_err(),
            "prior_stream_release_anchor_mismatch"
        );
        let mut mismatched = released.clone();
        mismatched.resource.revision += 1;
        assert_eq!(
            validated_anchor_input(&root, &anchor, "cleanup", &mismatched).unwrap_err(),
            "prior_stream_release_anchor_mismatch"
        );
        let mut foreign = anchor.clone();
        foreign.released.resource.incarnation = "foreign".into();
        assert_eq!(
            validated_anchor_input(&root, &foreign, "cleanup", &foreign.released).unwrap_err(),
            "session_release_readback_mismatch"
        );
        // A forged structurally valid anchor with no authenticated checkpoint is
        // never accepted by the actual journal-backed SessionRelease resolver.
        let mut encoded = serde_json::to_value(&journal).unwrap();
        encoded["composition"]["stage_anchors"] = json!([anchor]);
        let forged: Journal = serde_json::from_value(encoded.clone()).unwrap();
        assert_eq!(
            session_release_input(&forged, &root).unwrap_err(),
            "composition_history_invalid"
        );
        let first = encoded["composition"]["stage_anchors"][0].clone();
        encoded["composition"]["stage_anchors"] = json!([first, first]);
        let ambiguous: Journal = serde_json::from_value(encoded).unwrap();
        assert_eq!(
            session_release_input(&ambiguous, &root).unwrap_err(),
            "composition_history_invalid"
        );
    }

    #[test]
    fn session_prepare_closure_requires_exact_authenticated_set() {
        let (mut journal, work, mut state) = complete_fixture();
        let mut release_state = state.clone();
        release_state.request.provider_sequence = 2;
        release_state.last_sequence = 2;
        release_state
            .terminal_dispatch
            .as_mut()
            .unwrap()
            .call
            .arguments = serde_json::to_value(&release_state.request).unwrap();
        let root = journal.composition.records().next().unwrap().clone();
        let proof = root.callback_proof.unwrap();
        state.request = proof.request.clone();
        state.participant = proof.participant.clone();
        state.phase = PreparePhase::Prepared;
        state.admission_id = Some(root.admission_id.clone());
        state.plan = Some(root.plan.clone());
        state.terminal_dispatch = Some(proof.terminal.clone());
        state.response = match &proof.terminal.state {
            crate::provider_dispatch::DispatchState::Completed { payload } => {
                Some(serde_json::from_value(payload.clone()).unwrap())
            }
            _ => panic!("missing completed prepare"),
        };
        let mut prepare = work.clone();
        prepare.phase = LifecyclePhase::SessionPrepare;
        prepare.transition_id = root.transition_id;
        prepare.states.insert("owner".into(), state.clone());
        journal.receipts.get_mut("receipt").unwrap().work = Some(prepare.clone());
        session_prepare_evidence::record(&mut journal, &prepare, "owner", &state).unwrap();
        assert!(
            session_prepare_evidence::validated_closed_prepares(&journal, &work.target).is_err()
        );
        journal.receipts.get_mut("receipt").unwrap().work = Some(work.clone());
        record_session_release_proof(&mut journal, &work, "owner", &release_state).unwrap();
        journal.sessions.get_mut("session").unwrap().phase = SessionPhase::Ended;
        let session = journal.sessions.get_mut("session").unwrap();
        session.application_ref = json!({"kind":"desktop"});
        session.application_retirement_proof =
            Some(application::ApplicationRetirementProof::Desktop {
                target: work.target.clone(),
            });
        journal.receipts.clear();
        journal.authority.sequences.insert("owner".into(), 2);
        journal.authority.operation_counter = 1;
        journal.authority.issued_operation_ids =
            BTreeSet::from(["root-call".into(), "release-call".into()]);
        assert!(super::session_closure::preflight(&journal, &work.target).is_ok());
        let session = &journal.sessions["session"];
        let root = journal.composition.records().next().unwrap().clone();
        let terminal = journal.session_releases.proofs[0].callback.clone();
        let cert = super::session_closed_history::EndedSessionCertificate {
            version: 1,
            target: work.target.clone(),
            owner: session.owner.clone(),
            end_receipt: None,
            graph: session.provider_graph.clone().unwrap(),
            application_ref: session.application_ref.clone(),
            application: session.application.clone(),
            application_operation_id: session.application_operation_id.clone(),
            application_proof: session.application_retirement_proof.clone().unwrap(),
            producer: "owner".into(),
            resource_generation: 1,
            root,
            root_admission: journal.authority.settled["root"].clone(),
            owner_release: super::session_closed_history::TerminalCallbackEvidence {
                admission_id: "release-admission".into(),
                admission: journal.authority.settled["release-admission"].clone(),
                callback: terminal,
            },
            dependents: vec![],
            closed_children: 0,
            children: None,
            provider_sequences: BTreeMap::from([("owner".into(), 2)]),
            operation_counter: 1,
            legacy_operation_ids: BTreeSet::from(["root-call".into(), "release-call".into()]),
        };
        assert!(cert.validate());
        let constructed = super::session_closed_history::EndedSessionCertificate::from_verified(
            &journal,
            &work.target,
        )
        .unwrap();
        assert!(constructed == cert);
        let end_request =
            json!({"operation":"end_session","operation_id":"end-operation","target":work.target});
        let end_response =
            json!({"status":"succeeded","operation_id":"end-operation","target":work.target});
        let end_key = receipt_key("transport", "client", "end-operation");
        let mut with_end = journal.clone();
        with_end.receipts.insert(
            end_key.clone(),
            Receipt {
                caller: "transport".into(),
                initiator: "client".into(),
                request: end_request.clone(),
                response: end_response.clone(),
                work: None,
                wait_target: None,
            },
        );
        let mut retired = with_end;
        super::session_closure::retire_ended_session(&mut retired, &work.target).unwrap();
        assert_eq!(
            retired
                .ended_session_history
                .end_response(&retired, &end_key, &end_request),
            Some(end_response.clone())
        );
        assert_eq!(
            retired.ended_session_history.end_response(
                &retired,
                &end_key,
                &json!({"operation":"lookup"})
            ),
            Some(end_response.clone())
        );
        assert_eq!(
            retired.ended_session_history.end_response(
                &retired,
                &end_key,
                &json!({"operation":"end_session","operation_id":"end-operation","target":null})
            ),
            Some(rejection("operation_id_conflict"))
        );
        assert!(
            retired
                .ended_session_history
                .end_response(
                    &retired,
                    &receipt_key("foreign", "client", "end-operation"),
                    &end_request
                )
                .is_none()
        );
        let reloaded: Journal =
            serde_json::from_value(serde_json::to_value(&retired).unwrap()).unwrap();
        assert!(validate_journal(&reloaded).is_ok());
        assert_eq!(
            reloaded
                .ended_session_history
                .end_response(&reloaded, &end_key, &end_request),
            Some(end_response)
        );
        assert!(retired.sessions.is_empty());
        assert!(retention::session_expired(&retired, &work.target));
        assert!(retired.composition.records().next().is_none());
        assert!(validate_journal(&retired).is_ok());
        let mut rejected = journal.clone();
        rejected.sessions.get_mut("session").unwrap().phase = SessionPhase::RecoveryRequired;
        let before = serde_json::to_value(&rejected).unwrap();
        assert!(super::session_closure::retire_ended_session(&mut rejected, &work.target).is_err());
        assert_eq!(serde_json::to_value(&rejected).unwrap(), before);
        let mut substituted = journal.clone();
        let mut value = serde_json::to_value(&substituted.composition).unwrap();
        value["outputs"][0]["callback_proof"]["request"]["operation_id"] =
            json!("alternate-root-call");
        value["outputs"][0]["callback_proof"]["terminal"]["call"]["operation_id"] =
            json!("alternate-root-call");
        value["outputs"][0]["callback_proof"]["terminal"]["call"]["arguments"]["operation_id"] =
            json!("alternate-root-call");
        substituted.composition = serde_json::from_value(value).unwrap();
        substituted
            .authority
            .issued_operation_ids
            .insert("alternate-root-call".into());
        assert!(
            super::session_closed_history::EndedSessionCertificate::from_verified(
                &substituted,
                &work.target
            )
            .is_err()
        );
        let restored: super::session_closed_history::EndedSessionCertificate =
            serde_json::from_slice(&serde_json::to_vec(&cert).unwrap()).unwrap();
        assert!(restored.validate());
        let mut forged = restored.clone();
        forged.owner_release.callback.terminal.call.approval["provider"] = json!("foreign");
        assert!(!forged.validate());
        let mut forged = restored.clone();
        forged.resource_generation = 2;
        assert!(!forged.validate());
        let mut forged = restored.clone();
        forged.closed_children = 1;
        assert!(!forged.validate());
        let mut forged = restored.clone();
        forged.owner_release.callback.request.provider_sequence = 1;
        forged.owner_release.callback.terminal.call.arguments =
            serde_json::to_value(&forged.owner_release.callback.request).unwrap();
        assert!(!forged.validate());
        let mut checkpoint: Journal = serde_json::from_value(
            json!({"version":3,"host_epoch":"epoch","receipts":{},"sessions":{}}),
        )
        .unwrap();
        checkpoint.authority.sequences.insert("owner".into(), 2);
        checkpoint.authority.operation_counter = 1;
        checkpoint.authority.issued_operation_ids =
            BTreeSet::from(["root-call".into(), "release-call".into()]);
        retention::fence_closed_session(&mut checkpoint, &work.target).unwrap();
        checkpoint
            .ended_session_history
            .stage_verified(&journal, &work.target)
            .unwrap();
        assert!(validate_journal(&checkpoint).is_ok());
        let stream = LifecycleTarget::Stream {
            host_epoch: "epoch".into(),
            session_id: "session".into(),
            stream_id: "historical-stream".into(),
            generation: 1,
        };
        let root_digest = super::closed_stream::test_root_commitment(&cert.root);
        let (child_book, child_snapshot) = super::closed_stream::test_archive_snapshot(
            &journal,
            &work.target,
            stream.clone(),
            &cert.root,
        )
        .unwrap();
        let mut with_child = checkpoint.clone();
        let mut child_cert = cert.clone();
        child_cert.closed_children = 1;
        child_cert.children = Some(child_snapshot);
        let mut source_with_child = journal.clone();
        source_with_child.closed_streams = child_book.clone();
        let actual_child_cert =
            super::session_closed_history::EndedSessionCertificate::from_verified(
                &source_with_child,
                &work.target,
            )
            .unwrap();
        assert!(actual_child_cert == child_cert);
        let mut transferred = source_with_child.clone();
        super::session_closure::retire_ended_session(&mut transferred, &work.target).unwrap();
        assert!(!transferred.closed_streams.contains_target(&stream));
        let (history, _) = transferred
            .ended_session_history
            .historical_children(&transferred, &stream, "owner")
            .unwrap();
        assert!(history.contains_closure_query(
            "owner",
            &stream,
            1,
            ("display", "incarnation"),
            "control",
            &"b".repeat(64)
        ));
        assert!(!history.contains_closure_query(
            "owner",
            &stream,
            2,
            ("display", "incarnation"),
            "control",
            &"b".repeat(64)
        ));
        assert!(
            transferred
                .ended_session_history
                .historical_children(&transferred, &stream, "foreign")
                .is_none()
        );
        let reopened: Journal =
            serde_json::from_slice(&serde_json::to_vec(&transferred).unwrap()).unwrap();
        assert!(validate_journal(&reopened).is_ok());
        let key = serde_json::to_string(&("epoch", "session")).unwrap();
        let mut encoded_book = json!({"certificates":{}});
        encoded_book["certificates"][&key] = serde_json::to_value(&child_cert).unwrap();
        with_child.ended_session_history = serde_json::from_value(encoded_book).unwrap();
        with_child.closed_streams = child_book;
        assert!(validate_journal(&with_child).is_err());
        with_child.closed_streams = Default::default();
        assert!(validate_journal(&with_child).is_ok());
        assert!(super::closed_stream::test_archived_root(
            &journal,
            stream.clone(),
            &cert.root,
            1,
            &root_digest
        ));
        assert!(super::closed_stream::test_archived_root(
            &checkpoint,
            stream.clone(),
            &cert.root,
            1,
            &root_digest
        ));
        assert!(!super::closed_stream::test_archived_root(
            &checkpoint,
            stream.clone(),
            &cert.root,
            2,
            &root_digest
        ));
        assert!(!super::closed_stream::test_archived_root(
            &checkpoint,
            stream,
            &cert.root,
            1,
            &"0".repeat(64)
        ));
        let resolve = |snapshot: &Journal,
                       target: &LifecycleTarget,
                       admission: &str,
                       producer: &str,
                       generation| {
            snapshot
                .ended_session_history
                .historical_root(snapshot, target, admission, producer, generation)
                .is_some()
        };
        assert!(resolve(&checkpoint, &work.target, "root", "owner", 1));
        assert!(!resolve(&checkpoint, &work.target, "other", "owner", 1));
        assert!(!resolve(&checkpoint, &work.target, "root", "other", 1));
        assert!(!resolve(&checkpoint, &work.target, "root", "owner", 2));
        let mut wrong_target = work.target.clone();
        if let LifecycleTarget::Session { generation, .. } = &mut wrong_target {
            *generation += 1;
        }
        assert!(!resolve(&checkpoint, &wrong_target, "root", "owner", 1));
        let mut stale = checkpoint.clone();
        stale.authority.sequences.insert("owner".into(), 0);
        assert!(validate_journal(&stale).is_err());
        assert!(!resolve(&stale, &work.target, "root", "owner", 1));
        let mut unfenced = checkpoint.clone();
        unfenced.retention = Default::default();
        assert!(validate_journal(&unfenced).is_err());
        assert!(!resolve(&unfenced, &work.target, "root", "owner", 1));
        let directory = std::env::temp_dir().join(format!(
            "amaterasu-ended-certificate-{}",
            identifier().unwrap()
        ));
        std::fs::create_dir(&directory).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let path = directory.join("journal.json");
        let mut store = LifecycleStore::new(path.clone()).unwrap();
        store.replace(&checkpoint).unwrap();
        drop(store);
        let reopened = Coordinator::open(&path).unwrap();
        assert!(resolve(
            &reopened.shared.borrow().value,
            &work.target,
            "root",
            "owner",
            1
        ));
        assert!(
            reopened
                .shared
                .borrow()
                .value
                .ended_session_history
                .validate(&reopened.shared.borrow().value)
        );
        drop(reopened);
        std::fs::remove_dir_all(directory).unwrap();
        let mut legacy = journal.clone();
        legacy
            .sessions
            .get_mut("session")
            .unwrap()
            .application_retirement_proof = None;
        assert!(super::session_closure::preflight(&legacy, &work.target).is_err());
        let mut pending = journal.clone();
        let key = receipt_key("transport", "client", "unsettled");
        pending.receipts.insert(
            key,
            Receipt {
                caller: "transport".into(),
                initiator: "client".into(),
                request: json!({"operation_id":"unsettled","target":work.target}),
                response: json!({"operation_id":"unsettled","status":"recovery_required"}),
                work: None,
                wait_target: None,
            },
        );
        assert!(super::session_closure::preflight(&pending, &work.target).is_err());
        let mut aliased = journal.clone();
        let source = receipt_key("transport", "client", "source");
        let alias = receipt_key("transport", "client", "alias");
        for (key, operation) in [(&source, "source"), (&alias, "alias")] {
            aliased.receipts.insert(key.clone(), Receipt {
                caller: "transport".into(), initiator: "client".into(),
                request: json!({"operation_id":operation,"target":work.target}),
                response: json!({"operation_id":operation,"status":"succeeded","target":work.target}),
                work: None, wait_target: None,
            });
        }
        aliased.recovery_sources.insert(alias, source);
        assert_eq!(
            super::session_closure::preflight(&aliased, &work.target).unwrap_err(),
            "session_has_live_references"
        );
        let mut external = journal.clone();
        external.authority.settled.insert(
            "external".into(),
            external.authority.settled["root"].clone(),
        );
        assert_eq!(
            super::session_closure::preflight(&external, &work.target).unwrap_err(),
            "session_root_has_external_reference"
        );
        let evidence =
            session_prepare_evidence::validated_closed_prepares(&journal, &work.target).unwrap();
        assert_eq!(evidence.len(), 1);
        assert_eq!(evidence[0].admission_id, "root");
        let callback =
            session_prepare_evidence::closed_prepare_callback(&journal, &work.target, &evidence[0])
                .unwrap();
        assert_eq!(callback.request.admission_id.as_deref(), Some("root"));
        let mut wrong = evidence.clone();
        wrong[0].transition_id = "foreign".into();
        assert!(
            session_prepare_evidence::closed_prepare_callback(&journal, &work.target, &wrong[0])
                .is_err()
        );
        let before = serde_json::to_value(&journal).unwrap();
        assert!(
            session_prepare_evidence::retire_closed_prepares(&mut journal, &work.target, &wrong)
                .is_err()
        );
        assert_eq!(serde_json::to_value(&journal).unwrap(), before);
        session_prepare_evidence::retire_closed_prepares(&mut journal, &work.target, &evidence)
            .unwrap();
        assert!(
            !journal
                .session_prepare_evidence
                .references_admission("root")
        );
    }

    #[test]
    fn ended_certificate_independently_verifies_dependent_noops() {
        use super::session_closed_history::{
            DependentTerminalEvidence, EndedSessionCertificate, TerminalCallbackEvidence,
        };
        let (journal, work, mut owner, mut dependent) = dependent_fixture();
        let mut root = journal.composition.records().next().unwrap().clone();
        root.state = composition::OutputState::Released;
        owner.request.provider_sequence = 2;
        owner.terminal_dispatch.as_mut().unwrap().call.arguments =
            serde_json::to_value(&owner.request).unwrap();
        let mut prepare = dependent.clone();
        prepare.request.phase = LifecyclePhase::SessionPrepare;
        prepare.request.transition_id = root.transition_id.clone();
        prepare.request.operation_id = "settings-prepare-call".into();
        prepare.request.provider_sequence = 1;
        prepare.request.admission_id = Some("settings-prepare".into());
        prepare
            .terminal_dispatch
            .as_mut()
            .unwrap()
            .call
            .operation_id = prepare.request.operation_id.clone();
        prepare.terminal_dispatch.as_mut().unwrap().call.arguments =
            serde_json::to_value(&prepare.request).unwrap();
        prepare.terminal_dispatch.as_mut().unwrap().state =
            crate::provider_dispatch::DispatchState::Completed {
                payload: json!({"status":"prepared","candidate":null,"obligations":[],"outputs":[]}),
            };
        dependent.request.provider_sequence = 2;
        dependent.terminal_dispatch.as_mut().unwrap().call.arguments =
            serde_json::to_value(&dependent.request).unwrap();
        let callback = |id: &str, state: &PrepareState| TerminalCallbackEvidence {
            admission_id: id.into(),
            admission: journal.authority.settled[id].clone(),
            callback: composition::ProducerCallbackProof {
                participant: state.participant.clone(),
                request: state.request.clone(),
                terminal: state.terminal_dispatch.clone().unwrap(),
                pending_receipt: None,
                pending_dispatch: None,
            },
        };
        let session = &journal.sessions["session"];
        let mut certificate = EndedSessionCertificate {
            version: 1,
            target: work.target.clone(),
            owner: session.owner.clone(),
            end_receipt: None,
            graph: session.provider_graph.clone().unwrap(),
            application_ref: json!({"kind":"desktop"}),
            application: None,
            application_operation_id: None,
            application_proof: application::ApplicationRetirementProof::Desktop {
                target: work.target.clone(),
            },
            producer: "owner".into(),
            resource_generation: 1,
            root,
            root_admission: journal.authority.settled["root"].clone(),
            owner_release: callback("release-admission", &owner),
            dependents: vec![DependentTerminalEvidence {
                provider: "settings".into(),
                prepare: callback("settings-prepare", &prepare),
                release: callback("settings-release", &dependent),
            }],
            closed_children: 0,
            children: None,
            provider_sequences: BTreeMap::from([("owner".into(), 2), ("settings".into(), 2)]),
            operation_counter: 1,
            legacy_operation_ids: BTreeSet::from([
                "root-call".into(),
                "release-call".into(),
                "settings-prepare-call".into(),
                "settings-release-call".into(),
            ]),
        };
        assert!(certificate.validate());
        certificate.dependents[0]
            .release
            .callback
            .request
            .provider_sequence = 1;
        certificate.dependents[0]
            .release
            .callback
            .terminal
            .call
            .arguments =
            serde_json::to_value(&certificate.dependents[0].release.callback.request).unwrap();
        assert!(!certificate.validate());
    }

    #[test]
    fn exact_session_and_application_retirement_gate_lineage() {
        let (mut journal, work) = fixture();
        assert!(session_lineage_closed(&journal, &work.target, "owner"));
        journal
            .sessions
            .get_mut("session")
            .unwrap()
            .application_retired = false;
        assert!(!session_lineage_closed(&journal, &work.target, "owner"));
        journal
            .sessions
            .get_mut("session")
            .unwrap()
            .application_retired = true;
        journal.sessions.get_mut("session").unwrap().generation = 2;
        assert!(!session_lineage_closed(&journal, &work.target, "owner"));
        assert!(!session_lineage_closed(&journal, &work.target, "foreign"));
    }

    #[test]
    fn retained_root_without_admission_and_callback_provenance_is_rejected() {
        let (mut journal, work) = fixture();
        let output: amaterasu_extension_api::ResourceOutput = serde_json::from_value(json!({"output_id":"output","capability":"screen","schema":{"id":"screen/2","major":1,"minor":0,"digest":"a".repeat(64)},"producer":"owner","producer_service":"owner/2","origin":work.target,"resource":{"provider":"owner","resource_id":"display","incarnation":"native","revision":2,"execution_domain":"seat","mutation_domain":null,"mutation_domains":[]},"data":{}})).unwrap();
        let plan = serde_json::from_value(json!({"plan_id":"plan","revision":1,"resources":[{"provider":"owner","resource_id":"display","incarnation":null,"expected_revision":1,"execution_domain":"seat","mutation_domains":[],"access":"exclusive"}],"mutation_domains":[],"required_capabilities":[],"constraints":{},"obligations":[],"affected_consumers":[],"consumer_change":"live","candidate_constraint":null,"allowed_degradation":[]})).unwrap();
        let selection = composition::OutputSelection {
            producer: output.producer.clone(),
            service: output.producer_service.clone(),
            capability: output.capability.clone(),
            schema: output.schema.clone(),
        };
        let root = composition::StoredOutput {
            output,
            admission_id: "root".into(),
            transition_id: "prepare".into(),
            plan,
            state: composition::OutputState::Retained,
            callback_proof: None,
        };
        journal
            .composition
            .record(root, &selection, &work.target, &|_| true)
            .unwrap();
        assert_eq!(
            validate_session_release_preflight(&journal, &work, "owner").unwrap_err(),
            "session_root_missing_or_ambiguous"
        );
    }

    #[test]
    fn missing_root_and_malformed_release_graph_fail_before_dispatch() {
        let (journal, mut work) = fixture();
        assert_eq!(
            validate_session_release_preflight(&journal, &work, "owner").unwrap_err(),
            "session_root_missing_or_ambiguous"
        );
        work.provider_graph
            .as_mut()
            .unwrap()
            .dependencies
            .insert("foreign".into(), vec![]);
        assert_eq!(
            validate_session_release_preflight(&journal, &work, "owner").unwrap_err(),
            "session_lineage_not_retired"
        );
        work.provider_graph
            .as_mut()
            .unwrap()
            .dependencies
            .remove("foreign");
        work.providers.push("foreign".into());
        assert_eq!(
            validate_session_release_preflight(&journal, &work, "owner").unwrap_err(),
            "session_lineage_not_retired"
        );
    }
}
