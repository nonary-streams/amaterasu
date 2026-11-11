//! Reconcile failed preparation only from the selected provider's scoped cleanup proof.
use super::*;
use crate::prepare_authority::CoordinatorPrepareAuthority;
use crate::prepare_driver::{PrepareAuthority, PreparePhase};
use crate::provider_dispatch::{
    DispatchJournal, DispatchRecord, DispatchState, ProviderDispatcher,
};
use amaterasu_extension_api::{CleanupState, ProviderResponse, ProviderStatusRequest};

pub(super) struct Recovery {
    key: String,
    provider: String,
    original: crate::prepare_driver::PrepareState,
    dispatcher: ProviderDispatcher,
}

struct ReadOnlyJournal;
impl DispatchJournal for ReadOnlyJournal {
    fn persist(&mut self, _: &DispatchRecord) -> ModuleResult<()> {
        // This call only reads a durable receipt. Reissuing a status query after
        // process loss cannot repeat preparation or cleanup effects.
        Ok(())
    }
}

pub(super) fn cleanup_proven(state: &crate::prepare_driver::PrepareState, payload: &Value) -> bool {
    let Some(plan) = &state.plan else {
        return false;
    };
    let Ok(ProviderResponse::Cleanup { outcomes, outputs }) =
        serde_json::from_value(payload.clone())
    else {
        return false;
    };
    let ids: BTreeSet<_> = outcomes
        .iter()
        .map(|outcome| &outcome.obligation_id)
        .collect();
    outputs.is_empty()
        && outcomes.len() == plan.obligations.len()
        && ids.len() == outcomes.len()
        && outcomes.iter().all(|outcome| {
            outcome.state == CleanupState::Released
                && plan
                    .obligations
                    .iter()
                    .any(|obligation| obligation.obligation_id == outcome.obligation_id)
        })
}

impl Coordinator {
    pub(super) fn advance_failed_prepare_recovery(
        &mut self,
        send: &mut dyn FnMut(Command) -> ModuleResult<Response>,
    ) -> ModuleResult<()> {
        if let Some(mut recovery) = self.failed_prepare_recovery.take() {
            let result = recovery.dispatcher.advance(&mut ReadOnlyJournal, send);
            if recovery.dispatcher.has_request() {
                self.failed_prepare_recovery = Some(recovery);
                return Ok(());
            }
            if result.is_ok() {
                if let DispatchState::Completed { payload } = &recovery.dispatcher.record().state {
                    if cleanup_proven(&recovery.original, payload) {
                        self.finish_failed_prepare_recovery(&recovery)?;
                    } else if payload["status"] == "pending" {
                        self.failed_prepare_recovery_attempted
                            .remove(&format!("{}/{}", recovery.key, recovery.provider));
                    }
                    return Ok(());
                }
                if recovery.dispatcher.record().state == DispatchState::Ready {
                    self.failed_prepare_recovery = Some(recovery);
                    return Ok(());
                }
                // No live request and no completed proof means the read could
                // not be confirmed. Preserve the reservation, without replay.
            }
            return Ok(());
        }
        if self.failed_prepare_recovery_scanned
            || self.failed_prepare_recovery_attempted.len() >= 128
            || self.failed_prepare_recovery_polls >= 128
            || Instant::now() < self.failed_prepare_recovery_next_poll
        {
            return Ok(());
        }
        let candidate = {
            let shared = self.shared.borrow();
            let journal = &shared.value;
            journal.receipts.iter().find_map(|(key, receipt)| {
                let work = receipt.work.as_ref()?;
                let failed = work.failed_preparation.as_ref()?;
                let (epoch, session_id, stream, _) = operations::target_parts(&work.target).ok()?;
                let session = journal.sessions.get(session_id)?;
                if epoch == journal.host_epoch
                    || stream.is_some()
                    || work.phase != LifecyclePhase::SessionRelease
                    || work.reason != Some(ReleaseReason::FailedStart)
                    || receipt.request["operation"] != "prepare"
                    || session.application.is_some()
                    || !session.application_retired
                    || !session.streams.is_empty()
                {
                    return None;
                }
                failed.states.iter().find_map(|(provider, state)| {
                    let attempt = format!("{key}/{provider}");
                    if self.failed_prepare_recovery_attempted.contains(&attempt)
                        || state.phase != PreparePhase::RecoveryRequired
                        || state.pending_receipt.as_ref().is_none_or(String::is_empty)
                        || state.dispatch.is_some()
                        || state.request.operation
                            != amaterasu_extension_api::ProviderOperation::LifecycleCallback
                    {
                        return None;
                    }
                    let admission = journal
                        .authority
                        .admissions
                        .get(state.admission_id.as_ref()?)?;
                    if admission.provider != *provider
                        || state.plan.as_ref() != Some(&admission.plan)
                    {
                        return None;
                    }
                    Some((
                        key.clone(),
                        provider.clone(),
                        state.clone(),
                        work.clone(),
                        attempt,
                    ))
                })
            })
        };
        let Some((key, provider, state, work, attempt)) = candidate else {
            // Only prior-epoch failed preparations are eligible. Once exhausted,
            // new activity in this host epoch cannot create another candidate.
            self.failed_prepare_recovery_scanned = true;
            return Ok(());
        };
        self.failed_prepare_recovery_polls += 1;
        self.failed_prepare_recovery_next_poll = Instant::now() + Duration::from_millis(250);
        // Startup discovers provider approvals asynchronously. A missing
        // selection is not a failed recovery and must not fail the host tick.
        if !self.provider_approvals_complete(&provider) {
            return Ok(());
        }
        self.failed_prepare_recovery_attempted.insert(attempt);
        let session_id = operations::target_parts(&work.target)?.1;
        let journal = self.shared.borrow().value.clone();
        let session = &journal.sessions[session_id];
        if recovery::validate_recovery_provider_contracts(
            &journal,
            &self.settings,
            session,
            &work.target,
            work.provider_graph.as_ref(),
            &session.provider_settings,
        )
        .is_err()
        {
            return Ok(());
        }
        let mut authority = CoordinatorPrepareAuthority::new(
            journal.authority.clone(),
            self.work_selections(&work)?,
            providers::AuthorityWriter(self.shared.clone()),
        )?;
        let operation_id = authority.allocate_poll_id(&provider)?;
        let request = ProviderStatusRequest {
            provider_sequence: state.request.provider_sequence,
            operation_id: state.request.operation_id.clone(),
            transition_id: state.request.transition_id.clone(),
            receipt_id: state.pending_receipt.clone().expect("validated receipt"),
        };
        let call = authority.approved_call(
            &state.participant,
            &state.participant.status_method,
            serde_json::to_value(request).map_err(|e| e.to_string())?,
            operation_id,
        )?;
        self.failed_prepare_recovery = Some(Recovery {
            key,
            provider,
            original: state,
            dispatcher: ProviderDispatcher::new(DispatchRecord {
                call,
                state: DispatchState::Ready,
            })?,
        });
        Ok(())
    }

    fn finish_failed_prepare_recovery(&mut self, recovery: &Recovery) -> ModuleResult<()> {
        let mut next = self.shared.borrow().value.clone();
        let work = next
            .receipts
            .get(&recovery.key)
            .and_then(|r| r.work.as_ref())
            .ok_or("recovery_work_missing")?;
        let state = work
            .failed_preparation
            .as_ref()
            .and_then(|f| f.states.get(&recovery.provider))
            .ok_or("recovery_state_missing")?;
        if serde_json::to_value(state).ok() != serde_json::to_value(&recovery.original).ok() {
            return Err("recovery_state_changed".into());
        }
        let id = state
            .admission_id
            .as_ref()
            .ok_or("admission_missing")?
            .clone();
        let admission = next
            .authority
            .admissions
            .get(&id)
            .ok_or("admission_missing")?;
        if admission.provider != recovery.provider
            || state.plan.as_ref() != Some(&admission.plan)
            || next.references_admission(&id)
            || next.authority.output_admissions.contains(&id)
            || next
                .authority
                .control_permits
                .values()
                .any(|p| p.request.admission_id == id)
            || next
                .authority
                .finalize_permits
                .values()
                .any(|p| p.request.admission_id == id)
            || admission
                .rights
                .iter()
                .any(|(domain, _)| next.authority.domain_locks.get(domain) != Some(&id))
        {
            return Err("recovery_admission_still_referenced".into());
        }
        let admission = next
            .authority
            .admissions
            .remove(&id)
            .expect("checked admission");
        next.authority.release_domains(&id, &admission.rights);
        next.admission_transitions.remove(&id);
        for session in next.sessions.values_mut() {
            session.admission_ids.remove(&id);
            session.admission_targets.remove(&id);
            session.admission_phases.remove(&id);
            session.prepared_plan_ids.remove(&id);
        }
        let receipt = next
            .receipts
            .get_mut(&recovery.key)
            .expect("checked receipt");
        let work = receipt.work.as_mut().expect("checked work");
        let failed = work.failed_preparation.as_mut().expect("checked failure");
        let state = failed
            .states
            .get_mut(&recovery.provider)
            .expect("checked provider");
        state.phase = PreparePhase::Rejected;
        state.terminal_dispatch = Some(recovery.dispatcher.record().clone());
        if let DispatchState::Completed { payload } = &recovery.dispatcher.record().state {
            state.response = serde_json::from_value(payload.clone()).ok();
        }
        work.cleanup_blockers.remove(&recovery.provider);
        failed.uncertain = failed
            .states
            .values()
            .any(|s| !matches!(s.phase, PreparePhase::Prepared | PreparePhase::Rejected));
        if !failed.uncertain && work.cleanup_blockers.is_empty() {
            if work.failure.as_deref() == Some("session_cleanup_obligations_unresolved") {
                work.failure = None;
            }
            receipt.response["status"] = json!("pending");
            let session_id = operations::target_parts(&work.target)?.1;
            next.sessions
                .get_mut(session_id)
                .expect("existing session")
                .phase = SessionPhase::Ending;
        }
        save(&self.shared, next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider_dispatch::ProviderCall;

    #[test]
    fn status_query_survives_approval_and_releases_a_rejected_result() {
        let method = json!({"service":"provider/1","method":"status","interface_major":1,"interface_minor":0,"schema_revision":1});
        let original = serde_json::from_value(json!({
            "provider":"provider", "participant":{"provider":"provider","plan_method":method,
                "callback_method":method,"status_method":method,"capabilities":[],"cleanup_methods":[],
                "settings_revision":"settings","settings":{}},
            "request":{"version":1,"provider_sequence":1,"operation":"lifecycle_callback",
                "operation_id":"original","transition_id":"transition",
                "target":{"kind":"session","host_epoch":"old","session_id":"session","generation":1},
                "phase":"session.prepare","settings_revision":"settings","settings":{},"intent":null,
                "reason":null,"requested_video":null,"remaining_ms":1000,"plan_revision":null,
                "admission_id":null,"approved_plan":null,
                "consumer_requirements":{"execution_domain":"desktop","capture_providers":[],"selector_schemas":[]}},
            "phase":"recovery_required","plan":null,"admission_id":null,"pending_receipt":"receipt",
            "status_polls":0,"last_sequence":1,"dispatch":null,"response":null
        })).unwrap();
        let approval = json!({"provider":"provider","method":method});
        let record = DispatchRecord {
            call: ProviderCall {
                service: "provider/1".into(),
                method: "status".into(),
                interface_major: 1,
                interface_minor: 0,
                approval: approval.clone(),
                arguments: json!({}),
                timeout_ms: 1000,
                operation_id: "read-status".into(),
            },
            state: DispatchState::Ready,
        };
        let path =
            std::env::temp_dir().join(format!("amaterasu-rollback-{}.json", identifier().unwrap()));
        std::fs::create_dir(&path).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let mut coordinator = Coordinator::open(&path.join("journal.json")).unwrap();
        coordinator.failed_prepare_recovery = Some(Recovery {
            key: "old".into(),
            provider: "provider".into(),
            original,
            dispatcher: ProviderDispatcher::new(record).unwrap(),
        });
        let mut commands = vec![];
        for _ in 0..4 {
            coordinator
                .advance_failed_prepare_recovery(&mut |command| {
                    let response = match &command {
                        Command::QueryApiApproval { .. } => Response::Result {
                            payload: approval.clone(),
                        },
                        Command::CallApi { .. } => Response::Accepted { id: 42 },
                        Command::TakeResult { .. } => Response::Result {
                            payload: json!({"status":"rejected","reason":"unknown_receipt"}),
                        },
                        Command::ReleaseResult { .. } => Response::Ok,
                        _ => panic!("unexpected status command"),
                    };
                    commands.push(command);
                    Ok(response)
                })
                .unwrap();
        }
        assert_eq!(commands.len(), 4);
        assert!(matches!(
            commands.last(),
            Some(Command::ReleaseResult { request: 42 })
        ));
        assert!(coordinator.failed_prepare_recovery.is_none());
        drop(coordinator);
        std::fs::remove_dir_all(&path).unwrap();
    }
}
