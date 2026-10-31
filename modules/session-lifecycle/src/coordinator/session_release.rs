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

    pub fn validate(&self, journal: &Journal) -> bool {
        self.proofs.len() <= MAX_RELEASES
            && self.proofs.iter().enumerate().all(|(index, proof)| {
                validate_proof(journal, proof)
                    && !self.proofs[..index]
                        .iter()
                        .any(|old| old.target == proof.target)
            })
    }
    pub fn references_admission(&self, id: &str) -> bool {
        self.proofs.iter().any(|proof| {
            proof.root_admission_id == id
                || proof.callback.request.admission_id.as_deref() == Some(id)
        })
    }
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
        |target: &LifecycleTarget| journal.stream_releases.has_valid_released(journal, target);
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
    }) {
        return false;
    }
    session.provider_graph.as_ref().is_some_and(|graph| {
        graph.order == [provider] && graph.dependencies.get(provider).is_some_and(Vec::is_empty)
    })
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

fn validate_proof(journal: &Journal, proof: &SessionReleaseProof) -> bool {
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
    session_lineage_closed(journal, &proof.target, &proof.provider)
        && root.state == composition::OutputState::Released
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
    if work.phase != LifecyclePhase::SessionRelease
        || !session_lineage_closed(journal, &work.target, provider)
    {
        return Err("session_lineage_not_retired".into());
    }
    let (_, session_id, None, _) = operations::target_parts(&work.target)? else {
        return Err("session_target_required".into());
    };
    let session = journal.sessions.get(session_id).ok_or("unknown_session")?;
    if session.phase != SessionPhase::Ending {
        return Err("session_not_ending".into());
    }
    let graph = work
        .provider_graph
        .as_ref()
        .ok_or("provider_graph_missing")?;
    if work.providers != [provider]
        || work.cleanup_blocked(provider)
        || graph.order != [provider]
        || graph
            .dependencies
            .get(provider)
            .is_none_or(|deps| !deps.is_empty())
    {
        return Err("standalone_release_required".into());
    }
    let mut roots = journal.composition.records().filter(|record| {
        record.output.origin == work.target
            && record.output.producer == provider
            && record.state == composition::OutputState::Retained
    });
    let root = roots.next().ok_or("session_root_missing_or_ambiguous")?;
    if roots.next().is_some() || !valid_root_provenance(journal, root) {
        return Err("session_root_missing_or_ambiguous".into());
    }
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
    if !validate_proof(journal, &proof) {
        journal.composition = previous;
        return Err("invalid_session_release_proof".into());
    }
    journal.session_releases.proofs.push(proof);
    Ok(())
}

#[cfg(test)]
mod tests {
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

    fn complete_fixture() -> (Journal, Work, PrepareState) {
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
        let root_request = request("session.prepare", "root", "prepare", "root-call");
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
        let release_request = request(
            "session.release",
            "release-admission",
            "release",
            "release-call",
        );
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
            "standalone_release_required"
        );
        work.provider_graph
            .as_mut()
            .unwrap()
            .dependencies
            .remove("foreign");
        work.providers.push("foreign".into());
        assert_eq!(
            validate_session_release_preflight(&journal, &work, "owner").unwrap_err(),
            "standalone_release_required"
        );
    }
}
