//! Provider-verified reconciliation of failed preparations, so one provider's
//! unresolved outcome cannot fence its mutation domain or other targets.
//!
//! Nothing is ever replayed. Three rules apply, in order:
//!
//! 1. A callback the provider has answered is no longer in flight. Its
//!    admission is finished, releasing the domain lock, while any admitted
//!    obligations stay owed and the target stays `recovery_required`.
//! 2. A participant that only planned had no effect, so a refused plan,
//!    including a release plan refused by a lock, may be planned again.
//! 3. An admitted callback whose outcome was never recorded is reconciled in
//!    a later host epoch: its obligations are recorded as owed and the
//!    provider receives a fresh cleanup callback. Only `Released` retires them.
//! 4. A release whose providers all finished, with no blocker or failure, but
//!    whose completion was refused, is completed again. Completion re-checks
//!    every proof itself, so this only applies corrected validation.
//!
//! Each fenced receipt is re-armed at most once per host epoch, and a failure
//! only leaves that receipt fenced; it never stalls unrelated work.
use super::*;
use crate::prepare_authority::{AuthorityJournal, CoordinatorPrepareAuthority};
use crate::prepare_driver::PreparePhase;
use amaterasu_extension_api::{ProviderOperation, ProviderResponse};

/// Holds an authority transition so it commits with the reconciled work.
struct CapturedAuthority(Rc<RefCell<Option<AuthorityState>>>);

impl AuthorityJournal for CapturedAuthority {
    fn persist(&mut self, state: &AuthorityState) -> ModuleResult<()> {
        *self.0.borrow_mut() = Some(state.clone());
        Ok(())
    }
}

fn unsettled(state: &PrepareState) -> bool {
    !matches!(state.phase, PreparePhase::Prepared | PreparePhase::Rejected)
}

/// The exact admitted plan of an effect callback.
fn admitted_callback(
    state: &PrepareState,
) -> Option<(&str, &amaterasu_extension_api::ResourcePlan)> {
    if state.request.operation != ProviderOperation::LifecycleCallback {
        return None;
    }
    let admission = state.admission_id.as_deref()?;
    Some((admission, state.plan.as_ref()?))
}

/// The provider returned a terminal answer to this callback; nothing remains
/// in flight. A lost dispatch keeps no callback answer and stays locked.
fn answered_callback(state: &PrepareState) -> bool {
    state.phase == PreparePhase::RecoveryRequired
        && admitted_callback(state).is_some()
        && matches!(
            state.response,
            Some(
                ProviderResponse::RecoveryRequired { .. }
                    | ProviderResponse::Cleanup { .. }
                    | ProviderResponse::Failed { .. }
                    | ProviderResponse::Rejected { .. }
            )
        )
}

/// How a release participant stands after the failed cleanup pass.
enum ReleaseStanding {
    Settled,
    /// Skipped behind a blocker, or its read-only plan was refused.
    Retry,
    Unresolved,
}

fn release_standing(work: &Work, provider: &str) -> ReleaseStanding {
    match work.states.get(provider) {
        None => ReleaseStanding::Retry,
        Some(state) if state.phase == PreparePhase::CleanupSettled => ReleaseStanding::Settled,
        Some(state) if !state.effect_unsettled() && state.admission_id.is_none() => {
            ReleaseStanding::Retry
        }
        Some(_) => ReleaseStanding::Unresolved,
    }
}

/// A fenced failed release that can be safely re-armed in this host epoch.
/// It may follow a failed preparation or be an ordinary pause or end.
fn reconcilable(receipt: &Receipt, journal: &Journal) -> bool {
    let Some(work) = &receipt.work else {
        return false;
    };
    let Ok((epoch, _, _, _)) = operations::target_parts(&work.target) else {
        return false;
    };
    let failed = work.failed_preparation.as_ref();
    let prepare_states = || failed.into_iter().flat_map(|failed| failed.states.iter());
    let unsettled_providers: BTreeSet<_> = prepare_states()
        .filter(|(_, state)| unsettled(state))
        .map(|(provider, _)| provider)
        .collect();
    let owed_ready = prepare_states()
        .filter(|(_, state)| state.effect_unsettled())
        .all(|(_, state)| {
            admitted_callback(state).is_some_and(|(admission, _)| {
                if failed.is_some_and(|failed| failed.uncertain) {
                    epoch != journal.host_epoch
                } else {
                    // Converted by an earlier reconciliation; no lock remains.
                    !journal.authority.admissions.contains_key(admission)
                }
            })
        });
    let blocker_retryable = |provider: &String| {
        (unsettled_providers.contains(provider) && !work.states.contains_key(provider))
            || (work.providers.contains(provider)
                && matches!(release_standing(work, provider), ReleaseStanding::Retry))
    };
    receipt.response["status"] == "recovery_required"
        && matches!(
            work.phase,
            LifecyclePhase::StreamRelease | LifecyclePhase::SessionRelease
        )
        && work.index == work.providers.len()
        && work.provider_graph.is_some()
        && work.join_context.is_none()
        && work.reprepare.is_none()
        && work.action_transfer.is_none()
        && !work.cleanup_blockers.is_empty()
        && work.cleanup_blockers.iter().all(blocker_retryable)
        && work.providers.iter().all(|provider| {
            !matches!(
                release_standing(work, provider),
                ReleaseStanding::Unresolved
            )
        })
        && owed_ready
}

/// Rule 4: every release participant settled and nothing is blocked; only the
/// completion step was refused.
fn finished_release_refused(receipt: &Receipt) -> bool {
    let Some(work) = &receipt.work else {
        return false;
    };
    receipt.response["status"] == "recovery_required"
        && matches!(
            work.phase,
            LifecyclePhase::StreamRelease | LifecyclePhase::SessionRelease
        )
        && work.index == work.providers.len()
        && work.cleanup_blockers.is_empty()
        && work.failure.is_none()
        && work
            .failed_preparation
            .as_ref()
            .is_none_or(|failed| !failed.uncertain)
        && work
            .providers
            .iter()
            .all(|provider| matches!(release_standing(work, provider), ReleaseStanding::Settled))
}

/// Dependents release before their prerequisites: reverse preparation order.
fn release_order(work: &Work, providers: &BTreeSet<String>) -> Vec<String> {
    let graph = work.provider_graph.as_ref().expect("reconcilable graph");
    graph
        .order
        .iter()
        .rev()
        .filter(|provider| providers.contains(*provider))
        .cloned()
        .collect()
}

impl Coordinator {
    /// Run one bounded reconciliation pass. Failures are logged and fence only
    /// the receipt or admission concerned.
    pub(super) fn reconcile_uncertain_preparation(&mut self) {
        if !self.approvals_loaded() {
            return;
        }
        self.release_answered_locks();
        self.revalidate_one_finished_release();
        self.rearm_one_failed_release();
    }

    /// Every participant's lifecycle methods have been approved this epoch.
    fn approvals_loaded(&self) -> bool {
        self.settings.participants.iter().all(|participant| {
            self.selections
                .get(&participant.provider)
                .is_some_and(|selection| {
                    [
                        &participant.plan_method,
                        &participant.callback_method,
                        &participant.status_method,
                    ]
                    .into_iter()
                    .chain(participant.binding_validate_method.as_ref())
                    .all(|method| {
                        selection
                            .approvals
                            .iter()
                            .any(|approval| &approval.method == method)
                    })
                })
        })
    }

    /// Rule 1: finish admissions of callbacks the provider already answered.
    fn release_answered_locks(&mut self) {
        let answered: Vec<(String, String, String)> = {
            let journal = &self.shared.borrow().value;
            journal
                .receipts
                .iter()
                .filter(|(_, receipt)| receipt.response["status"] != "pending")
                .filter_map(|(key, receipt)| Some((key, receipt.work.as_ref()?)))
                .flat_map(|(key, work)| {
                    let failed = work
                        .failed_preparation
                        .iter()
                        .flat_map(|failed| failed.states.values());
                    work.states
                        .values()
                        .chain(failed)
                        .filter(|state| answered_callback(state))
                        .filter_map(|state| {
                            let (admission, _) = admitted_callback(state)?;
                            journal
                                .authority
                                .admissions
                                .contains_key(admission)
                                .then(|| (key.clone(), state.provider.clone(), admission.into()))
                        })
                        .collect::<Vec<_>>()
                })
                .collect()
        };
        for (key, provider, admission) in answered {
            let fence = format!("admission:{admission}");
            if self.unreconcilable.contains(&fence) {
                continue;
            }
            if let Err(reason) = self.finish_answered_admission(&key, &provider, &admission) {
                eprintln!(
                    "lifecycle answered admission release deferred provider={provider} reason={reason}"
                );
                self.unreconcilable.insert(fence);
            }
        }
    }

    fn finish_answered_admission(
        &self,
        key: &str,
        provider: &str,
        admission: &str,
    ) -> ModuleResult<()> {
        let work = self.shared.borrow().value.receipts[key]
            .work
            .clone()
            .ok_or("answered_work_missing")?;
        self.authority_for_work(&work)?
            .finish_admission(provider, admission)
    }

    /// Rule 4: complete one refused finished release again. It needs no quiet
    /// coordinator: completion takes no admission and may unblock others.
    fn revalidate_one_finished_release(&mut self) {
        let key = {
            let journal = &self.shared.borrow().value;
            journal
                .receipts
                .iter()
                .find(|(key, receipt)| {
                    !self.unreconcilable.contains(*key) && finished_release_refused(receipt)
                })
                .map(|(key, _)| key.clone())
        };
        let Some(key) = key else {
            return;
        };
        self.unreconcilable.insert(key.clone());
        let mut next = self.shared.borrow().value.clone();
        let receipt = next.receipts.get_mut(&key).expect("finished release");
        receipt.response["status"] = json!("pending");
        receipt.response["reason"] = json!("revalidating_release_completion");
        if let Err(reason) = save(&self.shared, next) {
            eprintln!("lifecycle release revalidation deferred reason={reason}");
        }
    }

    /// Rules 2 and 3: re-arm at most one fenced failed release, only while no
    /// other work is pending, so a retry never contends with live admissions.
    fn rearm_one_failed_release(&mut self) {
        // The failed-preparation status read reconciles the same receipts
        // from the provider's own receipt. Never re-arm underneath it.
        if self.failed_prepare_recovery.is_some() {
            return;
        }
        let key = {
            let journal = &self.shared.borrow().value;
            let quiet = journal
                .receipts
                .values()
                .all(|receipt| receipt.response["status"] != "pending" || receipt.work.is_none());
            if !quiet {
                return;
            }
            journal
                .receipts
                .iter()
                .find(|(key, receipt)| {
                    !self.unreconcilable.contains(*key) && reconcilable(receipt, journal)
                })
                .map(|(key, _)| key.clone())
        };
        let Some(key) = key else {
            return;
        };
        // One attempt per host epoch; a repeated conflict waits for restart.
        self.unreconcilable.insert(key.clone());
        if let Err(reason) = self
            .reconciled_journal(&key)
            .and_then(|next| save(&self.shared, next))
        {
            eprintln!("lifecycle preparation reconciliation deferred reason={reason}");
        }
    }

    /// Build the reconciled journal on a private clone; nothing is persisted
    /// unless every owed obligation is recorded and the release re-armed.
    fn reconciled_journal(&self, key: &str) -> ModuleResult<Journal> {
        let mut next = self.shared.borrow().value.clone();
        let mut work = next.receipts[key].work.clone().expect("reconcilable work");
        let (_, session_id, stream_id, _) = operations::target_parts(&work.target)?;
        let mut retry = self.record_owed_cleanup(&mut next, &work)?;
        let completed: Vec<_> = work
            .providers
            .iter()
            .filter(|provider| {
                matches!(release_standing(&work, provider), ReleaseStanding::Settled)
            })
            .cloned()
            .collect();
        retry.extend(
            work.providers
                .iter()
                .filter(|provider| !completed.contains(provider))
                .cloned(),
        );
        // Refused plans had no effect; they are planned again from scratch.
        work.states
            .retain(|provider, _| completed.contains(provider));
        work.index = completed.len();
        work.providers = completed;
        work.providers.extend(release_order(&work, &retry));
        work.cleanup_blockers.clear();
        work.failure = None;
        work.recovery_authorized_epoch = Some(next.host_epoch.clone());
        if let Some(failed) = &mut work.failed_preparation {
            failed.uncertain = false;
        }
        let session = next.sessions.get_mut(session_id).ok_or("unknown_session")?;
        match stream_id {
            Some(stream_id) => {
                let stream = session.streams.get_mut(stream_id).ok_or("unknown_stream")?;
                stream.phase = StreamPhase::Quiescing;
                stream.retirements = stream.required.clone();
                session.phase = SessionPhase::Paused;
            }
            None => {
                session.phase = SessionPhase::Ending;
                session.application_retired = session.application.is_none();
            }
        }
        let receipt = next.receipts.get_mut(key).expect("reconcilable receipt");
        receipt.response["status"] = json!("pending");
        receipt.response["reason"] = json!("reconciling_failed_preparation");
        receipt.response["cleanup_pending"] = json!(true);
        receipt.work = Some(work);
        Ok(next)
    }

    /// Rule 3: record each unsettled callback's admitted obligations as owed,
    /// finishing its admission. Returns the providers that need cleanup.
    fn record_owed_cleanup(
        &self,
        next: &mut Journal,
        work: &Work,
    ) -> ModuleResult<BTreeSet<String>> {
        let Some(failed) = &work.failed_preparation else {
            return Ok(BTreeSet::new());
        };
        let selections = self.work_selections(work)?;
        let (_, session_id, _, _) = operations::target_parts(&work.target)?;
        let mut cleanup = BTreeSet::new();
        for (provider, state) in &failed.states {
            if !state.effect_unsettled() {
                continue;
            }
            let (admission, plan) =
                admitted_callback(state).ok_or("unsettled_effect_not_reconcilable")?;
            if next.authority.admissions.contains_key(admission) {
                let captured = Rc::new(RefCell::new(None));
                let mut authority = CoordinatorPrepareAuthority::new(
                    next.authority.clone(),
                    selections.clone(),
                    CapturedAuthority(captured.clone()),
                )?;
                authority.finish_admission(provider, admission)?;
                next.authority = captured
                    .borrow_mut()
                    .take()
                    .ok_or("reconciled_authority_missing")?;
            }
            let session = next.sessions.get_mut(session_id).ok_or("unknown_session")?;
            record_owed_obligations(session, &plan.obligations)?;
            if next.authority.settled.contains_key(admission) {
                cleanup.insert(provider.clone());
            }
        }
        Ok(cleanup)
    }
}

/// Record an admitted plan's obligations exactly as a prepared response would.
fn record_owed_obligations(
    session: &mut SessionRecord,
    obligations: &[amaterasu_extension_api::ResourceObligation],
) -> ModuleResult<()> {
    for obligation in obligations {
        match session
            .obligations
            .iter()
            .find(|owed| owed.obligation_id == obligation.obligation_id)
        {
            Some(owed) if owed != obligation => {
                return Err("obligation_identity_conflict".into());
            }
            Some(_) => {}
            None => session.obligations.push(obligation.clone()),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape left on hosts by a join refused at processing preflight
    /// before closing evidence accepted that case.
    fn refused_join_release() -> Receipt {
        let target = json!({"kind":"stream","host_epoch":"old","session_id":"session",
            "stream_id":"join","generation":1});
        let work = json!({"phase":"stream.release","target":target,"intent":"join","reason":"failed_start",
            "video":null,"transition_id":"release","providers":[],"index":0,"states":{},"candidate":null,
            "waiting_retirement":false,"cleanup_blockers":[],
            "failed_preparation":{"reason":"processing_output_range_mismatch","uncertain":false,"states":{}}});
        Receipt {
            caller: "amaterasu.rtsp".into(),
            initiator: "amaterasu.rtsp".into(),
            request: json!({"operation":"prepare_stream","operation_id":"join"}),
            response: json!({"status":"recovery_required","reason":"join_closed_evidence_unverified"}),
            work: Some(serde_json::from_value(work).unwrap()),
            wait_target: None,
        }
    }

    #[test]
    fn refused_completion_of_a_finished_release_is_revalidated() {
        assert!(finished_release_refused(&refused_join_release()));
    }

    #[test]
    fn unfinished_blocked_or_uncertain_releases_are_not_revalidated() {
        let mut blocked = refused_join_release();
        blocked
            .work
            .as_mut()
            .unwrap()
            .cleanup_blockers
            .insert("provider".into());
        let mut uncertain = refused_join_release();
        uncertain
            .work
            .as_mut()
            .unwrap()
            .failed_preparation
            .as_mut()
            .unwrap()
            .uncertain = true;
        let mut unfinished = refused_join_release();
        unfinished
            .work
            .as_mut()
            .unwrap()
            .providers
            .push("provider".into());
        let mut pending = refused_join_release();
        pending.response["status"] = json!("pending");
        for receipt in [blocked, uncertain, unfinished, pending] {
            assert!(!finished_release_refused(&receipt));
        }
    }
}
