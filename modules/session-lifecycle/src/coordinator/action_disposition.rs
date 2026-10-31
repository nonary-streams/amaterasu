//! Durable proof of an authenticated owner action that released a session root.
use super::*;
use crate::action_authority::{ActionApprovalSnapshot, ActionBinding};
use amaterasu_extension_api::{
    ActionProviderCompletionStatus, ActionResourceDispositionState, ResourcePlan, ResourceRef,
};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ReleasedActionMarker {
    pub session_target: LifecycleTarget,
    pub owner: String,
    pub root_admission_id: String,
    pub root_transition_id: String,
    pub output_id: String,
    pub resource: ResourceRef,
    pub resource_generation: u64,
    pub binding: ActionBinding,
    pub permit: String,
    pub approval_snapshot: ActionApprovalSnapshot,
    pub plan: ResourcePlan,
    pub dispatch_proof: Value,
    pub cleanup_work: Work,
}

pub(super) struct ResolvedDisposition {
    pub session_target: LifecycleTarget,
    pub owner: String,
    pub resource_generation: u64,
    pub resource: ResourceRef,
    pub root_admission_id: String,
}

/// Full marker or compact, independently verified read-only equivalent.
pub(super) fn resolved_at(
    journal: &Journal,
    session_id: &str,
    owner: &str,
    generation: u64,
) -> Option<ResolvedDisposition> {
    if let Some(marker) = verified_marker_at(journal, session_id, owner, generation) {
        if journal
            .closed_actions
            .released_disposition(journal, session_id, owner, generation)
            .is_some()
        {
            return None;
        }
        return Some(ResolvedDisposition {
            session_target: marker.session_target.clone(),
            owner: marker.owner.clone(),
            resource_generation: marker.resource_generation,
            resource: marker.resource.clone(),
            root_admission_id: marker.root_admission_id.clone(),
        });
    }
    journal
        .closed_actions
        .released_disposition(journal, session_id, owner, generation)
}

pub(super) fn marker_key(session_id: &str, owner: &str, generation: u64) -> String {
    serde_json::to_string(&(session_id, owner, generation)).expect("marker key serializes")
}

/// Resolve nominated cleanup IDs solely from the authenticated producer plan.
/// IDs are claims until tied to unique full obligation tuples in this session.
pub(super) fn selected_root_obligations(
    journal: &Journal,
    root: &composition::StoredOutput,
    ids: &[String],
) -> Option<Vec<amaterasu_extension_api::ResourceObligation>> {
    if ids.len() > 128 || !ids.windows(2).all(|pair| pair[0] < pair[1]) {
        return None;
    }
    let LifecycleTarget::Session { session_id, .. } = &root.output.origin else {
        return None;
    };
    let session = journal.sessions.get(session_id)?;
    let admission = journal.authority.settled.get(&root.admission_id)?;
    if admission.provider != root.output.producer || admission.plan != root.plan {
        return None;
    }
    let mut selected = Vec::new();
    for id in ids {
        let mut obligations = root
            .plan
            .obligations
            .iter()
            .filter(|item| &item.obligation_id == id);
        let item = obligations.next()?;
        if obligations.next().is_some()
            || item.provider != root.output.producer
            || item.resource_id != root.output.resource.resource_id
            || session
                .obligations
                .iter()
                .filter(|value| value.obligation_id == *id)
                .count()
                != 1
            || !session.obligations.contains(item)
            || session
                .admission_ids
                .iter()
                .filter(|other| *other != &root.admission_id)
                .any(|other| {
                    journal
                        .authority
                        .admissions
                        .get(other)
                        .or_else(|| journal.authority.settled.get(other))
                        .is_none_or(|admission| {
                            admission
                                .plan
                                .obligations
                                .iter()
                                .any(|value| value.obligation_id == *id)
                        })
                })
        {
            return None;
        }
        selected.push(item.clone());
    }
    Some(selected)
}

/// Obtain only owner-attested Released outcomes from an already verified marker.
pub(super) fn released_root_obligations(
    journal: &Journal,
    root: &composition::StoredOutput,
    marker: &ReleasedActionMarker,
) -> Option<Vec<amaterasu_extension_api::ResourceObligation>> {
    let disposition = marker.plan.resource_disposition.as_deref()?;
    let selected = selected_root_obligations(journal, root, &disposition.cleanup_obligation_ids)?;
    let completed = action_dispatch_proof::validate_dispatch_proof(
        &marker.dispatch_proof,
        &marker.binding,
        &marker.permit,
        &marker.plan,
        &marker.approval_snapshot,
    )
    .ok()?;
    let result = completed.resource_disposition?;
    Some(released_outcome_tuples(selected, &result.cleanup_outcomes))
}

fn released_outcome_tuples(
    selected: Vec<amaterasu_extension_api::ResourceObligation>,
    outcomes: &[amaterasu_extension_api::CleanupOutcome],
) -> Vec<amaterasu_extension_api::ResourceObligation> {
    selected
        .into_iter()
        .filter(|item| {
            outcomes.iter().any(|outcome| {
                outcome.obligation_id == item.obligation_id
                    && outcome.state == amaterasu_extension_api::CleanupState::Released
            })
        })
        .collect()
}

/// Reprepare may not hide unresolved cleanup of the removed root.
pub(super) fn root_cleanup_complete(
    journal: &Journal,
    session_id: &str,
    owner: &str,
    generation: u64,
) -> bool {
    let mut roots = journal.composition.records().filter(|root| {
        root.output.producer == owner && root.state == composition::OutputState::Released
            && matches!(&root.output.origin, LifecycleTarget::Session { session_id: id, .. } if id == session_id)
            && resource_generation::historical_generation(journal, root).ok() == Some(generation)
    });
    let Some(root) = roots.next() else {
        return false;
    };
    if roots.next().is_some() {
        return false;
    }
    let discharged = journal
        .closed_actions
        .certifies_cleanup_plan(root, generation)
        .then(|| {
            journal.closed_actions.discharged_obligations(
                &root.admission_id,
                &root.transition_id,
                owner,
                generation,
            )
        })
        .flatten()
        .map(<[_]>::to_vec)
        .or_else(|| {
            let marker = verified_marker_at(journal, session_id, owner, generation)?;
            released_root_obligations(journal, root, marker)
        });
    discharged.is_some_and(|items| {
        root.plan
            .obligations
            .iter()
            .all(|item| items.contains(item))
    })
}

/// Revalidate historical authenticated evidence. Current cursor and a new root
/// are checked separately by reprepare; this remains valid after cursor advance.
pub(super) fn verified_marker<'a>(
    journal: &'a Journal,
    session_id: &str,
    owner: &str,
) -> Option<&'a ReleasedActionMarker> {
    let generation = *journal
        .sessions
        .get(session_id)?
        .resource_generations
        .get(owner)?;
    verified_marker_at(journal, session_id, owner, generation)
}

pub(super) fn verified_marker_at<'a>(
    journal: &'a Journal,
    session_id: &str,
    owner: &str,
    generation: u64,
) -> Option<&'a ReleasedActionMarker> {
    let marker = journal
        .action_dispositions
        .get(&marker_key(session_id, owner, generation))?;
    let session = journal.sessions.get(session_id)?;
    let expected = LifecycleTarget::Session {
        host_epoch: session.host_epoch.clone(),
        session_id: session.id.clone(),
        generation: session.generation,
    };
    if marker.session_target != expected || marker.owner != owner || marker.resource_generation == 0
    {
        return None;
    }
    let mut roots = journal.composition.records().filter(|root| {
        root.output.origin == expected
            && root.output.producer == owner
            && root.output.output_id == marker.output_id
            && root.admission_id == marker.root_admission_id
            && root.transition_id == marker.root_transition_id
            && root.state == composition::OutputState::Released
            && root.output.resource == marker.resource
    });
    let root = roots.next()?;
    if roots.next().is_some()
        || resource_generation::historical_generation(journal, root).ok()?
            != marker.resource_generation
    {
        return None;
    }
    let transfer = marker.cleanup_work.action_transfer.as_ref()?;
    if transfer.stage != ActionTransferStage::Rebound
        || transfer.binding != marker.binding
        || transfer.permit != marker.permit
    {
        return None;
    }
    let (provider, _, plan) = journal
        .actions
        .execution_for_permit(&transfer.service, &marker.permit)
        .ok()?;
    if provider != owner
        || plan != marker.plan
        || journal
            .actions
            .approval_snapshot_for_permit(&transfer.service, &marker.permit)
            .ok()?
            != marker.approval_snapshot
    {
        return None;
    }
    let admission = journal
        .authority
        .settled
        .get(transfer.admission_id.as_ref()?)?;
    if admission.provider != owner || admission.plan != marker.plan {
        return None;
    }
    let proof = transfer.release_proof.as_ref()?;
    if !action_release_proof::validate_historical_graph_callbacks(
        journal,
        &marker.cleanup_work,
        proof,
    ) {
        return None;
    }
    // The stream cleanup preceded this action's authenticated removal. Verify
    // that historical proof in a private pre-release view of the exact root;
    // the real journal remains Released and the terminal action proof below is
    // still mandatory before this marker can be accepted.
    let mut historical = journal.clone();
    historical.composition = journal.composition.historical_pre_release_view(root)?;
    if !action_release_proof::validate_owner_release(&historical, &marker.cleanup_work, owner) {
        return None;
    }
    let completed = action_dispatch_proof::validate_dispatch_proof(
        &marker.dispatch_proof,
        &marker.binding,
        &marker.permit,
        &marker.plan,
        &marker.approval_snapshot,
    )
    .ok()?;
    if !journal.actions.terminal_succeeded_for(
        &transfer.service,
        &marker.permit,
        &marker.binding,
        &completed.receipt,
    ) {
        return None;
    }
    let result = completed.resource_disposition?;
    let disposition = marker.plan.resource_disposition.as_deref()?;
    selected_root_obligations(journal, root, &disposition.cleanup_obligation_ids)?;
    if completed.status != ActionProviderCompletionStatus::Succeeded
        || result.state != ActionResourceDispositionState::Released
        || result.output_id != marker.output_id
        || result.origin != marker.session_target
        || result.resource_generation != marker.resource_generation
        || result.resource.provider != marker.resource.provider
        || result.resource.resource_id != marker.resource.resource_id
        || result.resource.incarnation != marker.resource.incarnation
    {
        return None;
    }
    Some(marker)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_explicit_released_outcomes_discharge_selected_tuples() {
        let obligation: amaterasu_extension_api::ResourceObligation = serde_json::from_value(json!({
            "obligation_id":"old", "provider":"owner", "resource_id":"resource", "mutation_domain":null,
            "depends_on":[], "cleanup_method":{"service":"owner/2", "method":"cleanup", "interface_major":2,
            "interface_minor":0,"schema_revision":1}, "payload":{"original":true}
        })).unwrap();
        let outcome = |state| amaterasu_extension_api::CleanupOutcome {
            obligation_id: "old".into(),
            state,
            reason: None,
        };
        assert!(released_outcome_tuples(vec![obligation.clone()], &[]).is_empty());
        for state in [
            amaterasu_extension_api::CleanupState::Pending,
            amaterasu_extension_api::CleanupState::DeferredRestoration,
            amaterasu_extension_api::CleanupState::EffectUnknown,
        ] {
            assert!(
                released_outcome_tuples(vec![obligation.clone()], &[outcome(state)]).is_empty()
            );
        }
        assert_eq!(
            released_outcome_tuples(
                vec![obligation.clone()],
                &[outcome(amaterasu_extension_api::CleanupState::Released)]
            ),
            vec![obligation]
        );
    }

    #[test]
    fn unproven_marker_cannot_survive_recovery_validation() {
        let method = json!({"service":"owner/1","method":"execute","interface_major":1,"interface_minor":0,"schema_revision":1});
        let approval = json!({"provider":"owner","callers":["action-service"],"method":{"service":"owner/1","method":"execute","interface_major":1,"interface_minor":0,"request":{"required":{}},"response":{"required":{}},"privilege":"user"}});
        let session =
            json!({"kind":"session","host_epoch":"epoch","session_id":"session","generation":1});
        let target = json!({"kind":"session","host_epoch":"epoch","session_id":"session","expected_generation":1});
        let marker: ReleasedActionMarker = serde_json::from_value(json!({"session_target":session,"owner":"owner","root_admission_id":"root","root_transition_id":"prepare","output_id":"output","resource":{"provider":"owner","resource_id":"display","incarnation":"native","revision":1,"execution_domain":"seat","mutation_domain":null,"mutation_domains":[]},"resource_generation":1,"binding":{"operation_id":"op","action_id":"owner/remove","target":target,"plan_digest":"a".repeat(64),"source":{"caller":"transport","initiator":"user"}},"permit":"a".repeat(32),"approval_snapshot":{"descriptor":{"id":"owner/remove","label":"Remove","description":"Remove","execution":"session_mutation","arguments":{},"allowed_targets":["session"],"required_capabilities":[],"plan":null,"execute":method,"status":null},"execute_approval":approval,"status_approval":null},"plan":{"plan_id":"plan","revision":1,"resources":[],"mutation_domains":[],"required_capabilities":[],"constraints":{},"obligations":[],"affected_consumers":[],"consumer_change":"live","candidate_constraint":null,"allowed_degradation":[]},"dispatch_proof":{},"cleanup_work":{"phase":"stream.release","target":{"kind":"stream","host_epoch":"epoch","session_id":"session","stream_id":"stream","generation":1},"intent":null,"reason":null,"video":null,"transition_id":"transition","providers":[],"index":0,"states":{},"candidate":null,"waiting_retirement":false}})).unwrap();
        let mut journal: Journal = serde_json::from_value(
            json!({"version":3,"host_epoch":"epoch","receipts":{},"sessions":{}}),
        )
        .unwrap();
        journal
            .action_dispositions
            .insert(marker_key("session", "owner", 1), marker);
        assert!(verified_marker(&journal, "session", "owner").is_none());
        assert!(validate_journal(&journal).is_err());
        let session: SessionRecord = serde_json::from_value(json!({"host_epoch":"epoch","id":"session","generation":1,"phase":"paused","owner":{"caller":"transport","initiator":"user"},"delegates":[],"application_ref":null,"settings_revision":"revision","application_retired":false,"streams":{},"resource_generations":{"owner":2}})).unwrap();
        journal.sessions.insert("session".into(), session);
        assert!(verified_marker_at(&journal, "session", "owner", 1).is_none());
        assert!(verified_marker(&journal, "session", "owner").is_none());
        assert!(validate_journal(&journal).is_err());
    }
}
