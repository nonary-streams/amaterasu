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

pub(super) fn marker_key(session_id: &str, owner: &str) -> String {
    serde_json::to_string(&(session_id, owner)).expect("string tuple serializes")
}

/// Revalidate historical authenticated evidence. Current cursor and a new root
/// are checked separately by reprepare; this remains valid after cursor advance.
pub(super) fn verified_marker<'a>(
    journal: &'a Journal,
    session_id: &str,
    owner: &str,
) -> Option<&'a ReleasedActionMarker> {
    let marker = journal
        .action_dispositions
        .get(&marker_key(session_id, owner))?;
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
    if !action_release_proof::validate_graph_callbacks(journal, &marker.cleanup_work, proof)
        || !journal.stream_releases.has_released_for(
            &marker.cleanup_work.target,
            owner,
            &marker.cleanup_work.transition_id,
        )
        || !journal
            .stream_releases
            .has_valid_released(journal, &marker.cleanup_work.target)
    {
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
