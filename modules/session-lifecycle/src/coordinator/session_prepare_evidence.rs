//! Authenticated session-prepare callbacks, including zero-effect participants.
use super::*;
use crate::prepare_driver::PreparePhase;
use amaterasu_extension_api::{LifecyclePhase, ProviderOperation, ProviderResponse};

const MAX_EVIDENCE: usize = 512;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Evidence {
    target: LifecycleTarget,
    transition_id: String,
    admission_id: String,
    proof: composition::ProducerCallbackProof,
    response: ProviderResponse,
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SessionPrepareEvidence {
    records: Vec<Evidence>,
}

impl SessionPrepareEvidence {
    pub fn references_admission(&self, id: &str) -> bool {
        self.records.iter().any(|record| record.admission_id == id)
    }
    pub fn validate(&self, journal: &Journal) -> bool {
        self.records.len() <= MAX_EVIDENCE
            && self.records.iter().enumerate().all(|(index, record)| {
                !self.records[..index]
                    .iter()
                    .any(|old| old.admission_id == record.admission_id)
                    && valid_record(journal, record)
            })
    }
}

/// Exact, authenticated prepare references retained until session closure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ClosedPrepareEvidence {
    pub provider: String,
    pub admission_id: String,
    pub transition_id: String,
}

/// Resolve the complete session prepare set. This is a historical proof only;
/// callers must separately verify terminal release and all inbound references.
#[allow(dead_code, reason = "used by the ended-session closure transaction")]
pub(super) fn validated_closed_prepares(
    journal: &Journal,
    target: &LifecycleTarget,
) -> ModuleResult<Vec<ClosedPrepareEvidence>> {
    let (epoch, session_id, stream, generation) = operations::target_parts(target)?;
    let session = journal.sessions.get(session_id).ok_or("unknown_session")?;
    let graph = session
        .provider_graph
        .as_ref()
        .ok_or("provider_graph_missing")?;
    if stream.is_some()
        || session.host_epoch != epoch
        || session.generation != generation
        || session.phase != SessionPhase::Ended
        || !journal.session_prepare_evidence.validate(journal)
    {
        return Err("invalid_ended_session_prepares".into());
    }
    let mut result = Vec::new();
    for id in &session.admission_ids {
        if session.admission_targets.get(id) != Some(target)
            || session.admission_phases.get(id) != Some(&LifecyclePhase::SessionPrepare)
        {
            continue;
        }
        let admission = journal
            .authority
            .settled
            .get(id)
            .ok_or("unsettled_session_prepare")?;
        let mut records = journal
            .session_prepare_evidence
            .records
            .iter()
            .filter(|record| &record.admission_id == id && &record.target == target);
        let record = records.next().ok_or("session_prepare_evidence_missing")?;
        if records.next().is_some()
            || !valid_record(journal, record)
            || !graph.order.contains(&admission.provider)
        {
            return Err("invalid_session_prepare_evidence".into());
        }
        result.push(ClosedPrepareEvidence {
            provider: admission.provider.clone(),
            admission_id: id.clone(),
            transition_id: record.transition_id.clone(),
        });
    }
    if graph
        .order
        .iter()
        .any(|provider| !result.iter().any(|record| &record.provider == provider))
        || journal
            .session_prepare_evidence
            .records
            .iter()
            .filter(|record| &record.target == target)
            .count()
            != result.len()
    {
        return Err("incomplete_session_prepare_evidence".into());
    }
    let unique_providers: BTreeSet<_> = result.iter().map(|item| &item.provider).collect();
    let unique_graph: BTreeSet<_> = graph.order.iter().collect();
    if unique_providers.len() != result.len() || unique_graph.len() != graph.order.len() {
        return Err("owner_generation_lineage_not_compactable".into());
    }
    if result
        .iter()
        .map(|item| &item.transition_id)
        .collect::<BTreeSet<_>>()
        .len()
        != 1
    {
        return Err("session_prepare_transition_ambiguous".into());
    }
    result.sort_by(|a, b| a.admission_id.cmp(&b.admission_id));
    Ok(result)
}

/// Called only on a private journal clone in the atomic terminal transaction.
#[allow(dead_code, reason = "used by the ended-session closure transaction")]
pub(super) fn retire_closed_prepares(
    journal: &mut Journal,
    target: &LifecycleTarget,
    expected: &[ClosedPrepareEvidence],
) -> ModuleResult<()> {
    if validated_closed_prepares(journal, target)? != expected {
        return Err("session_prepare_closure_changed".into());
    }
    journal
        .session_prepare_evidence
        .records
        .retain(|record| &record.target != target);
    Ok(())
}

/// Recover the exact callback transcript only after revalidating its source.
#[allow(
    dead_code,
    reason = "used by the ended-session certificate constructor"
)]
pub(super) fn closed_prepare_callback(
    journal: &Journal,
    target: &LifecycleTarget,
    expected: &ClosedPrepareEvidence,
) -> ModuleResult<composition::ProducerCallbackProof> {
    let all = validated_closed_prepares(journal, target)?;
    if !all.contains(expected) {
        return Err("session_prepare_not_in_closure".into());
    }
    let mut records = journal
        .session_prepare_evidence
        .records
        .iter()
        .filter(|record| &record.target == target && record.admission_id == expected.admission_id);
    let record = records.next().ok_or("session_prepare_evidence_missing")?;
    if records.next().is_some() || !valid_record(journal, record) {
        return Err("session_prepare_evidence_ambiguous".into());
    }
    Ok(record.proof.clone())
}

fn valid_record(journal: &Journal, record: &Evidence) -> bool {
    let Ok((epoch, session_id, None, generation)) = operations::target_parts(&record.target) else {
        return false;
    };
    let Some(session) = journal.sessions.get(session_id) else {
        return false;
    };
    let Some(admission) = journal
        .authority
        .admissions
        .get(&record.admission_id)
        .or_else(|| journal.authority.settled.get(&record.admission_id))
    else {
        return false;
    };
    let proof = &record.proof;
    let provider = &admission.provider;
    let request = &proof.request;
    session.host_epoch == epoch
        && session.generation == generation
        && session
            .provider_graph
            .as_ref()
            .is_some_and(|graph| graph.order.contains(provider))
        && session.admission_ids.contains(&record.admission_id)
        && session.admission_targets.get(&record.admission_id) == Some(&record.target)
        && session.admission_phases.get(&record.admission_id)
            == Some(&LifecyclePhase::SessionPrepare)
        && journal.admission_transitions.get(&record.admission_id) == Some(&record.transition_id)
        && request.operation == ProviderOperation::LifecycleCallback
        && request.phase == LifecyclePhase::SessionPrepare
        && request.target == record.target
        && request.transition_id == record.transition_id
        && request.admission_id.as_deref() == Some(record.admission_id.as_str())
        && request.approved_plan.as_ref() == Some(&admission.plan)
        && proof.participant == admission.selection.participant
        && matches!(&record.response, ProviderResponse::Prepared { .. })
        && composition::valid_callback_proof(proof, &record.response)
        && composition::callback_approvals_valid(proof, &admission.selection.approvals, provider)
}

/// Record on the clone that atomically receives settled authority and progress.
pub(super) fn record(
    journal: &mut Journal,
    work: &Work,
    provider: &str,
    state: &PrepareState,
) -> ModuleResult<()> {
    if work.phase != LifecyclePhase::SessionPrepare
        || state.phase != PreparePhase::Prepared
        || state.provider != provider
        || state.request.target != work.target
        || state.request.transition_id != work.transition_id
    {
        return Err("session_prepare_evidence_context_invalid".into());
    }
    let persisted = journal.receipts.values().any(|receipt| {
        receipt.work.as_ref().is_some_and(|saved| {
            saved.target == work.target
                && saved.transition_id == work.transition_id
                && saved.states.get(provider).is_some_and(|s| {
                    serde_json::to_value(s).ok() == serde_json::to_value(state).ok()
                })
        })
    });
    if !persisted {
        return Err("session_prepare_result_not_journaled".into());
    }
    let admission_id = state.admission_id.clone().ok_or("admission_missing")?;
    let record = Evidence {
        target: work.target.clone(),
        transition_id: work.transition_id.clone(),
        admission_id,
        proof: composition::ProducerCallbackProof {
            participant: state.participant.clone(),
            request: state.request.clone(),
            terminal: state
                .terminal_dispatch
                .clone()
                .ok_or("provider_terminal_proof_missing")?,
            pending_receipt: state.pending_receipt.clone(),
            pending_dispatch: state.pending_dispatch.clone(),
        },
        response: state.response.clone().ok_or("provider_response_missing")?,
    };
    if !valid_record(journal, &record) {
        return Err("session_prepare_evidence_invalid".into());
    }
    if journal
        .session_prepare_evidence
        .references_admission(&record.admission_id)
    {
        return Err("session_prepare_evidence_duplicate".into());
    }
    if journal.session_prepare_evidence.records.len() >= MAX_EVIDENCE {
        return Err("session_prepare_evidence_capacity".into());
    }
    journal.session_prepare_evidence.records.push(record);
    Ok(())
}

/// Find the exact original callback evidence for a participant and target.
#[allow(dead_code)] // Consumed by the action proof integration.
pub(super) fn admission_for<'a>(
    journal: &'a Journal,
    target: &LifecycleTarget,
    provider: &str,
) -> Option<(&'a str, &'a str)> {
    let mut records = journal
        .session_prepare_evidence
        .records
        .iter()
        .filter(|record| {
            &record.target == target
                && record.proof.participant.provider == provider
                && valid_record(journal, record)
        });
    let record = records.next()?;
    if records.next().is_some() {
        return None;
    }
    Some((&record.admission_id, &record.transition_id))
}

/// Prove each transitive dependent completed a no-effect session preparation.
#[allow(dead_code)] // Consumed by the action proof integration.
pub(super) fn all_dependent_session_prepares_noop(
    journal: &Journal,
    target: &LifecycleTarget,
    graph: &ProviderGraph,
    owner: &str,
) -> bool {
    let Ok((_, session_id, None, _)) = operations::target_parts(target) else {
        return false;
    };
    let Some(session) = journal.sessions.get(session_id) else {
        return false;
    };
    if serde_json::to_value(&session.provider_graph).ok() != serde_json::to_value(Some(graph)).ok()
        || !graph.order.iter().any(|provider| provider == owner)
    {
        return false;
    }
    let mut affected = BTreeSet::from([owner.to_owned()]);
    loop {
        let previous = affected.len();
        for provider in &graph.order {
            let Some(dependencies) = graph.dependencies.get(provider) else {
                return false;
            };
            if dependencies
                .iter()
                .any(|dependency| affected.contains(dependency))
            {
                affected.insert(provider.clone());
            }
        }
        if affected.len() == previous {
            break;
        }
    }
    affected.remove(owner);
    affected.iter().all(|provider| {
        let Some((id, _)) = admission_for(journal, target, provider) else { return false; };
        let Some(admission) = journal.authority.settled.get(id) else { return false; };
        let plan = &admission.plan;
        let Some(record) = journal.session_prepare_evidence.records.iter().find(|record| record.admission_id == id) else { return false; };
        let no_effect_response = matches!(&record.response, ProviderResponse::Prepared { candidate: None, obligations, outputs } if obligations.is_empty() && outputs.is_empty());
        no_effect_response && plan.resources.is_empty() && plan.mutation_domains.is_empty()
            && plan.obligations.is_empty() && plan.control_proposals.is_empty()
            && plan.candidate_constraint.is_none() && plan.affected_consumers.is_empty()
            && plan.consumer_change == amaterasu_extension_api::ConsumerChange::Live
            && !journal.composition.records().any(|output| output.output.origin == *target && output.output.producer == *provider)
            && session.admission_ids.iter().filter(|other| {
                session.admission_targets.get(*other) == Some(target)
                    && session.admission_phases.get(*other) == Some(&LifecyclePhase::SessionPrepare)
                    && journal.authority.settled.get(*other).or_else(|| journal.authority.admissions.get(*other))
                        .is_some_and(|entry| entry.provider == *provider)
            }).count() == 1
    })
}
