//! Provider-scoped recovery and observation metadata.
use super::*;
use amaterasu_extension_api::{MutationDomainPlan, ResourceAccess};

const MAX_ITEMS: usize = 128;
const MAX_RESPONSE_BYTES: usize = 16 * 1024;

fn obligation_remains_live(
    obligation: &amaterasu_extension_api::ResourceObligation,
    closed: &[&amaterasu_extension_api::ResourceObligation],
    live: &[&amaterasu_extension_api::ResourceObligation],
) -> bool {
    !closed.contains(&obligation) || live.contains(&obligation)
}

fn root_plan_is_scoped_to_released_resource(root: &super::composition::StoredOutput) -> bool {
    let resource = &root.output.resource;
    let domains: BTreeSet<_> = resource
        .mutation_domains
        .iter()
        .chain(resource.mutation_domain.iter())
        .collect();
    !root.plan.resources.is_empty()
        && root.plan.resources.iter().all(|planned| {
            planned.provider == resource.provider
                && planned.resource_id == resource.resource_id
                && planned.execution_domain == resource.execution_domain
                && planned
                    .incarnation
                    .as_ref()
                    .is_none_or(|incarnation| incarnation == &resource.incarnation)
                && planned
                    .mutation_domains
                    .iter()
                    .all(|domain| domains.contains(domain))
        })
        && root
            .plan
            .mutation_domains
            .iter()
            .all(|domain| domains.contains(&domain.domain))
        && root.plan.obligations.iter().all(|obligation| {
            obligation.provider == resource.provider
                && obligation.resource_id == resource.resource_id
                && obligation
                    .mutation_domain
                    .as_ref()
                    .is_none_or(|domain| domains.contains(domain))
        })
}

/// Return only admissions named by an authenticated stream cleanup chain.
fn closed_stream_admissions(
    journal: &Journal,
    session: &SessionRecord,
) -> ModuleResult<BTreeSet<String>> {
    let mut closed = BTreeSet::new();
    for (stream_id, stream) in &session.streams {
        let target = LifecycleTarget::Stream {
            host_epoch: session.host_epoch.clone(),
            session_id: session.id.clone(),
            stream_id: stream_id.clone(),
            generation: stream.generation,
        };
        let has_standalone = journal.stream_releases.has_released(&target);
        let has_composed = journal.composed_releases.contains_target(&target);
        if !has_standalone && !has_composed {
            continue;
        }
        let (cleanup_id, _) = journal
            .verified_released_output(&target)
            .ok_or("stream_release_unproven")?;
        let mut results = journal
            .finalize_results
            .records()
            .filter(|result| result.request.target == target);
        let finalized = results.next().ok_or("stream_release_finalize_missing")?;
        if results.next().is_some() {
            return Err("stream_release_finalize_ambiguous".into());
        }
        let permit = journal
            .authority
            .finalize_permits
            .get(&finalized.request.permit_id)
            .ok_or("stream_release_finalize_permit_missing")?;
        if !finalized.validate_structure()
            || !finalized.validate_control_chain(journal)
            || !permit.consumed
            || !permit.settled
            || permit.request != finalized.request
            || !journal
                .authority
                .output_admissions
                .contains(&finalized.request.admission_id)
        {
            return Err("stream_release_finalize_unproven".into());
        }
        let generation =
            super::resource_generation::historical_generation(journal, &finalized.selection.input)?;
        let owner = &finalized.selection.policy.owner_provider;
        if session.resource_generations.get(owner) != Some(&generation) {
            // Earlier generations are handled by the independently verified action closure.
            continue;
        }
        let stage = finalized
            .selection
            .stage
            .as_ref()
            .ok_or("stream_release_stage_missing")?;
        let mut candidates = BTreeSet::from([
            stage.admission_id.clone(),
            finalized.request.admission_id.clone(),
            cleanup_id.to_owned(),
        ]);
        if has_composed {
            let transition = journal
                .admission_transitions
                .get(cleanup_id)
                .ok_or("stream_release_transition_missing")?;
            let work = journal
                .composed_releases
                .verified_work_for(journal, &target, transition)
                .ok_or("stream_release_unproven")?;
            let source = finalized
                .chain_work
                .as_ref()
                .ok_or("stream_release_source_missing")?;
            candidates.extend(
                work.states
                    .values()
                    .filter_map(|state| state.admission_id.clone()),
            );
            candidates.extend(
                source
                    .states
                    .values()
                    .filter_map(|state| state.admission_id.clone()),
            );
        }
        for id in candidates {
            if !session.admission_ids.contains(&id)
                || session.admission_targets.get(&id) != Some(&target)
                || !matches!(
                    session.admission_phases.get(&id),
                    Some(LifecyclePhase::StreamPrepare | LifecyclePhase::StreamRelease)
                )
                || !journal.authority.settled.contains_key(&id)
                || journal.authority.admissions.contains_key(&id)
            {
                return Err("stream_release_admission_invalid".into());
            }
            closed.insert(id);
        }
    }
    Ok(closed)
}

/// Only authenticated terminal action evidence closes historical admissions.
/// Domain names and a released output flag alone do not prove cleanup.
fn closed_admissions(journal: &Journal, session: &SessionRecord) -> ModuleResult<BTreeSet<String>> {
    let target = LifecycleTarget::Session {
        host_epoch: session.host_epoch.clone(),
        session_id: session.id.clone(),
        generation: session.generation,
    };
    let mut closed = closed_stream_admissions(journal, session)?;
    closed.extend(
        journal
            .closed_actions
            .closed_admissions(&target)
            .ok_or("closed_action_invalid")?,
    );
    for root in journal.composition.records().filter(|root| {
        root.output.origin == target && root.state == super::composition::OutputState::Released
    }) {
        if journal
            .session_releases
            .has_valid_released(journal, &target, &root.output.producer)
        {
            // Standalone session closure is not an action-reprepare proof; keep
            // its old admissions under the existing metadata rules.
            continue;
        }
        let generation = super::resource_generation::historical_generation(journal, root)?;
        let marker = super::action_disposition::verified_marker_at(
            journal,
            &session.id,
            &root.output.producer,
            generation,
        );
        if marker.is_none_or(|marker| marker.root_admission_id != root.admission_id)
            && !journal.closed_actions.certifies_root(root, generation)
        {
            return Err("historical_release_unproven".into());
        }
        if !root_plan_is_scoped_to_released_resource(root) {
            return Err("historical_release_scope_ambiguous".into());
        }
    }
    for marker in journal
        .action_dispositions
        .values()
        .filter(|marker| marker.session_target == target)
    {
        let verified = super::action_disposition::verified_marker_at(
            journal,
            &session.id,
            &marker.owner,
            marker.resource_generation,
        )
        .ok_or("historical_release_unproven")?;
        if verified.resource_generation != marker.resource_generation
            || session
                .resource_generations
                .get(&marker.owner)
                .is_none_or(|cursor| *cursor < marker.resource_generation)
        {
            return Err("historical_release_generation_invalid".into());
        }
        let transfer = marker
            .cleanup_work
            .action_transfer
            .as_ref()
            .ok_or("historical_release_unproven")?;
        let snapshot = transfer
            .snapshot
            .as_ref()
            .ok_or("historical_release_unproven")?;
        let proof = transfer
            .release_proof
            .as_ref()
            .ok_or("historical_release_unproven")?;
        closed.insert(marker.root_admission_id.clone());
        closed.extend(snapshot.stream_admissions.keys().cloned());
        closed.extend(
            proof
                .callbacks
                .iter()
                .filter_map(|callback| callback.request.admission_id.clone()),
        );
        if let Some(id) = &transfer.admission_id {
            closed.insert(id.clone());
        }
    }
    if closed
        .iter()
        .any(|id| journal.authority.admissions.contains_key(id))
        || journal.composition.records().any(|root| {
            root.output.origin == target
                && root.state == super::composition::OutputState::Retained
                && closed.contains(&root.admission_id)
        })
    {
        return Err("historical_release_conflicts_with_active_admission".into());
    }
    Ok(closed)
}

// The released root remains evidence for observing its domain, never authority
// to recover its resource, obligations, or effect rights. Restrict this exception
// to the one persisted, proof-authorized replacement prepare.
fn reprepare_domains(
    journal: &Journal,
    session: &SessionRecord,
    provider: &str,
) -> ModuleResult<BTreeMap<String, ResourceAccess>> {
    let mut works = journal.receipts.values().filter_map(|receipt| {
        let work = receipt.work.as_ref()?;
        let continuation = work.reprepare.as_ref()?;
        (receipt.response["status"] == "pending"
            && continuation.owner == provider
            && work.phase == LifecyclePhase::SessionPrepare
            && work.target
                == (LifecycleTarget::Session {
                    host_epoch: session.host_epoch.clone(),
                    session_id: session.id.clone(),
                    generation: session.generation,
                }))
        .then_some(work)
    });
    let Some(work) = works.next() else {
        return Ok(BTreeMap::new());
    };
    if works.next().is_some() || !work.reprepare_authorized(journal) {
        return Err("reprepare_observation_unproven".into());
    }
    let state = work
        .states
        .get(provider)
        .ok_or("reprepare_observation_unproven")?;
    // Once the replacement plan is admitted, ordinary metadata comes from its
    // new admission; the released-root observation exception has expired.
    if state.request.operation != amaterasu_extension_api::ProviderOperation::ResourcePlan {
        return Ok(BTreeMap::new());
    }
    if state.provider != provider
        || state.request.target != work.target
        || state.request.transition_id != work.transition_id
        || state.request.phase != LifecyclePhase::SessionPrepare
        || state.request.version != amaterasu_extension_api::COMPOSITION_VERSION
        || state.request.resource_generation != session.resource_generations.get(provider).copied()
        || !matches!(
            state.phase,
            crate::prepare_driver::PreparePhase::Plan
                | crate::prepare_driver::PreparePhase::PlanStatus
        )
        || !state.dispatch.as_ref().is_some_and(|dispatch| {
            matches!(
                dispatch.state,
                crate::provider_dispatch::DispatchState::OutcomeUnknown
            )
        })
    {
        return Err("reprepare_observation_unproven".into());
    }
    let continuation = work
        .reprepare
        .as_ref()
        .ok_or("reprepare_observation_unproven")?;
    let marker = super::action_disposition::resolved_at(
        journal,
        &session.id,
        provider,
        continuation.old_generation,
    )
    .ok_or("reprepare_observation_unproven")?;
    let mut roots = journal.composition.records().filter(|root| {
        root.admission_id == marker.root_admission_id
            && root.output.origin == work.target
            && root.output.producer == provider
            && root.output.resource == marker.resource
            && root.state == super::composition::OutputState::Released
    });
    let root = roots.next().ok_or("reprepare_observation_unproven")?;
    if roots.next().is_some() {
        return Err("reprepare_observation_unproven".into());
    }
    scoped_root_domains(journal, root, provider)
}

fn scoped_root_domains(
    journal: &Journal,
    root: &super::composition::StoredOutput,
    provider: &str,
) -> ModuleResult<BTreeMap<String, ResourceAccess>> {
    if !root_plan_is_scoped_to_released_resource(root) {
        return Err("reprepare_observation_unproven".into());
    }
    let mut domains = BTreeMap::new();
    for domain in &root.plan.mutation_domains {
        if journal
            .authority
            .domain_owners
            .get(&domain.domain)
            .map(String::as_str)
            != Some(provider)
            || !journal
                .authority
                .snapshot
                .domains
                .contains_key(&domain.domain)
            || domains
                .insert(domain.domain.clone(), domain.access.clone())
                .is_some()
        {
            return Err("reprepare_observation_unproven".into());
        }
    }
    Ok(domains)
}

/// This is called only after exact broker caller/request/admission verification.
/// Domain entries are the last authenticated observations; entries in
/// `stale_mutation_domains` must be refreshed before they are used as current.
pub(super) fn metadata(
    journal: &Journal,
    session: &SessionRecord,
    provider: &str,
) -> ModuleResult<Value> {
    metadata_with_scope(journal, session, provider, None)
}

pub(super) fn session_metadata(
    journal: &Journal,
    session: &SessionRecord,
    provider: &str,
    domains: &BTreeSet<String>,
) -> ModuleResult<Value> {
    metadata_with_scope(journal, session, provider, Some(domains))
}

// A dependent can see a domain only through its exact settled control. Domain
// ownership remains with the producer and this metadata grants no effect rights.
fn settled_dependent_domain(
    journal: &Journal,
    session: &SessionRecord,
    admission_id: &str,
    admission: &crate::prepare_authority::Admission,
    domain: &str,
    owner: &str,
) -> bool {
    journal.control_settlements.records().any(|record| {
        let request = &record.settlement.request;
        let Some(permit) = journal.authority.control_permits.get(&request.permit_id) else {
            return false;
        };
        let policy = &permit.context.policy;
        let Some(selection) = journal.composition.selection(
            &request.target,
            &record.selection_transition_id,
            &admission.provider,
            owner,
            &request.proposal.output_id,
        ) else {
            return false;
        };
        request.admission_id == admission_id
            && session.admission_targets.get(admission_id) == Some(&request.target)
            && journal.admission_transitions.get(admission_id) == Some(&request.transition_id)
            && request.proposal.owner_provider == owner
            && request
                .proposal
                .mutation_domains
                .iter()
                .any(|item| item == domain)
            && admission.plan.control_proposals.contains(&request.proposal)
            && policy.dependent_provider == admission.provider
            && policy.owner_provider == owner
            && selection.validate_structure()
            && selection.control_policy == *policy
            && permit.request == *request
            && permit.consumed
            && permit.settled
            && record.owner_ack.is_some()
            && record.valid_under_permit(permit)
            && [
                &selection.control_approval,
                &selection.control_status_approval,
            ]
            .iter()
            .any(|approval| {
                approval.broker_approval == record.effect_dispatch.call.approval
                    && super::composition::approval_matches_call(
                        approval,
                        &record.effect_dispatch.call,
                    )
            })
            && record.effect_pending_proof.as_ref().is_none_or(|proof| {
                proof.call.approval == selection.control_approval.broker_approval
                    && super::composition::approval_matches_call(
                        &selection.control_approval,
                        &proof.call,
                    )
            })
            && record.owner_dispatch.as_ref().is_some_and(|proof| {
                proof.call.approval == selection.settlement_approval.broker_approval
                    && super::composition::approval_matches_call(
                        &selection.settlement_approval,
                        &proof.call,
                    )
            })
    })
}

fn metadata_with_scope(
    journal: &Journal,
    session: &SessionRecord,
    provider: &str,
    scoped_domains: Option<&BTreeSet<String>>,
) -> ModuleResult<Value> {
    let mut closed_by_session = BTreeMap::new();
    for candidate in journal.sessions.values() {
        let relevant = candidate.id == session.id
            || (scoped_domains.is_none()
                && candidate.admission_ids.iter().any(|id| {
                    journal
                        .authority
                        .admissions
                        .get(id)
                        .or_else(|| journal.authority.settled.get(id))
                        .is_some_and(|admission| admission.provider == provider)
                }));
        closed_by_session.insert(
            candidate.id.clone(),
            if relevant {
                closed_admissions(journal, candidate)?
            } else {
                BTreeSet::new()
            },
        );
    }
    let owned = |id: &String| {
        journal
            .authority
            .admissions
            .get(id)
            .or_else(|| journal.authority.settled.get(id))
            .filter(|admission| admission.provider == provider)
    };
    let mut selected_domains = BTreeSet::new();
    for id in &session.admission_ids {
        if !closed_by_session[&session.id].contains(id)
            && let Some(admission) = owned(id)
        {
            selected_domains.extend(
                admission
                    .plan
                    .mutation_domains
                    .iter()
                    .map(|domain| domain.domain.clone()),
            );
        }
    }
    if let Some(allowed) = scoped_domains {
        selected_domains.retain(|domain| allowed.contains(domain));
    }
    let mut admitted = Vec::new();
    let mut domains = BTreeMap::new();
    let mut target_values = BTreeSet::new();
    let mut obligations = Vec::new();
    for candidate_session in journal.sessions.values() {
        let is_origin = candidate_session.id == session.id;
        if scoped_domains.is_some() && !is_origin {
            continue;
        }
        let mut included = false;
        for id in &candidate_session.admission_ids {
            if closed_by_session[&candidate_session.id].contains(id) {
                continue;
            }
            let Some(admission) = owned(id) else {
                continue;
            };
            if (!is_origin || scoped_domains.is_some())
                && !admission
                    .plan
                    .mutation_domains
                    .iter()
                    .any(|domain| selected_domains.contains(&domain.domain))
            {
                continue;
            }
            let target = candidate_session
                .admission_targets
                .get(id)
                .ok_or("admission_target_missing")?;
            let phase = candidate_session
                .admission_phases
                .get(id)
                .ok_or("admission_phase_missing")?;
            let (_, target_session, _, _) = super::operations::target_parts(target)?;
            if target_session != candidate_session.id {
                return Err("admission_target_mismatch".into());
            }
            for domain in &admission.plan.mutation_domains {
                if !selected_domains.contains(&domain.domain) {
                    continue;
                }
                let owner = journal
                    .authority
                    .domain_owners
                    .get(&domain.domain)
                    .ok_or("domain_not_owned")?;
                if owner != provider
                    && !settled_dependent_domain(
                        journal,
                        candidate_session,
                        id,
                        admission,
                        &domain.domain,
                        owner,
                    )
                {
                    return Err("domain_not_owned".into());
                }
                let observed = journal
                    .authority
                    .snapshot
                    .domains
                    .get(&domain.domain)
                    .ok_or("domain_observation_missing")?;
                let entry = MutationDomainPlan {
                    domain: domain.domain.clone(),
                    expected_revision: observed.revision,
                    borrowers: observed.borrowers.clone(),
                    access: domain.access.clone(),
                };
                domains
                    .entry(domain.domain.clone())
                    .and_modify(|old: &mut MutationDomainPlan| {
                        if access_rank(&entry.access) > access_rank(&old.access) {
                            old.access = entry.access.clone();
                        }
                    })
                    .or_insert(entry);
            }
            target_values.insert(serde_json::to_string(target).map_err(|error| error.to_string())?);
            admitted.push(
                json!({"admission_id":id,"target":target,"phase":phase,"plan":admission.plan}),
            );
            included = true;
        }
        if included || (is_origin && scoped_domains.is_none()) {
            // Equal obligations still referenced by any live admission are
            // retained, including shared domains reused by a new generation.
            let closed_obligations: Vec<_> = closed_by_session[&candidate_session.id]
                .iter()
                .filter_map(&owned)
                .flat_map(|admission| admission.plan.obligations.iter())
                .collect();
            let live_obligations: Vec<_> = candidate_session
                .admission_ids
                .iter()
                .filter(|id| !closed_by_session[&candidate_session.id].contains(*id))
                .filter_map(&owned)
                .flat_map(|admission| admission.plan.obligations.iter())
                .collect();
            obligations.extend(
                candidate_session
                    .obligations
                    .iter()
                    .filter(|obligation| {
                        obligation.provider == provider
                            && obligation_remains_live(
                                obligation,
                                &closed_obligations,
                                &live_obligations,
                            )
                            && ((is_origin && scoped_domains.is_none())
                                || obligation
                                    .mutation_domain
                                    .as_ref()
                                    .is_some_and(|domain| selected_domains.contains(domain)))
                    })
                    .cloned(),
            );
        }
    }
    if scoped_domains.is_none() {
        for (domain, access) in reprepare_domains(journal, session, provider)? {
            let observed = journal
                .authority
                .snapshot
                .domains
                .get(&domain)
                .ok_or("domain_observation_missing")?;
            domains.entry(domain.clone()).or_insert(MutationDomainPlan {
                domain,
                expected_revision: observed.revision,
                borrowers: observed.borrowers.clone(),
                access,
            });
        }
    }
    let targets: Vec<Value> = target_values
        .into_iter()
        .map(|target| serde_json::from_str(&target).expect("serialized target is valid"))
        .collect();
    if admitted.len() > MAX_ITEMS
        || obligations.len() > MAX_ITEMS
        || domains.len() > MAX_ITEMS
        || targets.len() > MAX_ITEMS
    {
        return Err("provider_metadata_too_large".into());
    }
    let stale: Vec<_> = domains
        .keys()
        .filter(|domain| journal.authority.refresh_required.contains(*domain))
        .cloned()
        .collect();
    let value = json!({"admitted_plans":admitted,"obligations":obligations,"mutation_domains":domains.into_values().collect::<Vec<_>>(),"stale_mutation_domains":stale,"resource_targets":targets});
    if serde_json::to_vec(&value).map_err(|e| e.to_string())?.len() > MAX_RESPONSE_BYTES {
        return Err("provider_metadata_too_large".into());
    }
    Ok(value)
}

fn access_rank(access: &ResourceAccess) -> u8 {
    match access {
        ResourceAccess::Read => 0,
        ResourceAccess::Shared => 1,
        ResourceAccess::Exclusive => 2,
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DomainObservation {
    domain: String,
    revision: u64,
    borrowers: BTreeMap<String, u64>,
}

struct StagedAuthority(Option<crate::prepare_authority::AuthorityState>);
impl crate::prepare_authority::AuthorityJournal for StagedAuthority {
    fn persist(&mut self, state: &crate::prepare_authority::AuthorityState) -> ModuleResult<()> {
        self.0 = Some(state.clone());
        Ok(())
    }
}

impl Coordinator {
    /// Authenticate the provider and atomically replace only existing, unlocked
    /// observations covered by its own admitted plans for this session.
    pub(super) fn refresh_provider_observations(
        &mut self,
        caller: &str,
        initiator: &str,
        payload: Value,
        key: String,
    ) -> ModuleResult<Value> {
        if !self
            .settings
            .participants
            .iter()
            .any(|participant| participant.provider == caller)
        {
            return Err("provider_not_selected".into());
        }
        let target: LifecycleTarget =
            serde_json::from_value(payload.get("target").cloned().ok_or("target_required")?)
                .map_err(|_| "invalid_target")?;
        if serde_json::to_vec(&payload)
            .map_err(|_| "invalid_observation_batch")?
            .len()
            > MAX_RESPONSE_BYTES
        {
            return Err("observation_batch_too_large".into());
        }
        let (epoch, session_id, stream_id, generation) = super::operations::target_parts(&target)?;
        let mut next = self.shared.borrow().value.clone();
        let mut active_plans = Vec::new();
        for receipt in next
            .receipts
            .values()
            .filter(|receipt| receipt.response["status"] == "pending")
        {
            let Some(work) = &receipt.work else {
                continue;
            };
            if work.target != target {
                continue;
            }
            let normal = work.states.get(caller).filter(|state| {
                state.request.version == amaterasu_extension_api::COMPOSITION_VERSION
                    && state.request.operation
                        == amaterasu_extension_api::ProviderOperation::ResourcePlan
            });
            let finalize = work
                .composition_progress
                .as_ref()
                .and_then(|progress| progress.finalize.as_ref())
                .filter(|finalize| finalize.selection.policy.owner_provider == caller)
                .map(|finalize| &finalize.owner_plan);
            for state in normal.into_iter().chain(finalize) {
                if state.provider == caller
                    && state.request.target == work.target
                    && state.request.transition_id == work.transition_id
                    && matches!(
                        state.phase,
                        crate::prepare_driver::PreparePhase::Plan
                            | crate::prepare_driver::PreparePhase::PlanStatus
                    )
                {
                    active_plans.push((&state.request, &state.participant));
                }
            }
            for observation in work.owner_observations.values().filter(|observation| {
                observation.participant.provider == caller
                    && observation.phase == super::plan_observation::PlanObservationPhase::Ready
                    && observation.dispatch.as_ref().is_some_and(|dispatch| {
                        matches!(
                            dispatch.state,
                            crate::provider_dispatch::DispatchState::OutcomeUnknown
                        )
                    })
            }) {
                active_plans.push((&observation.request, &observation.participant));
            }
        }
        if active_plans.len() > 1 {
            return Err("ambiguous_provider_refresh".into());
        }
        let nested = active_plans.first().copied();
        match (nested, payload.get("provider_request")) {
            (Some((expected, participant)), Some(value)) => {
                let supplied: amaterasu_extension_api::ProviderRequest =
                    serde_json::from_value(value.clone())
                        .map_err(|_| "invalid_finalize_refresh")?;
                if &supplied != expected
                    || !matches!(
                        supplied.operation,
                        amaterasu_extension_api::ProviderOperation::ResourcePlan
                            | amaterasu_extension_api::ProviderOperation::ResourceFinalizePlan
                    )
                    || supplied.version != amaterasu_extension_api::COMPOSITION_VERSION
                    || self.selections.get(caller).is_none_or(|selected| {
                        !selected.approvals.iter().any(|approval| {
                            approval.method == participant.plan_method
                                && approval.broker_approval["provider"].as_str() == Some(caller)
                        })
                    })
                {
                    return Err("finalize_refresh_mismatch".into());
                }
                let verification = json!({"host_epoch":epoch,"provider_sequence":supplied.provider_sequence,"operation_id":supplied.operation_id,"transition_id":supplied.transition_id,"target":supplied.target,"fingerprint":super::security::fingerprint(&supplied)?});
                if self.verify_provider(caller, &verification)["status"] != "succeeded" {
                    return Err("finalize_refresh_not_authorized".into());
                }
            }
            (Some(_), None) => return Err("finalize_refresh_context_required".into()),
            (None, Some(_)) => return Err("unexpected_finalize_refresh_context".into()),
            (None, None) => {}
        }
        let session = next.sessions.get(session_id).ok_or("unknown_session")?;
        if epoch != next.host_epoch || epoch != session.host_epoch {
            return Err("stale_host_epoch".into());
        }
        if let Some(stream_id) = stream_id {
            if session
                .streams
                .get(stream_id)
                .is_none_or(|stream| stream.generation != generation)
            {
                return Err("stale_generation".into());
            }
        } else if session.generation != generation {
            return Err("stale_generation".into());
        }
        let mut allowed_domains = BTreeSet::new();
        let mut allowed_resources = BTreeSet::new();
        let closed = closed_admissions(&next, session)?;
        for id in &session.admission_ids {
            if closed.contains(id) {
                continue;
            }
            let Some(admission) = next
                .authority
                .admissions
                .get(id)
                .or_else(|| next.authority.settled.get(id))
            else {
                continue;
            };
            if admission.provider != caller {
                continue;
            }
            let original_target = session
                .admission_targets
                .get(id)
                .ok_or("admission_target_missing")?;
            if super::operations::target_parts(original_target)?.1 != session_id {
                return Err("admission_target_mismatch".into());
            }
            for domain in &admission.plan.mutation_domains {
                allowed_domains.insert(domain.domain.clone());
            }
            for resource in &admission.plan.resources {
                if resource.provider == caller {
                    allowed_resources.insert(resource.resource_id.clone());
                }
            }
        }
        // Nested provider-request verification above authenticates this exact
        // persisted replacement plan before the released-root exception applies.
        if nested.is_some() {
            allowed_domains.extend(reprepare_domains(&next, session, caller)?.into_keys());
        }
        if allowed_domains.is_empty() && allowed_resources.is_empty() {
            return Err("provider_session_not_owned".into());
        }
        let domains: Vec<DomainObservation> =
            serde_json::from_value(payload.get("domains").cloned().ok_or("domains_required")?)
                .map_err(|_| "invalid_domains")?;
        let resources: Vec<crate::planning::ResourceRef> = serde_json::from_value(
            payload
                .get("resources")
                .cloned()
                .unwrap_or_else(|| json!([])),
        )
        .map_err(|_| "invalid_resources")?;
        if domains.len() > MAX_ITEMS
            || resources.len() > MAX_ITEMS
            || (domains.is_empty() && resources.is_empty())
        {
            return Err("invalid_observation_batch".into());
        }
        if domains
            .iter()
            .any(|domain| !allowed_domains.contains(&domain.domain))
            || resources.iter().any(|resource| {
                resource.provider != caller || !allowed_resources.contains(&resource.resource_id)
            })
        {
            return Err("observation_not_owned_by_session".into());
        }
        let observations: Vec<_> = domains
            .into_iter()
            .map(|domain| {
                (
                    domain.domain,
                    crate::planning::DomainState {
                        revision: domain.revision,
                        borrowers: domain.borrowers,
                    },
                )
            })
            .collect();
        let selections: Vec<_> = self.selections.values().cloned().collect();
        let mut authority = crate::prepare_authority::CoordinatorPrepareAuthority::new(
            next.authority.clone(),
            selections,
            StagedAuthority(None),
        )?;
        authority.refresh_observations(caller, &observations, &resources)?;
        next.authority = authority.state().clone();
        let refreshed = metadata(
            &next,
            next.sessions.get(session_id).ok_or("unknown_session")?,
            caller,
        )?;
        let mut response =
            json!({"status":"succeeded","operation_id":payload["operation_id"],"target":target});
        response
            .as_object_mut()
            .unwrap()
            .extend(refreshed.as_object().ok_or("invalid_metadata")?.clone());
        if serde_json::to_vec(&response)
            .map_err(|e| e.to_string())?
            .len()
            > MAX_RESPONSE_BYTES
        {
            return Err("provider_metadata_too_large".into());
        }
        next.receipts.insert(
            key.clone(),
            Receipt {
                caller: caller.into(),
                initiator: initiator.into(),
                request: payload,
                response: response.clone(),
                work: None,
                wait_target: None,
            },
        );
        super::refresh_receipt::compact(&mut next, &key);
        save(&self.shared, next)?;
        Ok(response)
    }
}

#[cfg(test)]
#[path = "provider_observations_tests.rs"]
mod tests;
