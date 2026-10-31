//! Resource-generation metadata reconstructed from canonical producer evidence.
//!
//! This lookup does not authorize an effect. Active callers must independently
//! validate the live permit, graph, admission and owner identity.
use super::composition::{OutputState, StoredOutput};
use super::*;
use amaterasu_extension_api::{ResourceOutput, validate_output_refinement};

fn root_generation(journal: &Journal, root: &StoredOutput) -> ModuleResult<u64> {
    let LifecycleTarget::Session {
        host_epoch,
        session_id,
        generation,
    } = &root.output.origin
    else {
        return Err("resource_root_not_session".into());
    };
    let session = journal
        .sessions
        .get(session_id)
        .ok_or("resource_session_missing")?;
    let admission = journal
        .authority
        .admissions
        .get(&root.admission_id)
        .or_else(|| journal.authority.settled.get(&root.admission_id))
        .ok_or("resource_admission_missing")?;
    let proof = root
        .callback_proof
        .as_ref()
        .ok_or("resource_callback_missing")?;
    let resource_generation = proof
        .request
        .resource_generation
        .filter(|value| *value > 0)
        .ok_or("resource_generation_missing")?;
    if session.host_epoch != *host_epoch
        || session.id != *session_id
        || session.generation != *generation
        || !session.admission_ids.contains(&root.admission_id)
        || session.admission_targets.get(&root.admission_id) != Some(&root.output.origin)
        || session.admission_phases.get(&root.admission_id) != Some(&LifecyclePhase::SessionPrepare)
        || journal.admission_transitions.get(&root.admission_id) != Some(&root.transition_id)
        || admission.provider != root.output.producer
        || admission.plan != root.plan
        || !composition::validate_callback_provenance(root, admission)
    {
        return Err("resource_root_provenance_invalid".into());
    }
    Ok(resource_generation)
}

/// Return the frozen root generation for an exact retained historical record.
/// This is read-only evidence and never authorizes an effect or a new binding.
#[allow(dead_code)] // Consumed by historical proof endpoints.
pub(super) fn historical_generation(journal: &Journal, root: &StoredOutput) -> ModuleResult<u64> {
    if !journal.composition.records().any(|record| record == root) {
        return Err("resource_root_not_retained".into());
    }
    root_generation(journal, root)
}

/// Verify an exact retained root is still the current session resource. This
/// is metadata only; action or lifecycle callers must establish separate rights.
#[allow(dead_code)] // Consumed by action target metadata.
pub(super) fn current_root_generation(journal: &Journal, root: &StoredOutput) -> ModuleResult<u64> {
    if root.state != OutputState::Retained {
        return Err("resource_root_not_active".into());
    }
    let LifecycleTarget::Session { host_epoch, .. } = &root.output.origin else {
        return Err("resource_root_not_session".into());
    };
    if journal.host_epoch != *host_epoch {
        return Err("resource_root_stale_epoch".into());
    }
    let generation = historical_generation(journal, root)?;
    let LifecycleTarget::Session { session_id, .. } = &root.output.origin else {
        return Err("resource_root_not_session".into());
    };
    let session = journal
        .sessions
        .get(session_id)
        .ok_or("resource_session_missing")?;
    if session.resource_generations.get(&root.output.producer) != Some(&generation) {
        return Err("resource_generation_stale".into());
    }
    Ok(generation)
}

/// Block stream admission until each released session root has been replaced by
/// one authenticated newer incarnation at the current owner cursor.
pub(super) fn released_root_requires_reprepare(journal: &Journal, session: &SessionRecord) -> bool {
    released_root_requires_reprepare_except(journal, session, None)
}

/// An exact verified removal may enter its own reprepare before a replacement
/// exists. Every older root for that owner must have independent closure proof.
pub(super) fn released_root_requires_reprepare_except(
    journal: &Journal,
    session: &SessionRecord,
    allowed: Option<(&str, u64)>,
) -> bool {
    let origin = LifecycleTarget::Session {
        host_epoch: session.host_epoch.clone(),
        session_id: session.id.clone(),
        generation: session.generation,
    };
    for released in journal
        .composition
        .records()
        .filter(|record| record.output.origin == origin && record.state == OutputState::Released)
    {
        let Some(cursor) = session.resource_generations.get(&released.output.producer) else {
            return true;
        };
        let Ok(old_generation) = historical_generation(journal, released) else {
            return true;
        };
        if allowed.is_some_and(|(owner, generation)| {
            owner == released.output.producer
                && generation == *cursor
                && old_generation <= generation
                && super::action_disposition::resolved_at(
                    journal,
                    &session.id,
                    owner,
                    old_generation,
                )
                .is_some_and(|proof| {
                    proof.root_admission_id == released.admission_id
                        && proof.resource == released.output.resource
                })
        }) {
            continue;
        }
        if old_generation >= *cursor {
            return true;
        }
        let mut replacements = journal.composition.records().filter(|record| {
            record.output.origin == origin
                && record.output.producer == released.output.producer
                && record.state == OutputState::Retained
                && current_root_generation(journal, record).ok() == Some(*cursor)
        });
        let Some(replacement) = replacements.next() else {
            return true;
        };
        if replacements.next().is_some()
            || replacement.output.resource.incarnation == released.output.resource.incarnation
        {
            return true;
        }
    }
    false
}

/// Active endpoint metadata comes from the immutable root callback, never the
/// incoming owner request. The caller still checks rights and frozen graph.
pub(super) fn active_generation(
    journal: &Journal,
    work: &Work,
    output: &ResourceOutput,
) -> ModuleResult<u64> {
    let (epoch, session_id, stream_id, generation) = operations::target_parts(&work.target)?;
    let session = journal
        .sessions
        .get(session_id)
        .ok_or("resource_session_missing")?;
    let stream = stream_id
        .and_then(|id| session.streams.get(id))
        .ok_or("resource_stream_missing")?;
    if epoch != journal.host_epoch
        || session.host_epoch != epoch
        || !amaterasu_extension_api::output_in_scope(output, &work.target)
        || !matches!(&output.origin, LifecycleTarget::Session { generation, .. } if *generation == session.generation)
        || stream.generation != generation
        || !journal.receipts.values().any(|receipt| {
            receipt.response["status"] == "pending"
                && receipt.work.as_ref().is_some_and(|stored| {
                    stored.target == work.target
                        && stored.transition_id == work.transition_id
                        && serde_json::to_value(stored).ok() == serde_json::to_value(work).ok()
                })
        })
    {
        return Err("resource_work_stale".into());
    }
    let mut roots = journal.composition.records().filter(|root| {
        root.state == OutputState::Retained
            && root.output.producer == output.producer
            && root.output.output_id == output.output_id
            && root.output.origin == output.origin
            && validate_output_refinement(&root.output, output).is_ok()
    });
    let root = roots.next().ok_or("resource_root_missing")?;
    if roots.next().is_some() {
        return Err("resource_root_ambiguous".into());
    }
    current_root_generation(journal, root)
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::prepare_authority::{Admission, MethodApproval, ProviderSelection};
    use crate::provider_dispatch::{DispatchRecord, DispatchState, ProviderCall};
    use serde_json::json;

    pub(in crate::coordinator) fn fixture() -> (Journal, Work, StoredOutput) {
        let target =
            json!({"kind":"session","host_epoch":"epoch","session_id":"session","generation":1});
        let stream = json!({"kind":"stream","host_epoch":"epoch","session_id":"session","stream_id":"stream","generation":2});
        let session = json!({"host_epoch":"epoch","id":"session","generation":1,"phase":"ready","owner":{"caller":"transport","initiator":"client"},"delegates":[],"application_ref":null,"settings_revision":"revision","application_retired":false,"streams":{"stream":{"generation":2,"phase":"preparing","candidate":null,"required":[],"acknowledgements":[],"retirements":[]}},"resource_generations":{"owner":7}});
        let mut journal: Journal = serde_json::from_value(
            json!({"version":3,"host_epoch":"epoch","receipts":{},"sessions":{"session":session}}),
        )
        .unwrap();
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
        let plan = serde_json::from_value(json!({"plan_id":"plan","revision":1,"resources":[{"provider":"owner","resource_id":"display","incarnation":null,"expected_revision":2,"execution_domain":"seat","mutation_domains":[],"access":"exclusive"}],"mutation_domains":[],"required_capabilities":[],"constraints":{},"obligations":[],"affected_consumers":[],"consumer_change":"live","candidate_constraint":null,"allowed_degradation":[]})).unwrap();
        let output = serde_json::from_value(json!({"output_id":"output","capability":"screen","schema":{"id":"screen/2","major":1,"minor":0,"digest":"a".repeat(64)},"producer":"owner","producer_service":"owner/2","origin":target,"resource":{"provider":"owner","resource_id":"display","incarnation":"native","revision":2,"execution_domain":"seat","mutation_domain":null,"mutation_domains":[]},"data":{}})).unwrap();
        let request = serde_json::from_value(json!({"provider_sequence":1,"operation":"lifecycle_callback","version":2,"transition_id":"prepare","operation_id":"root-call","target":target,"resource_generation":7,"phase":"session.prepare","settings_revision":"revision","settings":{},"intent":null,"reason":null,"requested_video":null,"consumer_requirements":{"execution_domain":"seat","capture_providers":["capture"],"selector_schemas":["selector/1"]},"remaining_ms":1000,"plan_revision":1,"admission_id":"root","approved_plan":plan})).unwrap();
        let terminal = DispatchRecord {
            call: ProviderCall {
                service: "owner/2".into(),
                method: "callback".into(),
                interface_major: 2,
                interface_minor: 0,
                approval: broker,
                arguments: serde_json::to_value(&request).unwrap(),
                timeout_ms: 1000,
                operation_id: "root-call".into(),
            },
            state: DispatchState::Completed {
                payload: json!({"status":"prepared","candidate":null,"obligations":[],"outputs":[output]}),
            },
        };
        let root = StoredOutput {
            output,
            admission_id: "root".into(),
            transition_id: "prepare".into(),
            plan,
            state: OutputState::Retained,
            callback_proof: Some(composition::ProducerCallbackProof {
                participant,
                request,
                terminal,
                pending_receipt: None,
                pending_dispatch: None,
            }),
        };
        let output_selection = composition::OutputSelection {
            producer: "owner".into(),
            service: "owner/2".into(),
            capability: "screen".into(),
            schema: root.output.schema.clone(),
        };
        journal
            .composition
            .record(
                root.clone(),
                &output_selection,
                &root.output.origin,
                &|_| true,
            )
            .unwrap();
        journal.authority.settled.insert(
            "root".into(),
            Admission {
                provider: "owner".into(),
                selection,
                plan: root.plan.clone(),
                rights: vec![],
            },
        );
        let session = journal.sessions.get_mut("session").unwrap();
        session.admission_ids.insert("root".into());
        session
            .admission_targets
            .insert("root".into(), root.output.origin.clone());
        session
            .admission_phases
            .insert("root".into(), LifecyclePhase::SessionPrepare);
        journal
            .admission_transitions
            .insert("root".into(), "prepare".into());
        let work: Work = serde_json::from_value(json!({"phase":"stream.prepare","target":stream,"intent":"initial","reason":null,"video":null,"transition_id":"stream-transition","providers":[],"index":0,"states":{},"candidate":null,"waiting_retirement":false})).unwrap();
        let receipt: Receipt = serde_json::from_value(json!({"caller":"transport","initiator":"client","request":{},"response":{"status":"pending"},"work":work,"wait_target":null})).unwrap();
        journal.receipts.insert("work".into(), receipt);
        (journal, work, root)
    }

    #[test]
    fn historical_root_survives_epoch_rotation_without_live_authority() {
        let (mut journal, work, root) = fixture();
        assert_eq!(historical_generation(&journal, &root).unwrap(), 7);
        assert_eq!(current_root_generation(&journal, &root).unwrap(), 7);
        assert_eq!(active_generation(&journal, &work, &root.output).unwrap(), 7);

        journal.host_epoch = "new-epoch".into();
        assert_eq!(historical_generation(&journal, &root).unwrap(), 7);
        assert!(current_root_generation(&journal, &root).is_err());
        assert!(active_generation(&journal, &work, &root.output).is_err());

        journal
            .composition
            .set_state(
                &root.output.producer,
                &root.output.output_id,
                &root.output.origin,
                OutputState::Released,
            )
            .unwrap();
        let released = journal.composition.records().next().unwrap();
        assert_eq!(historical_generation(&journal, released).unwrap(), 7);
        assert!(current_root_generation(&journal, released).is_err());
    }
    #[test]
    fn released_root_blocks_stream_until_authenticated_replacement() {
        let (mut journal, _, root) = fixture();
        assert!(!released_root_requires_reprepare(
            &journal,
            &journal.sessions["session"]
        ));
        journal
            .composition
            .set_state(
                "owner",
                &root.output.output_id,
                &root.output.origin,
                OutputState::Released,
            )
            .unwrap();
        assert!(released_root_requires_reprepare(
            &journal,
            &journal.sessions["session"]
        ));
        assert!(released_root_requires_reprepare_except(
            &journal,
            &journal.sessions["session"],
            Some(("owner", 7)),
        ));
        journal
            .sessions
            .get_mut("session")
            .unwrap()
            .resource_generations
            .insert("owner".into(), 8);
        assert!(released_root_requires_reprepare(
            &journal,
            &journal.sessions["session"]
        ));
        let mut replacement = root.clone();
        replacement.output.output_id = "new-output".into();
        replacement.output.resource.incarnation = "new-native".into();
        replacement.admission_id = "new-root".into();
        replacement.transition_id = "new-prepare".into();
        let proof = replacement.callback_proof.as_mut().unwrap();
        proof.request.resource_generation = Some(8);
        proof.request.admission_id = Some("new-root".into());
        proof.request.transition_id = "new-prepare".into();
        proof.request.operation_id = "new-call".into();
        proof.terminal.call.operation_id = "new-call".into();
        proof.terminal.call.arguments = serde_json::to_value(&proof.request).unwrap();
        proof.terminal.state = DispatchState::Completed {
            payload: json!({"status":"prepared","candidate":null,"obligations":[],"outputs":[replacement.output]}),
        };
        let selection = composition::OutputSelection {
            producer: "owner".into(),
            service: "owner/2".into(),
            capability: "screen".into(),
            schema: replacement.output.schema.clone(),
        };
        journal
            .composition
            .record(
                replacement.clone(),
                &selection,
                &replacement.output.origin,
                &|_| true,
            )
            .unwrap();
        journal
            .authority
            .settled
            .insert("new-root".into(), journal.authority.settled["root"].clone());
        let session = journal.sessions.get_mut("session").unwrap();
        session.admission_ids.insert("new-root".into());
        session
            .admission_targets
            .insert("new-root".into(), replacement.output.origin.clone());
        session
            .admission_phases
            .insert("new-root".into(), LifecyclePhase::SessionPrepare);
        journal
            .admission_transitions
            .insert("new-root".into(), "new-prepare".into());
        assert!(!released_root_requires_reprepare(
            &journal,
            &journal.sessions["session"]
        ));
        journal
            .sessions
            .get_mut("session")
            .unwrap()
            .resource_generations
            .clear();
        assert!(released_root_requires_reprepare(
            &journal,
            &journal.sessions["session"]
        ));
    }

    #[test]
    fn current_generation_requires_exact_callback_cursor_and_live_work() {
        let (mut journal, work, root) = fixture();
        assert_eq!(historical_generation(&journal, &root).unwrap(), 7);
        assert_eq!(current_root_generation(&journal, &root).unwrap(), 7);
        assert_eq!(active_generation(&journal, &work, &root.output).unwrap(), 7);
        let mut changed_work = work.clone();
        changed_work.transition_id = "other".into();
        assert!(active_generation(&journal, &changed_work, &root.output).is_err());
        let mut foreign_output = root.output.clone();
        foreign_output.resource.incarnation = "other".into();
        assert!(active_generation(&journal, &work, &foreign_output).is_err());
        journal
            .sessions
            .get_mut("session")
            .unwrap()
            .resource_generations
            .insert("owner".into(), 8);
        assert!(current_root_generation(&journal, &root).is_err());
        assert!(active_generation(&journal, &work, &root.output).is_err());
        assert_eq!(historical_generation(&journal, &root).unwrap(), 7);
        let mut forged = root.clone();
        forged
            .callback_proof
            .as_mut()
            .unwrap()
            .request
            .resource_generation = Some(8);
        assert!(historical_generation(&journal, &forged).is_err());
        journal
            .admission_transitions
            .insert("root".into(), "forged".into());
        assert!(historical_generation(&journal, &root).is_err());
    }
}
