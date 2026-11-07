//! Resume an interrupted, undispatched end only after independently verified
//! stream and application retirement. Historical identity never admits new work.
use super::*;

fn untouched_session_release(receipt: &Receipt, work: &Work, session: &SessionRecord) -> bool {
    receipt.request["operation"] == "end_session"
        && receipt.request["target"] == json!(work.target)
        && receipt.response["status"] == "recovery_required"
        && receipt.wait_target.is_none()
        && work.phase == LifecyclePhase::SessionRelease
        && work.reason == Some(ReleaseReason::End)
        && work.waiting_retirement
        && work.index == 0
        && work.states.is_empty()
        && work.candidate.is_none()
        && work.composition_progress.is_none()
        && work.owner_observations.is_empty()
        && !work.compacted_release
        && work.reprepare.is_none()
        && work.failed_preparation.is_none()
        && work.action_transfer.is_none()
        && work.failure.is_none()
        && work.cleanup_blockers.is_empty()
        && work.provider_graph == session.provider_graph
        && work.provider_graph.as_ref().is_some_and(|graph| {
            work.providers == graph.order.iter().rev().cloned().collect::<Vec<_>>()
                && graph.dependencies.len() == graph.order.len()
                && graph.order.iter().enumerate().all(|(index, provider)| {
                    !graph.order[..index].contains(provider)
                        && graph
                            .dependencies
                            .get(provider)
                            .is_some_and(|dependencies| {
                                dependencies
                                    .iter()
                                    .all(|dependency| graph.order[..index].contains(dependency))
                            })
                })
        })
}

fn exact_child_retired(
    journal: &Journal,
    session: &SessionRecord,
    id: &str,
    stream: &StreamRecord,
) -> bool {
    let target = LifecycleTarget::Stream {
        host_epoch: session.host_epoch.clone(),
        session_id: session.id.clone(),
        stream_id: id.into(),
        generation: stream.generation,
    };
    if stream.phase != StreamPhase::Retired
        || stream.required != stream.retirements
        || journal
            .inputs
            .has_unresolved_stream(&session.host_epoch, &session.id, id)
        || journal
            .actions
            .has_unresolved_stream(&session.host_epoch, &session.id, id)
        || journal
            .input_bindings
            .values()
            .any(|binding| binding.session_id == session.id && binding.stream_id == id)
        || join::awaiting_shared_release(journal, &target)
    {
        return false;
    }
    // A verified composition checkpoint can replace the original receipt. For
    // v1 callbacks, retain the exact authenticated coordinator terminal receipt.
    if journal.verified_released_output(&target).is_some()
        || join::subscriber_resources_released(journal, &target)
    {
        return true;
    }
    journal.receipts.values().any(|receipt| {
        matches!(
            receipt.request["operation"].as_str(),
            Some("pause_stream" | "capture_invalidated" | "resource_invalidated" | "retire_stream")
        ) && receipt.request["target"] == json!(target)
            && receipt.work.is_none()
            && receipt.wait_target.is_none()
            && receipt.response["transition_id"]
                .as_str()
                .is_some_and(valid_id)
            && amaterasu_extension_api::stream_retirement_proof(&receipt.response, &target, None)
                .is_ok_and(|proof| proof.consumer_retired && proof.resources_released)
    })
}

fn release_preflight(
    journal: &Journal,
    settings: &LifecycleSettings,
    original_key: &str,
) -> ModuleResult<()> {
    let receipt = journal
        .receipts
        .get(original_key)
        .ok_or("unknown_original_operation")?;
    let work = receipt.work.as_ref().ok_or("recovery_work_unavailable")?;
    let (epoch, id, None, generation) = operations::target_parts(&work.target)? else {
        return Err("session_target_required".into());
    };
    let session = journal.sessions.get(id).ok_or("unknown_session")?;
    if epoch == journal.host_epoch
        || session.host_epoch != epoch
        || session.generation != generation
        || !untouched_session_release(receipt, work, session)
    {
        return Err("session_release_not_recoverable".into());
    }
    recovery::validate_recovery_provider_contracts(
        journal,
        settings,
        session,
        &work.target,
        session.provider_graph.as_ref(),
        &session.provider_settings,
    )?;
    if journal.inputs.has_unresolved_session(epoch, id)
        || journal.actions.has_unresolved_session(epoch, id)
        || journal.receipts.iter().any(|(key, receipt)| {
            key != original_key
                && (receipt.work.as_ref().is_some_and(|other| {
                    operations::target_parts(&other.target)
                        .is_ok_and(|(_, other_id, _, _)| other_id == id)
                }) || receipt.wait_target.as_ref().is_some_and(|target| {
                    operations::target_parts(target).is_ok_and(|(_, other_id, _, _)| other_id == id)
                }))
        })
        || session
            .streams
            .iter()
            .any(|(id, stream)| !exact_child_retired(journal, session, id, stream))
    {
        return Err("session_consumers_not_retired".into());
    }
    Ok(())
}

impl Coordinator {
    pub(super) fn recover_undispatched_session_release(
        &mut self,
        caller: &str,
        initiator: &str,
        payload: Value,
        key: String,
        original_key: &str,
    ) -> ModuleResult<Value> {
        let mut next = self.shared.borrow().value.clone();
        release_preflight(&next, &self.settings, original_key)?;
        let work = next.receipts[original_key]
            .work
            .as_ref()
            .expect("validated release");
        let (_, id, None, _) = operations::target_parts(&work.target)? else {
            unreachable!()
        };
        let session = &next.sessions[id];
        if session.owner.caller != caller || session.owner.initiator != initiator {
            return Err("permission_denied".into());
        }
        if payload["expected_host_epoch"] != next.host_epoch
            || payload["target"] != json!(work.target)
        {
            return Err("stale_recovery_target".into());
        }
        let target = work.target.clone();
        let transition = work.transition_id.clone();
        let application_proven = application::valid_retirement_proof(session, &target);
        if session.application_retired && !application_proven {
            return Err("application_retirement_unconfirmed".into());
        }
        let desktop = session.application.is_none()
            && session.application_operation_id.is_none()
            && session.application_ref["kind"] == "desktop";
        let job = if application_proven || desktop {
            None
        } else {
            if self.application_jobs.len() >= 128 {
                return Err("application_capacity_exhausted".into());
            }
            Some(application::ApplicationJob::for_session_recovery(
                key.clone(),
                target.clone(),
                session,
                original_key.into(),
                transition,
                next.host_epoch.clone(),
            )?)
        };
        let session_id = id.to_owned();
        let epoch = next.host_epoch.clone();
        let receipt = next
            .receipts
            .get_mut(original_key)
            .expect("validated release");
        receipt.response["status"] = json!("pending");
        receipt.response["reason"] = json!("reconciliation_in_progress");
        receipt
            .work
            .as_mut()
            .expect("validated release")
            .recovery_authorized_epoch = Some(epoch);
        let session = next
            .sessions
            .get_mut(&session_id)
            .expect("validated session");
        session.phase = SessionPhase::Ending;
        if desktop && !application_proven {
            session.application_retired = true;
            session.application_retirement_proof =
                Some(application::ApplicationRetirementProof::Desktop {
                    target: target.clone(),
                });
        }
        let response = json!({"status":if job.is_some() {"pending"} else {"succeeded"},
            "operation_id":payload["operation_id"],"target":target,
            "reconciliation_started":true,"original_operation_id":payload["original_operation_id"]});
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
        save(&self.shared, next)?;
        if let Some(job) = job {
            self.application_jobs.insert(key, job);
        }
        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (Journal, LifecycleSettings) {
        let method = |name| json!({"service":"provider/1","method":name,"interface_major":1,"interface_minor":0,"schema_revision":1});
        let participant = json!({"provider":"provider","plan_method":method("plan"),"callback_method":method("callback"),
            "status_method":method("status"),"capabilities":[],"cleanup_methods":[],"settings_revision":"current","settings":{"value":"new"}});
        let settings: LifecycleSettings =
            serde_json::from_value(json!({"participants":[participant]})).unwrap();
        let target =
            json!({"kind":"session","host_epoch":"old","session_id":"session","generation":1});
        let graph = json!({"order":["provider"],"dependencies":{"provider":[]}});
        let session = json!({"host_epoch":"old","id":"session","generation":1,"phase":"recovery_required",
            "owner":{"caller":"owner","initiator":"owner"},"delegates":[],"application_ref":{"kind":"desktop"},
            "settings_revision":"one","application_retired":false,"streams":{},"provider_graph":graph,
            "provider_settings":{"provider":{"settings":{"value":"old"},"revision":"frozen"}}});
        let work = json!({"phase":"session.release","target":target,"reason":"end","intent":null,"video":null,
            "transition_id":"transition","providers":["provider"],"provider_graph":graph,"index":0,"states":{},
            "candidate":null,"waiting_retirement":true});
        let mut journal:Journal = serde_json::from_value(json!({"version":3,"host_epoch":"current","sessions":{"session":session},
            "receipts":{"end":{"caller":"owner","initiator":"owner","request":{"operation":"end_session","operation_id":"end","target":target},
                "response":{"status":"recovery_required"},"work":work}}})).unwrap();
        let contract = settings_snapshot::FrozenParticipantContract::from_participant(
            &settings.participants[0],
        )
        .unwrap();
        journal
            .sessions
            .get_mut("session")
            .unwrap()
            .provider_settings
            .get_mut("provider")
            .unwrap()
            .contract = Some(contract);
        (journal, settings)
    }

    #[test]
    fn recovery_requires_frozen_settings_and_unchanged_provider_graph() {
        let (journal, mut settings) = fixture();
        assert!(release_preflight(&journal, &settings, "end").is_ok());
        let mut missing = journal.clone();
        missing
            .sessions
            .get_mut("session")
            .unwrap()
            .provider_settings
            .clear();
        assert_eq!(
            release_preflight(&missing, &settings, "end").unwrap_err(),
            "recovery_settings_snapshot_missing"
        );
        settings.participants[0].provider = "replacement".into();
        assert_eq!(
            release_preflight(&journal, &settings, "end").unwrap_err(),
            "recovery_provider_graph_changed"
        );
    }

    #[test]
    fn matching_snapshot_cannot_override_a_conflicting_exact_prepare_contract() {
        let (mut journal, settings) = fixture();
        let target = journal.receipts["end"]
            .work
            .as_ref()
            .unwrap()
            .target
            .clone();
        let mut original = settings.participants[0].clone();
        original
            .capabilities
            .insert("historical_capability/1".into());
        let approvals: Vec<_> = [
            &original.plan_method,
            &original.callback_method,
            &original.status_method,
        ]
        .into_iter()
        .map(|method| {
            json!({"method":method,
                "broker_approval":{"provider":original.provider,"method":method}})
        })
        .collect();
        let admission = serde_json::from_value(json!({"provider":original.provider,
            "selection":{"participant":original,"approvals":approvals,"capabilities":original.capabilities,
                "cleanup_methods":[],"authorized_borrowers":[]},
            "plan":{"plan_id":"original-plan","revision":1,"resources":[],"mutation_domains":[],
                "required_capabilities":[],"constraints":{},"obligations":[],"affected_consumers":[],
                "consumer_change":"live","candidate_constraint":null,"allowed_degradation":[]},
            "rights":[]})).unwrap();
        journal
            .authority
            .settled
            .insert("original-admission".into(), admission);
        let session = journal.sessions.get_mut("session").unwrap();
        session.admission_ids.insert("original-admission".into());
        session
            .admission_targets
            .insert("original-admission".into(), target);
        session
            .admission_phases
            .insert("original-admission".into(), LifecyclePhase::SessionPrepare);
        assert_eq!(
            release_preflight(&journal, &settings, "end").unwrap_err(),
            "recovery_provider_contract_changed"
        );
    }

    #[test]
    fn recovery_never_replays_a_release_that_reached_provider_dispatch() {
        let (mut journal, settings) = fixture();
        journal
            .receipts
            .get_mut("end")
            .unwrap()
            .work
            .as_mut()
            .unwrap()
            .index = 1;
        assert_eq!(
            release_preflight(&journal, &settings, "end").unwrap_err(),
            "session_release_not_recoverable"
        );
    }

    #[test]
    fn retired_phase_without_exact_child_proof_cannot_authorize_session_cleanup() {
        let (mut journal, settings) = fixture();
        journal.sessions.get_mut("session").unwrap().streams.insert("child".into(), serde_json::from_value(json!({
            "generation":1,"phase":"retired","candidate":null,"required":["capture"],"retirements":["capture"],
            "acknowledgements":["capture"],"provider_graph":{"order":["provider"],"dependencies":{"provider":[]}}
        })).unwrap());
        assert_eq!(
            release_preflight(&journal, &settings, "end").unwrap_err(),
            "session_consumers_not_retired"
        );
        let target = json!({"kind":"stream","host_epoch":"old","session_id":"session","stream_id":"child","generation":1});
        journal.receipts.insert("consumer-waiter".into(), serde_json::from_value(json!({
            "caller":"capture","initiator":"capture", "request":{"operation":"retire_stream","operation_id":"waiter","target":target},
            "response":{"status":"succeeded","operation_id":"waiter","target":target}
        })).unwrap());
        assert_eq!(
            release_preflight(&journal, &settings, "end").unwrap_err(),
            "session_consumers_not_retired",
            "consumer waiter completion cannot replace the provider release transition"
        );
    }
}
