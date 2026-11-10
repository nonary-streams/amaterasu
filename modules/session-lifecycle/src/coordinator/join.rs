//! A shared source retains its original provider lease while independent
//! transport consumers join and retire. Provider effects remain admitted calls.
use super::*;
use amaterasu_extension_api::{
    COMPOSITION_VERSION, ProviderResponse, STREAM_JOIN_CAPABILITY, StreamJoinContext,
};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SharedSourceLease {
    context: StreamJoinContext,
    subscribers: BTreeMap<String, u64>,
    #[serde(default)]
    callbacks: BTreeMap<String, JoinCallbackEvidence>,
    #[serde(default)]
    retired_subscribers: BTreeSet<String>,
    #[serde(default)]
    closed: BTreeMap<String, ClosedJoinEvidence>,
    deferred_release: Option<Work>,
    retirement: Option<SourceRetirementProof>,
    release_queued: bool,
    released: bool,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceRetirementProof {
    target: LifecycleTarget,
    required: BTreeSet<String>,
    retirements: BTreeSet<String>,
    receipt: String,
}

/// Closed Join calls retain their exact SDK plan, broker approval and terminal
/// callback as historical evidence. They never carry source resource authority.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct JoinCallbackEvidence {
    admission: crate::prepare_authority::Admission,
    callback: composition::ProducerCallbackProof,
}

impl JoinCallbackEvidence {
    fn valid(&self, id: &str, context: &StreamJoinContext) -> bool {
        let request = &self.callback.request;
        let provider = &self.admission.provider;
        self.admission.rights.is_empty()
            && self.admission.selection.participant == self.callback.participant
            && request.join_context.as_deref() == Some(context)
            && request.admission_id.as_deref() == Some(id)
            && request.approved_plan.as_ref() == Some(&self.admission.plan)
            && amaterasu_extension_api::stream_join_plan_context(&self.admission.plan)
                .ok()
                .flatten()
                .as_ref()
                == Some(context)
            && composition::callback_approvals_valid(
                &self.callback,
                &self.admission.selection.approvals,
                provider,
            )
            && amaterasu_extension_api::stream_join_callback(request, provider)
                .is_ok_and(|response| composition::valid_callback_proof(&self.callback, &response))
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClosedJoinEvidence {
    cleanup_providers: Vec<String>,
    failed: Option<FailedPreparation>,
}

fn rejected_plan_valid(
    state: &PrepareState,
    context: &StreamJoinContext,
    target: &LifecycleTarget,
) -> bool {
    let Some(terminal) = &state.terminal_dispatch else {
        return false;
    };
    let Some(response @ ProviderResponse::Rejected { .. }) = &state.response else {
        return false;
    };
    let crate::provider_dispatch::DispatchState::Completed { payload } = &terminal.state else {
        return false;
    };
    let request = &state.request;
    let method = &state.participant.plan_method;
    state.phase == crate::prepare_driver::PreparePhase::Rejected
        && state.admission_id.is_none()
        && state.pending_receipt.is_none()
        && state.pending_dispatch.is_none()
        && request.operation == amaterasu_extension_api::ProviderOperation::ResourcePlan
        && request.phase == LifecyclePhase::StreamPrepare
        && request.join_context.as_deref() == Some(context)
        && request.target == *target
        && request.admission_id.is_none()
        && request.approved_plan.is_none()
        && terminal.call.operation_id == request.operation_id
        && terminal.call.service == method.service
        && terminal.call.method == method.method
        && terminal.call.interface_major == method.interface_major
        && terminal.call.interface_minor == method.interface_minor
        && serde_json::to_value(request).ok().as_ref() == Some(&terminal.call.arguments)
        && serde_json::to_value(response).ok().as_ref() == Some(payload)
        && amaterasu_extension_api::validate_provider_request(request).is_ok()
        && amaterasu_extension_api::validate_provider_response(request, response).is_ok()
        && composition::approval_is_valid(
            &crate::prepare_authority::MethodApproval {
                method: method.clone(),
                broker_approval: terminal.call.approval.clone(),
            },
            &state.provider,
        )
}

/// Providers a failed join prepared, in preparation order, when its failure
/// evidence is exact: every attempted provider but the last prepared, and the
/// last rejected its read-only plan. A join refused before any provider was
/// attempted, e.g. by processing preflight, prepared nothing. `None` means the
/// evidence cannot prove the join is closed.
fn failed_join_cleanup(
    failed: &FailedPreparation,
    order: &[String],
    lease: &SharedSourceLease,
    target: &LifecycleTarget,
) -> Option<Vec<String>> {
    if failed.uncertain || failed.states.len() > order.len() {
        return None;
    }
    if failed.states.is_empty() {
        return Some(Vec::new());
    }
    let attempted = &order[..failed.states.len()];
    if failed
        .states
        .keys()
        .any(|provider| !attempted.contains(provider))
    {
        return None;
    }
    for (index, provider) in attempted.iter().enumerate() {
        let state = failed.states.get(provider)?;
        if state.provider != *provider || state.participant.provider != *provider {
            return None;
        }
        if index + 1 == attempted.len() {
            if !rejected_plan_valid(state, &lease.context, target) {
                return None;
            }
        } else if state.phase != crate::prepare_driver::PreparePhase::Prepared
            || state
                .admission_id
                .as_ref()
                .and_then(|id| lease.callbacks.get(id))
                .is_none_or(|proof| {
                    proof.callback.request.target != *target
                        || proof.callback.request.phase != LifecyclePhase::StreamPrepare
                        || proof.callback.request != state.request
                })
        {
            return None;
        }
    }
    Some(attempted[..attempted.len() - 1].to_vec())
}

fn closed_join_valid(
    lease: &SharedSourceLease,
    stream_id: &str,
    stream: &StreamRecord,
    target: &LifecycleTarget,
) -> bool {
    let Some(closed) = lease.closed.get(stream_id) else {
        return false;
    };
    let Some(graph) = &stream.provider_graph else {
        return false;
    };
    let mut expected = match &closed.failed {
        Some(failed) => match failed_join_cleanup(failed, &graph.order, lease, target) {
            Some(prepared) => prepared,
            None => return false,
        },
        None => graph.order.clone(),
    };
    expected.reverse();
    if expected != closed.cleanup_providers {
        return false;
    }
    expected.iter().all(|provider| {
        let mut callbacks = lease.callbacks.iter().filter(|(_, proof)| {
            proof.callback.request.target == *target
                && proof.admission.provider == *provider
                && proof.callback.request.phase == LifecyclePhase::StreamRelease
        });
        let Some((id, proof)) = callbacks.next() else {
            return false;
        };
        callbacks.next().is_none() && proof.valid(id, &lease.context)
    })
}

// Older journals can contain rejected joins recorded before callback evidence
// was retained. Keep such closed history readable only after the entire source
// was released; it is never accepted as proof by `closed_join_valid`.
fn legacy_closed_join_unproven(lease: &SharedSourceLease, stream_id: &str) -> bool {
    lease.released
        && lease.subscribers.is_empty()
        && lease.deferred_release.is_none()
        && !lease.release_queued
        && lease.callbacks.is_empty()
        && lease.closed.get(stream_id).is_some_and(|closed| {
            closed.cleanup_providers.is_empty()
                && closed.failed.as_ref().is_some_and(|failed| {
                    failed.reason == "processing_output_range_mismatch"
                        && !failed.uncertain
                        && failed.states.is_empty()
                })
        })
}

pub(super) fn record_callback(journal: &mut Journal, state: &PrepareState) -> ModuleResult<()> {
    let context = state
        .request
        .join_context
        .as_deref()
        .ok_or("join_context_missing")?;
    let (_, _, Some(source), _) = operations::target_parts(&context.source)? else {
        return Err("invalid_join_source".into());
    };
    let id = state.admission_id.as_ref().ok_or("admission_missing")?;
    let evidence = JoinCallbackEvidence {
        admission: journal
            .authority
            .settled
            .get(id)
            .cloned()
            .ok_or("join_admission_missing")?,
        callback: composition::ProducerCallbackProof {
            participant: state.participant.clone(),
            request: state.request.clone(),
            terminal: state
                .terminal_dispatch
                .clone()
                .ok_or("join_terminal_missing")?,
            pending_receipt: state.pending_receipt.clone(),
            pending_dispatch: state.pending_dispatch.clone(),
        },
    };
    if !evidence.valid(id, context) {
        return Err("join_terminal_invalid".into());
    }
    let lease = journal
        .shared_sources
        .get_mut(source)
        .ok_or("shared_source_lease_missing")?;
    if lease.context != *context || lease.callbacks.len() >= 4096 {
        return Err("shared_source_evidence_capacity".into());
    }
    if let Some(old) = lease.callbacks.get(id) {
        if serde_json::to_value(old).ok() != serde_json::to_value(&evidence).ok() {
            return Err("join_terminal_conflict".into());
        }
    } else {
        lease.callbacks.insert(id.clone(), evidence);
    }
    Ok(())
}

fn retire_admissions(journal: &mut Journal, work: &Work, source: &str) -> ModuleResult<()> {
    let (_, session_id, Some(stream_id), _) = operations::target_parts(&work.target)? else {
        return Err("invalid_join_target".into());
    };
    let lease = journal
        .shared_sources
        .get(source)
        .ok_or("shared_source_lease_missing")?;
    let session = &journal.sessions[session_id];
    let stream = &session.streams[stream_id];
    if stream.required != stream.retirements || stream.phase != StreamPhase::Retired {
        return Err("join_consumer_retirement_unverified".into());
    }
    let ids: Vec<_> = session
        .admission_targets
        .iter()
        .filter(|(_, target)| **target == work.target)
        .map(|(id, _)| id.clone())
        .collect();
    for id in &ids {
        let evidence = lease
            .callbacks
            .get(id)
            .ok_or("join_retirement_callback_missing")?;
        if !evidence.valid(id, &lease.context)
            || journal.authority.settled.get(id) != Some(&evidence.admission)
            || journal.references_admission(id)
            || journal.authority.output_admissions.contains(id)
        {
            return Err("join_retirement_admission_unverified".into());
        }
    }
    for provider in &work.providers {
        if !ids.iter().any(|id| {
            lease.callbacks.get(id).is_some_and(|evidence| {
                evidence.admission.provider == *provider
                    && evidence.callback.request.phase == LifecyclePhase::StreamRelease
                    && evidence.callback.request.transition_id == work.transition_id
            })
        }) {
            return Err("join_release_callback_missing".into());
        }
    }
    let closed = ClosedJoinEvidence {
        cleanup_providers: work.providers.clone(),
        failed: work.failed_preparation.clone(),
    };
    let mut candidate = lease.clone();
    candidate.closed.insert(stream_id.into(), closed.clone());
    if !closed_join_valid(&candidate, stream_id, stream, &work.target) {
        return Err("join_closed_evidence_unverified".into());
    }
    let session = journal
        .sessions
        .get_mut(session_id)
        .ok_or("unknown_session")?;
    for id in ids {
        journal.authority.settled.remove(&id);
        journal.admission_transitions.remove(&id);
        session.admission_ids.remove(&id);
        session.admission_targets.remove(&id);
        session.admission_phases.remove(&id);
        session.prepared_plan_ids.remove(&id);
    }
    let lease = journal
        .shared_sources
        .get_mut(source)
        .ok_or("shared_source_lease_missing")?;
    lease.retired_subscribers.insert(stream_id.into());
    lease.closed.insert(stream_id.into(), closed);
    Ok(())
}

/// Native actions and reconfiguration cannot treat an original transport's
/// consumer retirement as release of the source borrowed by another transport.
pub(super) fn session_has_shared_lease(journal: &Journal, session_id: &str) -> bool {
    journal.shared_sources.values().any(|lease| {
        !lease.released
            && operations::target_parts(&lease.context.source)
                .ok()
                .is_some_and(|(_, session, _, _)| session == session_id)
    })
}

pub(super) fn freeze_source(
    settings: &LifecycleSettings,
    journal: &Journal,
    session: &SessionRecord,
) -> ModuleResult<StreamJoinContext> {
    if session.phase != SessionPhase::Ready {
        return Err("join_source_unavailable".into());
    }
    if settings.participants.is_empty()
        || settings.participants.iter().any(|participant| {
            (session.capture_binding_mode.is_managed()
                && participant.contract_version != COMPOSITION_VERSION)
                || !participant.capabilities.contains(STREAM_JOIN_CAPABILITY)
        })
    {
        return Err("provider_join_unsupported".into());
    }
    let mut candidates = session
        .streams
        .iter()
        .filter(|(_, stream)| matches!(stream.phase, StreamPhase::Ready | StreamPhase::Active));
    let (stream_id, stream) = candidates.next().ok_or("join_source_unavailable")?;
    let binding = stream.candidate.as_ref();
    if let Some(binding) = binding {
        amaterasu_extension_api::validate_capture_binding(binding)?;
    }
    if (session.capture_binding_mode.is_managed()
        && (stream.binding_owner.is_empty() || binding.is_none()))
        || (session.capture_binding_mode.is_external()
            && (!stream.binding_owner.is_empty() || binding.is_some()))
        || binding.is_some_and(|binding| binding.generation != stream.generation)
        || candidates.any(|(_, other)| match (other.candidate.as_ref(), binding) {
            (Some(other), Some(binding)) => {
                other.resource != binding.resource
                    || other.mode != binding.mode
                    || other.geometry_revision != binding.geometry_revision
            }
            (None, None) => false,
            _ => true,
        })
    {
        return Err("join_source_ambiguous".into());
    }
    let context = stream
        .join_context
        .clone()
        .unwrap_or_else(|| StreamJoinContext {
            source: LifecycleTarget::Stream {
                host_epoch: session.host_epoch.clone(),
                session_id: session.id.clone(),
                stream_id: stream_id.clone(),
                generation: stream.generation,
            },
            source_transition: stream.preparation_transition.clone(),
            capture_binding_mode: session.capture_binding_mode,
            binding_owner: session
                .capture_binding_mode
                .is_managed()
                .then(|| stream.binding_owner.clone()),
            binding: binding.cloned(),
        });
    source_output(journal, &context)?;
    Ok(context)
}

/// Select only the completed, coordinator-retained owner finalization. This
/// never turns a client-selected output or an arbitrary native handle into a lease.
pub(super) fn source_output(
    journal: &Journal,
    context: &StreamJoinContext,
) -> ModuleResult<Option<amaterasu_extension_api::ResourceOutput>> {
    let (epoch, session_id, Some(stream_id), generation) =
        operations::target_parts(&context.source)?
    else {
        return Err("invalid_join_source".into());
    };
    let session = journal
        .sessions
        .get(session_id)
        .ok_or("join_source_unavailable")?;
    let stream = session
        .streams
        .get(stream_id)
        .ok_or("join_source_unavailable")?;
    if epoch != journal.host_epoch
        || epoch != session.host_epoch
        || generation != stream.generation
        || stream.binding_owner != context.binding_owner.as_deref().unwrap_or("")
        || stream.preparation_transition != context.source_transition
        || stream.candidate.as_ref() != context.binding.as_ref()
        || session.capture_binding_mode != context.capture_binding_mode
        || !matches!(
            stream.phase,
            StreamPhase::Ready | StreamPhase::Active | StreamPhase::Quiescing
        )
        || (stream.phase == StreamPhase::Quiescing
            && !journal.shared_sources.get(stream_id).is_some_and(|lease| {
                lease.context == *context && !lease.released && !lease.release_queued
            }))
    {
        return Err("join_source_unavailable".into());
    }
    if context.capture_binding_mode.is_external() {
        if context.binding.is_some()
            || context.binding_owner.is_some()
            || !journal.receipts.values().any(|receipt| {
                receipt.response["target"] == json!(context.source)
                    && receipt.response["transition_id"] == context.source_transition
                    && receipt.response["status"] == "succeeded"
                    && receipt.response["capture_binding_mode"] == "external"
            })
        {
            return Err("external_join_source_unverified".into());
        }
        return Ok(None);
    }
    let binding = context
        .binding
        .as_ref()
        .ok_or("join_source_binding_missing")?;
    let mut outputs = journal.finalize_results.records().filter_map(|result| {
        if result.request.target != context.source
            || result.request.transition_id != context.source_transition
            || context.binding_owner.as_deref()
                != Some(result.selection.policy.owner_provider.as_str())
        {
            return None;
        }
        match &result.applied {
            amaterasu_extension_api::ResourceControlResponse::Applied {
                output,
                candidate: Some(candidate),
            } if candidate.as_ref() == binding => Some(output.as_ref()),
            _ => None,
        }
    });
    let output = outputs.next().ok_or("join_source_proof_missing")?;
    if outputs.next().is_some() {
        return Err("join_source_proof_ambiguous".into());
    }
    // Finalization intentionally marks domain observations for a later native
    // refresh. A Join grants no native mutation rights; its consumer must pass
    // the original owner's fresh binding_validate method before activation.
    // The retained finalization is identity/provenance, not fresh native state.
    Ok(Some(output.clone()))
}

/// Old epochs grant cleanup only after the selected consumer has proved native
/// retirement. This cannot prepare, join, validate capture or mutate the source.
pub(super) fn cleanup_source_output(
    journal: &Journal,
    work: &Work,
) -> ModuleResult<Option<amaterasu_extension_api::ResourceOutput>> {
    let context = work.join_context.as_ref().ok_or("join_context_missing")?;
    if work.recovery_authorized_epoch.as_deref() != Some(journal.host_epoch.as_str()) {
        return source_output(journal, context);
    }
    let (epoch, session_id, Some(stream_id), generation) = operations::target_parts(&work.target)?
    else {
        return Err("invalid_join_target".into());
    };
    let (_, source_session, Some(source_id), source_generation) =
        operations::target_parts(&context.source)?
    else {
        return Err("invalid_join_source".into());
    };
    amaterasu_extension_api::validate_stream_join_context(context, &work.target)?;
    let session = journal.sessions.get(session_id).ok_or("unknown_session")?;
    let stream = session.streams.get(stream_id).ok_or("unknown_stream")?;
    let source = session
        .streams
        .get(source_id)
        .ok_or("join_source_unavailable")?;
    let lease = journal
        .shared_sources
        .get(source_id)
        .ok_or("shared_source_lease_missing")?;
    if epoch == journal.host_epoch
        || epoch != session.host_epoch
        || source_session != session_id
        || work.phase != LifecyclePhase::StreamRelease
        || !context.capture_binding_mode.is_external()
        || !session.capture_binding_mode.is_external()
        || stream.generation != generation
        || stream.join_context.as_ref() != Some(context)
        || stream.phase != StreamPhase::Quiescing
        || stream.required != stream.retirements
        || source.generation != source_generation
        || source.preparation_transition != context.source_transition
        || !source.binding_owner.is_empty()
        || source.candidate.is_some()
        || !matches!(
            source.phase,
            StreamPhase::RecoveryRequired | StreamPhase::Quiescing
        )
        || lease.context != *context
        || lease.released
        || lease.release_queued
        || lease.subscribers.get(stream_id) != Some(&generation)
    {
        return Err("shared_source_recovery_unverified".into());
    }
    Ok(None)
}

pub(super) fn subscribe(
    journal: &mut Journal,
    context: &StreamJoinContext,
    target: &LifecycleTarget,
) -> ModuleResult<()> {
    let (_, _, Some(source), source_generation) = operations::target_parts(&context.source)? else {
        return Err("invalid_join_source".into());
    };
    let (_, _, Some(subscriber), generation) = operations::target_parts(target)? else {
        return Err("invalid_join_target".into());
    };
    let lease = journal
        .shared_sources
        .entry(source.to_owned())
        .or_insert_with(|| SharedSourceLease {
            context: context.clone(),
            subscribers: [(source.to_owned(), source_generation)].into(),
            callbacks: BTreeMap::new(),
            retired_subscribers: BTreeSet::new(),
            closed: BTreeMap::new(),
            deferred_release: None,
            retirement: None,
            release_queued: false,
            released: false,
        });
    // Reserve the closed-consumer slot before any provider admission. A failed
    // or retired Join still needs durable no-effect callback evidence, so rapid
    // replacement cannot spend the capacity needed to retire existing clients.
    if lease
        .subscribers
        .len()
        .saturating_add(lease.retired_subscribers.len())
        .saturating_add(usize::from(lease.retirement.is_some()))
        >= 64
    {
        return Err("shared_source_subscription_capacity".into());
    }
    if lease.context != *context
        || lease.released
        || lease.release_queued
        || lease.subscribers.contains_key(subscriber)
    {
        return Err("join_source_changed".into());
    }
    lease.subscribers.insert(subscriber.to_owned(), generation);
    Ok(())
}

pub(super) fn validation_source(
    journal: &Journal,
    target: &LifecycleTarget,
    binding: &CaptureBinding,
    provider: &str,
) -> ModuleResult<(LifecycleTarget, CaptureBinding, String)> {
    let (_, session_id, Some(stream_id), generation) = operations::target_parts(target)? else {
        return Err("invalid_join_target".into());
    };
    let stream = journal
        .sessions
        .get(session_id)
        .and_then(|session| session.streams.get(stream_id))
        .ok_or("unknown_stream")?;
    if stream.generation != generation
        || stream.candidate.as_ref() != Some(binding)
        || stream.binding_owner != provider
    {
        return Err("resource_binding_stale".into());
    }
    let Some(context) = &stream.join_context else {
        return Ok((
            target.clone(),
            binding.clone(),
            stream.preparation_transition.clone(),
        ));
    };
    source_output(journal, context)?;
    let (_, _, Some(source), _) = operations::target_parts(&context.source)? else {
        return Err("invalid_join_source".into());
    };
    let lease = journal
        .shared_sources
        .get(source)
        .ok_or("shared_source_lease_missing")?;
    let mut expected = context
        .binding
        .clone()
        .ok_or("managed_join_binding_missing")?;
    expected.generation = generation;
    if lease.context != *context
        || lease.released
        || lease.release_queued
        || lease.subscribers.get(stream_id) != Some(&generation)
        || binding != &expected
    {
        return Err("shared_source_subscription_changed".into());
    }
    Ok((
        context.source.clone(),
        context
            .binding
            .clone()
            .ok_or("managed_join_binding_missing")?,
        context.source_transition.clone(),
    ))
}

pub(super) fn source_release_deferred(journal: &Journal, target: &LifecycleTarget) -> bool {
    operations::target_parts(target)
        .ok()
        .and_then(|(_, _, stream, _)| stream)
        .and_then(|stream| journal.shared_sources.get(stream))
        .is_some_and(|lease| {
            lease.context.source == *target && lease.deferred_release.is_some() && !lease.released
        })
}

pub(super) fn source_resources_retained(journal: &Journal, target: &LifecycleTarget) -> bool {
    operations::target_parts(target)
        .ok()
        .and_then(|(_, _, stream, _)| stream)
        .and_then(|stream| journal.shared_sources.get(stream))
        .is_some_and(|lease| lease.context.source == *target && !lease.released)
}

pub(super) fn retirement_proof(
    journal: &Journal,
    target: &LifecycleTarget,
) -> Option<amaterasu_extension_api::StreamRetirementProof> {
    let (_, session, Some(stream), generation) = operations::target_parts(target).ok()? else {
        return None;
    };
    let stream_record = journal.sessions.get(session)?.streams.get(stream)?;
    if stream_record.generation != generation {
        return None;
    }
    let lease = if let Some(context) = &stream_record.join_context {
        let (_, _, Some(source), _) = operations::target_parts(&context.source).ok()? else {
            return None;
        };
        if stream_record.phase != StreamPhase::Retired {
            return None;
        }
        journal.shared_sources.get(source)?
    } else {
        let lease = journal.shared_sources.get(stream)?;
        if stream_record.phase != StreamPhase::Retired && lease.retirement.is_none() {
            return None;
        }
        lease
    };
    if lease.release_queued && !lease.released {
        return None;
    }
    Some(amaterasu_extension_api::StreamRetirementProof {
        target: target.clone(),
        consumer_retired: true,
        resources_released: lease.released,
        shared_retained: !lease.released,
        shared_source: (!lease.released).then(|| lease.context.source.clone()),
    })
}

/// A no-native child has no separate finalized output. Its closed lineage is
/// the admitted SDK callback transcript plus the original source's real release.
/// This proof cannot substitute for a native owner's own released output.
pub(super) fn subscriber_resources_released(journal: &Journal, target: &LifecycleTarget) -> bool {
    let Ok((_, session_id, Some(stream_id), generation)) = operations::target_parts(target) else {
        return false;
    };
    let Some(session) = journal.sessions.get(session_id) else {
        return false;
    };
    let Some(stream) = session.streams.get(stream_id) else {
        return false;
    };
    let Some(context) = &stream.join_context else {
        return false;
    };
    let Ok((_, _, Some(source), _)) = operations::target_parts(&context.source) else {
        return false;
    };
    let Some(lease) = journal.shared_sources.get(source) else {
        return false;
    };
    if stream.generation != generation
        || stream.phase != StreamPhase::Retired
        || stream.required != stream.retirements
        || lease.context != *context
        || !lease.released
        || !lease.subscribers.is_empty()
        || !lease.retired_subscribers.contains(stream_id)
        || journal.verified_released_output(&context.source).is_none()
        || session
            .admission_targets
            .values()
            .any(|admitted| admitted == target)
    {
        return false;
    }
    closed_join_valid(lease, stream_id, stream, target)
}

pub(super) fn awaiting_shared_release(journal: &Journal, target: &LifecycleTarget) -> bool {
    let Ok((_, session, Some(stream), _)) = operations::target_parts(target) else {
        return false;
    };
    if journal.shared_sources.get(stream).is_some_and(|lease| {
        lease.context.source == *target && lease.release_queued && !lease.released
    }) {
        return true;
    }
    let Some(context) = journal
        .sessions
        .get(session)
        .and_then(|session| session.streams.get(stream))
        .and_then(|stream| stream.join_context.as_ref())
    else {
        return false;
    };
    let Ok((_, _, Some(source), _)) = operations::target_parts(&context.source) else {
        return false;
    };
    journal
        .shared_sources
        .get(source)
        .is_some_and(|lease| lease.release_queued && !lease.released)
}

pub(super) fn validate(journal: &Journal) -> bool {
    journal.shared_sources.len() <= MAX_SESSIONS * 64
        && journal.shared_sources.iter().all(|(key, lease)| {
            let Ok((epoch, session_id, Some(source_id), generation)) =
                operations::target_parts(&lease.context.source)
            else {
                return false;
            };
            if key != source_id
                || lease.subscribers.len() > 64
                || lease.callbacks.len() > 4096
                || lease.retired_subscribers.len() > 64
                || lease.closed.len() != lease.retired_subscribers.len()
                || lease
                    .callbacks
                    .iter()
                    .any(|(id, proof)| !proof.valid(id, &lease.context))
                || lease.context.binding.as_ref().is_some_and(|binding| {
                    amaterasu_extension_api::validate_capture_binding(binding).is_err()
                        || binding.generation != generation
                })
            {
                return false;
            }
            let Some(session) = journal.sessions.get(session_id) else {
                return lease.released
                    && lease.subscribers.is_empty()
                    && lease.deferred_release.is_none();
            };
            let Some(source) = session.streams.get(source_id) else {
                return lease.released
                    && lease.subscribers.is_empty()
                    && lease.deferred_release.is_none();
            };
            if session.host_epoch != epoch
                || source.generation != generation
                || source.join_context.is_some()
                || source.binding_owner != lease.context.binding_owner.as_deref().unwrap_or("")
                || session.capture_binding_mode != lease.context.capture_binding_mode
                || source.preparation_transition != lease.context.source_transition
                || source.candidate.as_ref() != lease.context.binding.as_ref()
                || lease.subscribers.iter().any(|(id, generation)| {
                    session.streams.get(id).is_none_or(|stream| {
                        stream.generation != *generation
                            || stream.phase == StreamPhase::Retired
                            || (id != source_id
                                && stream.join_context.as_ref() != Some(&lease.context))
                    })
                })
            {
                return false;
            }
            if lease.retired_subscribers.iter().any(|id| {
                session.streams.get(id).is_none_or(|stream| {
                    stream.phase != StreamPhase::Retired
                        || stream.required != stream.retirements
                        || stream.join_context.as_ref() != Some(&lease.context)
                        || lease.subscribers.contains_key(id)
                        || !(legacy_closed_join_unproven(lease, id)
                            || closed_join_valid(
                                lease,
                                id,
                                stream,
                                &LifecycleTarget::Stream {
                                    host_epoch: epoch.into(),
                                    session_id: session_id.into(),
                                    stream_id: id.clone(),
                                    generation: stream.generation,
                                },
                            ))
                })
            }) || lease.callbacks.iter().any(|(id, proof)| {
                let request = &proof.callback.request;
                let Ok((request_epoch, request_session, Some(subscriber), request_generation)) =
                    operations::target_parts(&request.target)
                else {
                    return true;
                };
                request_epoch != epoch
                    || request_session != session_id
                    || subscriber == source_id
                    || session.streams.get(subscriber).is_none_or(|stream| {
                        stream.generation != request_generation
                            || stream.join_context.as_ref() != Some(&lease.context)
                    })
                    || journal
                        .authority
                        .sequences
                        .get(&proof.admission.provider)
                        .is_none_or(|sequence| *sequence < request.provider_sequence)
                    || if lease.retired_subscribers.contains(subscriber) {
                        journal.authority.settled.contains_key(id)
                            || journal.authority.admissions.contains_key(id)
                            || session.admission_ids.contains(id)
                    } else {
                        journal.authority.settled.get(id) != Some(&proof.admission)
                    }
            }) {
                return false;
            }
            if lease.released {
                return source.phase == StreamPhase::Retired
                    && lease.subscribers.is_empty()
                    && lease.deferred_release.is_none()
                    && !lease.release_queued;
            }
            if let Some(proof) = &lease.retirement
                && (proof.target != lease.context.source
                    || proof.required != source.required
                    || proof.retirements != source.retirements
                    || proof.required != proof.retirements
                    || journal.receipts.get(&proof.receipt).is_none_or(|receipt| {
                        amaterasu_extension_api::stream_retirement_proof(
                            &receipt.response,
                            &lease.context.source,
                            None,
                        )
                        .is_err()
                    }))
            {
                return false;
            }
            if let Some(work) = &lease.deferred_release
                && (lease.release_queued
                    || lease.retirement.is_none()
                    || work.target != lease.context.source
                    || work.phase != LifecyclePhase::StreamRelease
                    || work.join_context.is_some()
                    || work.index != 0
                    || !work.states.is_empty())
            {
                return false;
            }
            if lease.release_queued {
                lease.subscribers.is_empty()
                    && lease.deferred_release.is_none()
                    && lease.retirement.is_some()
                    && journal
                        .receipts
                        .values()
                        .filter(|receipt| {
                            receipt.work.as_ref().is_some_and(|work| {
                                work.target == lease.context.source
                                    && work.phase == LifecyclePhase::StreamRelease
                            })
                        })
                        .count()
                        == 1
            } else {
                !lease.subscribers.is_empty()
            }
        })
}

/// Reclaim only independently proved, no-native Join scopes. Normal source
/// ownership and its native release history stay pinned. All original operation
/// IDs and stream scopes become durable replay fences in the same transaction.
pub(super) fn compact_retired_subscribers(journal: &mut Journal, target: &LifecycleTarget) {
    let Ok((_, session_id, None, _)) = operations::target_parts(target) else {
        return;
    };
    let candidates: Vec<_> = journal
        .shared_sources
        .iter()
        .flat_map(|(source, lease)| {
            let belongs = operations::target_parts(&lease.context.source)
                .ok()
                .is_some_and(|(_, session, _, _)| session == session_id);
            lease
                .retired_subscribers
                .iter()
                .filter(move |_| belongs)
                .map(move |subscriber| (source.clone(), subscriber.clone()))
        })
        .collect();
    // Keep recent full readback available; compact before the admission budget
    // is tight, and never sacrifice unresolved cleanup or replay protection.
    if candidates.len() < 16 {
        return;
    }
    for (source, subscriber) in candidates.into_iter().take(8) {
        let mut next = journal.clone();
        if compact_subscriber(&mut next, session_id, &source, &subscriber).is_ok()
            && validate_journal(&next).is_ok()
        {
            *journal = next;
        }
    }
}

fn compact_subscriber(
    journal: &mut Journal,
    session_id: &str,
    source: &str,
    subscriber: &str,
) -> ModuleResult<()> {
    let session = journal.sessions.get(session_id).ok_or("unknown_session")?;
    let stream = session.streams.get(subscriber).ok_or("unknown_stream")?;
    let target = LifecycleTarget::Stream {
        host_epoch: session.host_epoch.clone(),
        session_id: session_id.into(),
        stream_id: subscriber.into(),
        generation: stream.generation,
    };
    let lease = journal
        .shared_sources
        .get(source)
        .ok_or("shared_source_lease_missing")?;
    if !lease.retired_subscribers.contains(subscriber)
        || lease.subscribers.contains_key(subscriber)
        || stream.phase != StreamPhase::Retired
        || stream.required != stream.retirements
        || stream.join_context.as_ref() != Some(&lease.context)
        || session
            .admission_targets
            .values()
            .any(|admission| admission == &target)
        || journal
            .input_bindings
            .values()
            .any(|binding| binding.session_id == session_id && binding.stream_id == subscriber)
        || journal
            .inputs
            .has_unresolved_stream(&session.host_epoch, session_id, subscriber)
        || journal
            .actions
            .has_unresolved_stream(&session.host_epoch, session_id, subscriber)
        || journal.receipts.values().any(|receipt| {
            receipt
                .work
                .as_ref()
                .is_some_and(|work| work.target == target)
                || receipt.wait_target.as_ref() == Some(&target)
        })
    {
        return Err("joined_scope_still_referenced".into());
    }
    let proofs: Vec<_> = lease
        .callbacks
        .iter()
        .filter(|(_, proof)| proof.callback.request.target == target)
        .collect();
    if proofs.is_empty()
        || proofs.iter().any(|(id, proof)| {
            !proof.valid(id, &lease.context)
                || journal.authority.admissions.contains_key(*id)
                || journal.authority.settled.contains_key(*id)
                || journal.references_admission(id)
        })
    {
        return Err("joined_scope_unverified".into());
    }
    let keys: BTreeSet<_> = journal
        .receipts
        .iter()
        .filter(|(_, receipt)| {
            receipt.response["target"] == json!(target)
                || receipt.request["target"] == json!(target)
        })
        .map(|(key, _)| key.clone())
        .collect();
    let session_target = LifecycleTarget::Session {
        host_epoch: session.host_epoch.clone(),
        session_id: session_id.into(),
        generation: session.generation,
    };
    retention::fence_exact_receipts(journal, &session_target, &keys)?;
    retention::fence_join_scope(journal, &target)?;
    let lease = journal
        .shared_sources
        .get_mut(source)
        .ok_or("shared_source_lease_missing")?;
    lease
        .callbacks
        .retain(|_, proof| proof.callback.request.target != target);
    lease.retired_subscribers.remove(subscriber);
    lease.closed.remove(subscriber);
    journal
        .sessions
        .get_mut(session_id)
        .ok_or("unknown_session")?
        .streams
        .remove(subscriber);
    Ok(())
}

pub(super) fn compact(journal: &mut Journal) {
    for lease in journal.shared_sources.values_mut() {
        if let Ok((_, session_id, _, _)) = operations::target_parts(&lease.context.source) {
            let streams = journal
                .sessions
                .get(session_id)
                .map(|session| &session.streams);
            lease.callbacks.retain(|_, proof| {
                operations::target_parts(&proof.callback.request.target)
                    .ok()
                    .is_some_and(|(_, _, subscriber, _)| {
                        subscriber.is_some_and(|id| {
                            streams.is_some_and(|streams| streams.contains_key(id))
                        })
                    })
            });
            lease
                .retired_subscribers
                .retain(|id| streams.is_some_and(|streams| streams.contains_key(id)));
            lease
                .closed
                .retain(|id, _| lease.retired_subscribers.contains(id));
        }
    }
    journal.shared_sources.retain(|_, lease| {
        if !lease.released {
            return true;
        }
        operations::target_parts(&lease.context.source)
            .ok()
            .and_then(|(_, session, stream, _)| {
                journal
                    .sessions
                    .get(session)
                    .and_then(|session| session.streams.get(stream?))
            })
            .is_some()
    });
}

impl Coordinator {
    /// Called only after the normal capture and input retirement barrier passed.
    /// Retain native ownership, and prove this transport's consumer retirement
    /// separately from provider resource release.
    pub(super) fn retain_shared_source(&self, key: &str, work: &Work) -> ModuleResult<bool> {
        if work.phase != LifecyclePhase::StreamRelease
            || work.join_context.is_some()
            || work.action_transfer.is_some()
        {
            return Ok(false);
        }
        let (_, session_id, Some(stream_id), generation) = operations::target_parts(&work.target)?
        else {
            return Ok(false);
        };
        let mut next = self.shared.borrow().value.clone();
        let Some(lease) = next.shared_sources.get_mut(stream_id) else {
            return Ok(false);
        };
        if lease.released
            || lease.release_queued
            || lease.context.source != work.target
            || !lease
                .subscribers
                .iter()
                .any(|(id, _)| id.as_str() != stream_id)
        {
            return Ok(false);
        }
        let stream = &next.sessions[session_id].streams[stream_id];
        if stream.generation != generation
            || stream.phase != StreamPhase::Quiescing
            || stream.retirements != stream.required
            || work.index != 0
            || !work.states.is_empty()
        {
            return Err("shared_source_retirement_unverified".into());
        }
        lease.subscribers.remove(stream_id);
        lease.deferred_release = Some(work.clone());
        lease.retirement = Some(SourceRetirementProof {
            target: work.target.clone(),
            required: stream.required.clone(),
            retirements: stream.retirements.clone(),
            receipt: key.into(),
        });
        let receipt = next
            .receipts
            .get_mut(key)
            .ok_or("shared_source_receipt_missing")?;
        receipt.work = None;
        receipt.response["status"] = json!("succeeded");
        receipt.response["consumer_retired"] = json!(true);
        receipt.response["resources_released"] = json!(false);
        receipt.response["shared_retained"] = json!(true);
        receipt.response["shared_source"] = json!(work.target);
        save(&self.shared, next)?;
        Ok(true)
    }

    pub(super) fn release_join_subscription(
        &self,
        journal: &mut Journal,
        work: &Work,
        response: &mut Value,
    ) -> ModuleResult<()> {
        if work.phase != LifecyclePhase::StreamRelease {
            return Ok(());
        }
        let (_, session_id, Some(stream_id), _) = operations::target_parts(&work.target)? else {
            return Ok(());
        };
        if let Some(context) = &work.join_context {
            let (_, _, Some(source_id), _) = operations::target_parts(&context.source)? else {
                return Err("invalid_join_source".into());
            };
            retire_admissions(journal, work, source_id)?;
            let lease = journal
                .shared_sources
                .get_mut(source_id)
                .ok_or("shared_source_lease_missing")?;
            if lease.context != *context || lease.released {
                return Err("shared_source_changed".into());
            }
            lease.subscribers.remove(stream_id);
            response["consumer_retired"] = json!(true);
            response["resources_released"] = json!(false);
            response["shared_retained"] = json!(true);
            response["shared_source"] = json!(context.source);
            if lease.subscribers.is_empty()
                && !lease.release_queued
                && let Some(mut release) = lease.deferred_release.take()
            {
                release.waiting_retirement = false;
                if let Some(epoch) = &work.recovery_authorized_epoch {
                    if epoch != &journal.host_epoch {
                        return Err("shared_source_recovery_epoch_changed".into());
                    }
                    release.recovery_authorized_epoch = Some(epoch.clone());
                    journal
                        .sessions
                        .get_mut(session_id)
                        .ok_or("unknown_session")?
                        .streams
                        .get_mut(source_id)
                        .ok_or("unknown_stream")?
                        .phase = StreamPhase::Quiescing;
                }
                lease.release_queued = true;
                let owner = journal.sessions[session_id].owner.clone();
                let operation_id = format!("shared-source-release-{}", source_id);
                let key = receipt_key(&owner.caller, &owner.initiator, &operation_id);
                if journal.receipts.contains_key(&key) {
                    return Err("shared_source_release_conflict".into());
                }
                let request = json!({"operation":"pause_stream","operation_id":operation_id,"target":release.target});
                journal.receipts.insert(key, Receipt {caller:owner.caller, initiator:owner.initiator, request,
                    response:json!({"status":"pending","operation_id":operation_id,"target":release.target,"fenced":true}),
                    work:Some(release), wait_target:None});
                if response["status"] != "succeeded" {
                    response["terminal_status_after_cleanup"] = response["status"].clone();
                }
                response["status"] = json!("pending");
                response["shared_source_cleanup_pending"] = json!(true);
            }
        } else if let Some(lease) = journal.shared_sources.get_mut(stream_id) {
            if lease.context.source != work.target {
                return Err("shared_source_changed".into());
            }
            lease.subscribers.remove(stream_id);
            if !lease.subscribers.is_empty() {
                return Err("shared_source_still_borrowed".into());
            }
            lease.released = true;
            lease.release_queued = false;
            response["consumer_retired"] = json!(true);
            response["resources_released"] = json!(true);
            response["shared_retained"] = json!(false);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn legacy_rejected_join_is_readable_only_as_closed_unproven_history() {
        let context = StreamJoinContext {
            source: LifecycleTarget::Stream {
                host_epoch: "epoch".into(),
                session_id: "session".into(),
                stream_id: "source".into(),
                generation: 1,
            },
            source_transition: "transition".into(),
            capture_binding_mode: amaterasu_extension_api::CaptureBindingMode::External,
            binding_owner: None,
            binding: None,
        };
        let mut lease = SharedSourceLease {
            context,
            subscribers: BTreeMap::new(),
            callbacks: BTreeMap::new(),
            retired_subscribers: BTreeSet::from(["child".into()]),
            closed: BTreeMap::from([(
                "child".into(),
                ClosedJoinEvidence {
                    cleanup_providers: vec![],
                    failed: Some(FailedPreparation {
                        reason: "processing_output_range_mismatch".into(),
                        uncertain: false,
                        states: BTreeMap::new(),
                    }),
                },
            )]),
            deferred_release: None,
            retirement: None,
            release_queued: false,
            released: true,
        };
        assert!(legacy_closed_join_unproven(&lease, "child"));
        lease.released = false;
        assert!(!legacy_closed_join_unproven(&lease, "child"));
        lease.released = true;
        lease
            .closed
            .get_mut("child")
            .unwrap()
            .failed
            .as_mut()
            .unwrap()
            .uncertain = true;
        assert!(!legacy_closed_join_unproven(&lease, "child"));
        lease
            .closed
            .get_mut("child")
            .unwrap()
            .failed
            .as_mut()
            .unwrap()
            .uncertain = false;
        lease.subscribers.insert("other".into(), 1);
        assert!(!legacy_closed_join_unproven(&lease, "child"));
    }
    #[test]
    fn subscription_admission_reserves_closed_history_before_any_effect() {
        let directory =
            std::env::temp_dir().join(format!("shared-source-capacity-{}", identifier().unwrap()));
        std::fs::create_dir(&directory).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let coordinator = Coordinator::open(&directory.join("journal.json")).unwrap();
        let mut journal = coordinator.shared.borrow().value.clone();
        let target = |id: &str| LifecycleTarget::Stream {
            host_epoch: journal.host_epoch.clone(),
            session_id: "session".into(),
            stream_id: id.into(),
            generation: 1,
        };
        let context = StreamJoinContext {
            source: target("source"),
            source_transition: "source-prepare".into(),
            capture_binding_mode: amaterasu_extension_api::CaptureBindingMode::External,
            binding_owner: None,
            binding: None,
        };
        let next = target("next");
        for i in 0..63 {
            subscribe(
                &mut journal,
                &context,
                &LifecycleTarget::Stream {
                    host_epoch: next_host(&context),
                    session_id: "session".into(),
                    stream_id: format!("child-{i}"),
                    generation: 1,
                },
            )
            .unwrap();
        }
        let saved = serde_json::to_value(&journal).unwrap();
        assert_eq!(
            subscribe(&mut journal, &context, &next).unwrap_err(),
            "shared_source_subscription_capacity"
        );
        assert_eq!(serde_json::to_value(&journal).unwrap(), saved);
        let lease = journal.shared_sources.get_mut("source").unwrap();
        for i in 0..63 {
            let id = format!("child-{i}");
            lease.subscribers.remove(&id);
            lease.retired_subscribers.insert(id);
        }
        let saved = serde_json::to_value(&journal).unwrap();
        assert_eq!(
            subscribe(&mut journal, &context, &next).unwrap_err(),
            "shared_source_subscription_capacity"
        );
        assert_eq!(serde_json::to_value(&journal).unwrap(), saved);
        drop(coordinator);
        std::fs::remove_dir_all(directory).unwrap();
    }
    fn next_host(context: &StreamJoinContext) -> String {
        let LifecycleTarget::Stream { host_epoch, .. } = &context.source else {
            unreachable!()
        };
        host_epoch.clone()
    }
}
