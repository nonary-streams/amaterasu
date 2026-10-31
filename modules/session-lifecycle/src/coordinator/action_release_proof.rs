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

/// Verify complete cleanup against the durable action work and retained
/// authority admissions. Missing provenance, deferred work and unknown broker
/// outcomes do not prove cleanup, even when the associated plan is empty.
pub(super) fn validate_graph_callbacks(
    journal: &Journal,
    work: &Work,
    proof: &ActionReleaseProof,
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
    let Some(session_graph) = &session.provider_graph else {
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
    if host_epoch != &journal.host_epoch || host_epoch != &session.host_epoch
        || stream.generation != *generation || stream.phase != StreamPhase::Retired
        || stream.retirements != stream.required || work.phase != LifecyclePhase::StreamRelease
        || session.streams.iter().any(|(id, other)| id != stream_id && other.phase != StreamPhase::Retired)
        || proof.target != work.target || proof.transition_id != work.transition_id
        || graph.order != work_graph.order || graph.dependencies != work_graph.dependencies
        || !complete_provider_order(graph, &work.providers, &owner)
        || proof.callbacks.len() != work.providers.len() || work.index != work.providers.len()
        || work.waiting_retirement || work.failure.is_some() || !work.cleanup_blockers.is_empty()
        || transfer.binding.source != session.owner
        || super::action_endpoints::capture_action_snapshot(journal, &work.target).ok().as_ref() != Some(snapshot)
        || !target_matches
        // Stream cleanup does not prove session-scoped dependent restoration.
        || !super::action_endpoints::graph_proves_no_dependents(&session_graph.order, &session_graph.dependencies, &owner)
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
        let ProviderResponse::Cleanup { outcomes } = &response else {
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
