//! Validation of durable, graph-complete stream cleanup for an action replan.
//!
//! This is only one part of the eventual replan gate. It must not be used to
//! authorize an effect or replan until the coordinator also proves that the
//! original admissions were frozen before cleanup, retains every admission and
//! callback atomically, and verifies owner finalization and session restoration.
use super::*;
use amaterasu_extension_api::{CleanupState, ProviderResponse};
use serde::{Deserialize, Serialize};

fn complete_outcomes(
    obligations: &[amaterasu_extension_api::ResourceObligation],
    outcomes: &[amaterasu_extension_api::CleanupOutcome],
) -> bool {
    outcomes.len() == obligations.len()
        && obligations.iter().all(|obligation| {
            outcomes
                .iter()
                .filter(|outcome| {
                    outcome.obligation_id == obligation.obligation_id
                        && outcome.state == CleanupState::Released
                })
                .count()
                == 1
        })
}

fn complete_provider_order(graph: &ProviderGraph, providers: &[String], owner: &str) -> bool {
    let positions: BTreeMap<_, _> = graph
        .order
        .iter()
        .enumerate()
        .map(|(i, id)| (id.as_str(), i))
        .collect();
    positions.len() == graph.order.len()
        && positions.contains_key(owner)
        && graph.dependencies.len() == positions.len()
        && providers.iter().eq(graph.order.iter().rev())
        && graph.dependencies.iter().all(|(id, dependencies)| {
            let Some(position) = positions.get(id.as_str()) else {
                return false;
            };
            let unique: BTreeSet<_> = dependencies.iter().collect();
            unique.len() == dependencies.len()
                && dependencies.iter().all(|dependency| {
                    positions
                        .get(dependency.as_str())
                        .is_some_and(|parent| parent < position)
                })
        })
}

fn session_dependents_noop(
    journal: &Journal,
    session: &SessionRecord,
    snapshot: &ActionReleaseSnapshot,
    owner: &str,
    historical: bool,
) -> bool {
    let graph = &snapshot.session_graph;
    let reverse: Vec<_> = graph.order.iter().rev().cloned().collect();
    if !complete_provider_order(graph, &reverse, owner)
        || !super::session_prepare_evidence::all_dependent_session_prepares_noop(
            journal,
            &snapshot.session_target,
            graph,
            owner,
        )
    {
        return false;
    }
    let mut affected = BTreeSet::from([owner.to_owned()]);
    for provider in &graph.order {
        if graph.dependencies[provider]
            .iter()
            .any(|dependency| affected.contains(dependency))
        {
            affected.insert(provider.clone());
        }
    }
    affected.remove(owner);
    for provider in affected {
        let Some((id, transition)) = super::session_prepare_evidence::admission_for(
            journal,
            &snapshot.session_target,
            &provider,
        ) else {
            return false;
        };
        let Some(frozen) = snapshot.session_admissions.get(id) else {
            return false;
        };
        if frozen.admission.provider != provider
            || frozen.transition_id != transition
            || journal.authority.settled.get(id) != Some(&frozen.admission)
            || session.obligations.iter().any(|obligation| {
                obligation.provider == provider
                    && !dependent_stream_obligation_is_attributed(
                        journal, session, snapshot, obligation, historical,
                    )
            })
        {
            return false;
        }
    }
    true
}

fn dependent_stream_obligation_is_attributed(
    journal: &Journal,
    session: &SessionRecord,
    snapshot: &ActionReleaseSnapshot,
    obligation: &amaterasu_extension_api::ResourceObligation,
    historical: bool,
) -> bool {
    let frozen_stream = snapshot.stream_admissions.values().any(|entry| {
        entry.admission.provider == obligation.provider
            && entry.admission.plan.obligations.contains(obligation)
    });
    frozen_stream
        || session.admission_ids.iter().any(|admission_id| {
            let Some(target @ LifecycleTarget::Stream { session_id, .. }) =
                session.admission_targets.get(admission_id)
            else {
                return false;
            };
            session_id == &session.id
                && target != &snapshot.stream_target
                && session.admission_phases.get(admission_id)
                    == Some(&LifecyclePhase::StreamPrepare)
                && journal
                    .authority
                    .admissions
                    .get(admission_id)
                    .or_else(|| journal.authority.settled.get(admission_id))
                    .is_some_and(|admission| {
                        admission.provider == obligation.provider
                            && admission.plan.obligations.contains(obligation)
                    })
                && (historical || journal.verified_released_output(target).is_some())
        })
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ActionReleaseProof {
    pub target: LifecycleTarget,
    pub transition_id: String,
    /// Frozen at action transfer, before any cleanup callback; retained until
    /// this proof has been checked against the original stream admissions.
    pub original_admissions: BTreeSet<String>,
    /// One terminal callback per selected provider in reverse graph order.
    pub callbacks: Vec<super::composition::ProducerCallbackProof>,
}

fn frozen_admissions_still_present(
    journal: &Journal,
    session: &SessionRecord,
    snapshot: &ActionReleaseSnapshot,
) -> bool {
    let check = |id: &String,
                 original: &ActionSnapshotAdmission,
                 target: &LifecycleTarget,
                 phase: LifecyclePhase| {
        session.admission_ids.contains(id)
            && session.admission_targets.get(id) == Some(target)
            && session.admission_phases.get(id) == Some(&phase)
            && journal.admission_transitions.get(id) == Some(&original.transition_id)
            && journal.authority.settled.get(id) == Some(&original.admission)
    };
    snapshot.session_admissions.iter().all(|(id, original)| {
        check(
            id,
            original,
            &snapshot.session_target,
            LifecyclePhase::SessionPrepare,
        )
    }) && snapshot.stream_admissions.iter().all(|(id, original)| {
        check(
            id,
            original,
            &snapshot.stream_target,
            LifecyclePhase::StreamPrepare,
        )
    })
}

/// Verify complete cleanup against the durable action work and retained
/// authority admissions. Missing provenance, deferred work and unknown broker
/// outcomes do not prove cleanup, even when the associated plan is empty.
pub(super) fn validate_graph_callbacks(
    journal: &Journal,
    work: &Work,
    proof: &ActionReleaseProof,
) -> bool {
    validate_graph_callbacks_inner(journal, work, proof, false)
}

/// Historical proof remains valid after later admissions and streams exist.
pub(super) fn validate_historical_graph_callbacks(
    journal: &Journal,
    work: &Work,
    proof: &ActionReleaseProof,
) -> bool {
    validate_graph_callbacks_inner(journal, work, proof, true)
}

fn validate_graph_callbacks_inner(
    journal: &Journal,
    work: &Work,
    proof: &ActionReleaseProof,
    historical: bool,
) -> bool {
    let Some(transfer) = &work.action_transfer else {
        return false;
    };
    let Some(snapshot) = &transfer.snapshot else {
        return false;
    };
    let LifecycleTarget::Stream {
        host_epoch,
        session_id,
        stream_id,
        generation,
    } = &work.target
    else {
        return false;
    };
    let Some(session) = journal.sessions.get(session_id) else {
        return false;
    };
    let Some(stream) = session.streams.get(stream_id) else {
        return false;
    };
    let Some(graph) = &stream.provider_graph else {
        return false;
    };
    let Some(work_graph) = &work.provider_graph else {
        return false;
    };
    let Ok((owner, _, _)) = journal
        .actions
        .execution_for_permit(&transfer.service, &transfer.permit)
    else {
        return false;
    };
    let target_matches = match &transfer.binding.target {
        crate::action_authority::ActionTarget::Stream {
            host_epoch: epoch,
            session_id: id,
            stream_id: stream,
            expected_generation,
        } => {
            epoch == host_epoch
                && id == session_id
                && stream == stream_id
                && expected_generation == generation
        }
        crate::action_authority::ActionTarget::Session {
            host_epoch: epoch,
            session_id: id,
            expected_generation,
        } => epoch == host_epoch && id == session_id && *expected_generation == session.generation,
        _ => false,
    };
    if host_epoch != &session.host_epoch
        || stream.generation != *generation || stream.phase != StreamPhase::Retired
        || stream.retirements != stream.required || work.phase != LifecyclePhase::StreamRelease
        || (!historical && session.streams.iter().any(|(id, other)| id != stream_id && other.phase != StreamPhase::Retired))
        || proof.target != work.target || proof.transition_id != work.transition_id
        || graph.order != work_graph.order || graph.dependencies != work_graph.dependencies
        || !complete_provider_order(graph, &work.providers, &owner)
        || proof.callbacks.len() != work.providers.len() || work.index != work.providers.len()
        || work.waiting_retirement || work.failure.is_some() || !work.cleanup_blockers.is_empty()
        || transfer.binding.source != session.owner
        || if historical { !frozen_admissions_still_present(journal, session, snapshot) } else { super::action_endpoints::capture_action_snapshot(journal, &work.target).ok().as_ref() != Some(snapshot) }
        || !target_matches
        // Stream cleanup does not prove session-scoped dependent restoration;
        // each selected dependent must have authentic frozen no-op evidence.
        || !session_dependents_noop(journal, session, snapshot, &owner, historical)
    {
        return false;
    }
    let original_admissions: BTreeSet<_> = session
        .admission_ids
        .iter()
        .filter(|id| {
            session.admission_targets.get(*id) == Some(&work.target)
                && session.admission_phases.get(*id) == Some(&LifecyclePhase::StreamPrepare)
        })
        .cloned()
        .collect();
    if original_admissions != proof.original_admissions
        || proof.original_admissions != snapshot.stream_admissions.keys().cloned().collect()
    {
        return false;
    }
    for (provider, callback) in work.providers.iter().zip(&proof.callbacks) {
        let request = &callback.request;
        let Some(admission_id) = request.admission_id.as_deref() else {
            return false;
        };
        let Some(admission) = journal.authority.settled.get(admission_id) else {
            return false;
        };
        let crate::provider_dispatch::DispatchState::Completed { payload } =
            &callback.terminal.state
        else {
            return false;
        };
        let Ok(response) = serde_json::from_value::<ProviderResponse>(payload.clone()) else {
            return false;
        };
        let ProviderResponse::Cleanup { outcomes, .. } = &response else {
            return false;
        };
        if &admission.provider != provider
            || &callback.participant.provider != provider
            || callback.participant != admission.selection.participant
            || request.target != work.target
            || request.phase != LifecyclePhase::StreamRelease
            || request.operation != amaterasu_extension_api::ProviderOperation::LifecycleCallback
            || request.transition_id != work.transition_id
            || request.approved_plan.as_ref() != Some(&admission.plan)
            || session.admission_targets.get(admission_id) != Some(&work.target)
            || session.admission_phases.get(admission_id) != Some(&LifecyclePhase::StreamRelease)
            || journal.admission_transitions.get(admission_id) != Some(&work.transition_id)
            || !session.admission_ids.contains(admission_id)
            || !admission.selection.approvals.iter().any(|approval| {
                approval.broker_approval == callback.terminal.call.approval
                    && super::composition::approval_matches_call(approval, &callback.terminal.call)
            })
            || !super::composition::valid_callback_proof(callback, &response)
            || !complete_outcomes(&admission.plan.obligations, outcomes)
        {
            return false;
        }
        // A release plan must cover every original stream-scoped obligation;
        // checking only newly declared cleanup obligations could drop effects.
        for old_id in &proof.original_admissions {
            let Some(old) = journal.authority.settled.get(old_id) else {
                return false;
            };
            if &old.provider != provider {
                continue;
            }
            if old.plan.obligations.iter().any(|obligation| {
                !admission.plan.obligations.contains(obligation)
                    || outcomes
                        .iter()
                        .filter(|outcome| {
                            outcome.obligation_id == obligation.obligation_id
                                && outcome.state == CleanupState::Released
                        })
                        .count()
                        != 1
            }) {
                return false;
            }
        }
    }
    true
}

/// Require exactly one independently verified release proof for the frozen
/// owner, target and transition. Composed and standalone books may not overlap.
pub(super) fn validate_owner_release(journal: &Journal, work: &Work, owner: &str) -> bool {
    if journal.stream_releases.has_released(&work.target)
        && journal.composed_releases.contains_target(&work.target)
    {
        return false;
    }
    let standalone =
        journal
            .stream_releases
            .has_released_for(&work.target, owner, &work.transition_id)
            && journal
                .stream_releases
                .has_valid_released(journal, &work.target);
    let composed = journal
        .composed_releases
        .verified_work_for(journal, &work.target, &work.transition_id)
        .is_some_and(|released| {
            released.provider_graph == work.provider_graph
                && released.providers == work.providers
                && released
                    .provider_graph
                    .as_ref()
                    .is_some_and(|graph| graph.dependencies.get(owner).is_some_and(Vec::is_empty))
        });
    standalone ^ composed
}

/// Construct the action's retained callback proof exclusively from the exact
/// verified graph release; callers must persist it with ReplanRequired.
pub(super) fn proof_from_composed_release(
    journal: &Journal,
    work: &Work,
) -> Option<ActionReleaseProof> {
    let transfer = work.action_transfer.as_ref()?;
    if journal.stream_releases.has_released(&work.target) {
        return None;
    }
    let snapshot = transfer.snapshot.as_ref()?;
    let released =
        journal
            .composed_releases
            .verified_work_for(journal, &work.target, &work.transition_id)?;
    if released.provider_graph != work.provider_graph
        || released.providers != work.providers
        || released.index != work.providers.len()
    {
        return None;
    }
    let callbacks = work
        .providers
        .iter()
        .map(|provider| {
            let state = released.states.get(provider)?;
            Some(super::composition::ProducerCallbackProof {
                participant: state.participant.clone(),
                request: state.request.clone(),
                terminal: state.terminal_dispatch.clone()?,
                pending_receipt: state.pending_receipt.clone(),
                pending_dispatch: state.pending_dispatch.clone(),
            })
        })
        .collect::<Option<Vec<_>>>()?;
    Some(ActionReleaseProof {
        target: work.target.clone(),
        transition_id: work.transition_id.clone(),
        original_admissions: snapshot.stream_admissions.keys().cloned().collect(),
        callbacks,
    })
}

/// Replan can only be journaled after every graph callback and the owner's
/// authenticated released-output finalization have been retained. The caller
/// must separately check the current host epoch before granting live access.
pub(super) fn validate_replan_stage(
    journal: &Journal,
    receipt: &Receipt,
    work: &Work,
    transfer: &ActionTransfer,
) -> bool {
    let Some(proof) = &transfer.release_proof else {
        return false;
    };
    let Ok((owner, _, _)) = journal
        .actions
        .execution_for_permit(&transfer.service, &transfer.permit)
    else {
        return false;
    };
    matches!(
        receipt.response["status"].as_str(),
        Some("replan_required" | "recovery_required")
    ) && receipt.response["operation_id"].as_str() == Some(transfer.binding.operation_id.as_str())
        && receipt.response["target"]
            == serde_json::to_value(&transfer.binding.target).unwrap_or(Value::Null)
        && receipt.response["plan_digest"].as_str() == Some(transfer.binding.plan_digest.as_str())
        && receipt.response["transition_id"].as_str() == Some(work.transition_id.as_str())
        && validate_graph_callbacks(journal, work, proof)
        && validate_owner_release(journal, work, &owner)
}

/// Rebound reservations retain all cleanup evidence and their exact new rights.
pub(super) fn validate_rebound_stage(
    journal: &Journal,
    receipt: &Receipt,
    work: &Work,
    transfer: &ActionTransfer,
) -> bool {
    let Some(proof) = &transfer.release_proof else {
        return false;
    };
    let Some(id) = &transfer.admission_id else {
        return false;
    };
    let Some(admission) = journal.authority.admissions.get(id) else {
        return false;
    };
    let Ok((owner, _, plan)) = journal
        .actions
        .execution_for_permit(&transfer.service, &transfer.permit)
    else {
        return false;
    };
    receipt.response["status"] == "succeeded"
        && receipt.response["operation_id"].as_str() == Some(transfer.binding.operation_id.as_str())
        && receipt.response["target"]
            == serde_json::to_value(&transfer.binding.target).unwrap_or(Value::Null)
        && receipt.response["plan_digest"].as_str() == Some(transfer.binding.plan_digest.as_str())
        && admission.provider == owner
        && admission.plan == plan
        && admission
            .rights
            .iter()
            .all(|(domain, _)| journal.authority.domain_locks.get(domain) == Some(id))
        && validate_graph_callbacks(journal, work, proof)
        && validate_owner_release(journal, work, &owner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn graph_action_release_requires_exact_verified_work() {
        let journal: Journal = serde_json::from_value(
            json!({"version":3,"host_epoch":"epoch","receipts":{},"sessions":{}}),
        )
        .unwrap();
        let work: Work = serde_json::from_value(json!({"phase":"stream.release","target":{"kind":"stream","host_epoch":"epoch","session_id":"session","stream_id":"stream","generation":1},"intent":null,"reason":null,"video":null,"transition_id":"transition","providers":[],"index":0,"states":{},"candidate":null,"waiting_retirement":false})).unwrap();
        assert!(!validate_owner_release(&journal, &work, "owner"));
        assert!(proof_from_composed_release(&journal, &work).is_none());
    }

    #[test]
    fn dependent_session_noop_requires_authenticated_evidence() {
        let root = std::env::temp_dir().join(format!(
            "action-noop-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let coordinator = Coordinator::open(&root.join("journal")).unwrap();
        let mut journal = coordinator.shared.borrow().value.clone();
        let epoch = journal.host_epoch.clone();
        let target = LifecycleTarget::Session {
            host_epoch: epoch.clone(),
            session_id: "session".into(),
            generation: 1,
        };
        let mut graph = ProviderGraph {
            order: vec!["owner".into()],
            dependencies: BTreeMap::from([("owner".into(), vec![])]),
        };
        let session = SessionRecord {
            processing_decision: None,
            resource_generations: BTreeMap::new(),
            provider_graph: Some(graph.clone()),
            host_epoch: epoch.clone(),
            id: "session".into(),
            generation: 1,
            phase: SessionPhase::Ready,
            owner: crate::action_authority::Principal {
                caller: "transport".into(),
                initiator: "user".into(),
            },
            delegates: Default::default(),
            application_ref: json!({}),
            settings_revision: "revision".into(),
            application_retired: false,
            application_retirement_proof: None,
            application_disposition: None,
            streams: Default::default(),
            obligations: vec![],
            required_consumers: None,
            application: None,
            application_operation_id: None,
            admission_ids: Default::default(),
            admission_targets: Default::default(),
            capture_binding_mode: Default::default(),
            prepared_plan_ids: Default::default(),
            admission_phases: Default::default(),
            provider_settings: Default::default(),
        };
        journal.sessions.insert("session".into(), session);
        let mut snapshot = ActionReleaseSnapshot {
            session_target: target,
            stream_target: LifecycleTarget::Stream {
                host_epoch: epoch,
                session_id: "session".into(),
                stream_id: "stream".into(),
                generation: 1,
            },
            session_graph: graph.clone(),
            stream_graph: graph.clone(),
            session_admissions: Default::default(),
            stream_admissions: Default::default(),
        };
        assert!(session_dependents_noop(
            &journal,
            &journal.sessions["session"],
            &snapshot,
            "owner",
            false
        ));
        graph.order.push("settings".into());
        graph
            .dependencies
            .insert("settings".into(), vec!["owner".into()]);
        snapshot.session_graph = graph.clone();
        journal.sessions.get_mut("session").unwrap().provider_graph = Some(graph);
        assert!(!session_dependents_noop(
            &journal,
            &journal.sessions["session"],
            &snapshot,
            "owner",
            false
        ));
        let obligation = json!({"obligation_id":"stream-restore","provider":"settings","resource_id":"display","mutation_domain":null,"depends_on":[],"cleanup_method":{"service":"settings/2","method":"cleanup","interface_major":2,"interface_minor":0,"schema_revision":1},"payload":{}});
        let method = json!({"service":"settings/2","method":"callback","interface_major":2,"interface_minor":0,"schema_revision":1});
        let admission: crate::prepare_authority::Admission = serde_json::from_value(json!({"provider":"settings","selection":{"participant":{"provider":"settings","contract_version":2,"depends_on":["owner"],"plan_method":method,"callback_method":method,"status_method":method,"capabilities":[],"cleanup_methods":[],"settings_revision":"rev","settings":{}},"approvals":[],"capabilities":[],"cleanup_methods":[],"authorized_borrowers":[],"approved_control_bindings":[]},"plan":{"plan_id":"plan","revision":1,"resources":[],"mutation_domains":[],"required_capabilities":[],"constraints":{},"obligations":[obligation],"affected_consumers":[],"consumer_change":"live","candidate_constraint":null,"allowed_degradation":[]},"rights":[]})).unwrap();
        let obligation: amaterasu_extension_api::ResourceObligation =
            serde_json::from_value(obligation).unwrap();
        snapshot.stream_admissions.insert(
            "old".into(),
            ActionSnapshotAdmission {
                admission: admission.clone(),
                transition_id: "stream-transition".into(),
            },
        );
        assert!(dependent_stream_obligation_is_attributed(
            &journal,
            &journal.sessions["session"],
            &snapshot,
            &obligation,
            false
        ));
        let mut unknown = obligation.clone();
        unknown.obligation_id = "unknown".into();
        assert!(!dependent_stream_obligation_is_attributed(
            &journal,
            &journal.sessions["session"],
            &snapshot,
            &unknown,
            false
        ));
        snapshot.stream_admissions.clear();
        let session = journal.sessions.get_mut("session").unwrap();
        session.admission_ids.insert("later".into());
        session.admission_targets.insert(
            "later".into(),
            LifecycleTarget::Stream {
                host_epoch: session.host_epoch.clone(),
                session_id: "session".into(),
                stream_id: "later".into(),
                generation: 1,
            },
        );
        session
            .admission_phases
            .insert("later".into(), LifecyclePhase::StreamPrepare);
        journal.authority.settled.insert("later".into(), admission);
        assert!(!dependent_stream_obligation_is_attributed(
            &journal,
            &journal.sessions["session"],
            &snapshot,
            &obligation,
            false
        ));
        assert!(dependent_stream_obligation_is_attributed(
            &journal,
            &journal.sessions["session"],
            &snapshot,
            &obligation,
            true
        ));
        drop(coordinator);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn owner_only_or_malformed_graph_never_proves_composed_cleanup() {
        let mut graph = ProviderGraph {
            order: vec!["owner".into(), "settings".into(), "consumer".into()],
            dependencies: BTreeMap::from([
                ("owner".into(), vec![]),
                ("settings".into(), vec!["owner".into()]),
                ("consumer".into(), vec!["settings".into()]),
            ]),
        };
        assert!(complete_provider_order(
            &graph,
            &["consumer".into(), "settings".into(), "owner".into()],
            "owner"
        ));
        assert!(!complete_provider_order(&graph, &["owner".into()], "owner"));
        assert!(!complete_provider_order(
            &graph,
            &["settings".into(), "consumer".into(), "owner".into()],
            "owner"
        ));
        graph
            .dependencies
            .get_mut("owner")
            .unwrap()
            .push("consumer".into());
        assert!(!complete_provider_order(
            &graph,
            &["consumer".into(), "settings".into(), "owner".into()],
            "owner"
        ));
    }

    #[test]
    fn cleanup_requires_every_exact_released_outcome() {
        let obligations: Vec<amaterasu_extension_api::ResourceObligation> = serde_json::from_value(json!([
            {"obligation_id":"a","provider":"owner","resource_id":"screen","mutation_domain":"seat","depends_on":[],"cleanup_method":{"service":"owner/2","method":"cleanup","interface_major":2,"interface_minor":0,"schema_revision":1},"payload":{}},
            {"obligation_id":"b","provider":"owner","resource_id":"screen","mutation_domain":"seat","depends_on":[],"cleanup_method":{"service":"owner/2","method":"cleanup","interface_major":2,"interface_minor":0,"schema_revision":1},"payload":{}}
        ])).unwrap();
        let outcomes = |value| {
            serde_json::from_value::<Vec<amaterasu_extension_api::CleanupOutcome>>(value).unwrap()
        };
        assert!(complete_outcomes(
            &obligations,
            &outcomes(
                json!([{"obligation_id":"a","state":"released","reason":null},{"obligation_id":"b","state":"released","reason":null}])
            )
        ));
        for state in ["pending", "deferred_restoration", "effect_unknown"] {
            assert!(!complete_outcomes(
                &obligations,
                &outcomes(
                    json!([{"obligation_id":"a","state":"released","reason":null},{"obligation_id":"b","state":state,"reason":null}])
                )
            ));
        }
        assert!(!complete_outcomes(
            &obligations,
            &outcomes(
                json!([{"obligation_id":"a","state":"released","reason":null},{"obligation_id":"a","state":"released","reason":null}])
            )
        ));
        assert!(!complete_outcomes(
            &obligations,
            &outcomes(json!([{"obligation_id":"a","state":"released","reason":null}]))
        ));
        assert!(complete_outcomes(&[], &[]));
    }
}
