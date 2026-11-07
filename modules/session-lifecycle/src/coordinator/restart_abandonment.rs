//! Owner abandonment of a session interrupted by a host restart.
//!
//! A session owner may keep its application records only in memory. After a
//! restart it can never report that application retired, so the session would
//! remain `RecoveryRequired` forever. Abandonment records the application as
//! untracked (not retired) and resumes the session's exact, undispatched
//! release work through the normal scheduler. Provider cleanup results remain
//! authoritative: nothing here settles an obligation or claims retirement.
use super::*;

const UNTRACKED_APPLICATION: &str = "untracked_after_restart";
const SUPERSEDED_REASON: &str = "superseded_by_restart_abandonment";

/// Durable reason a session may release without an application retirement
/// proof. It never substitutes for that proof in a closed-session certificate.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum ApplicationDisposition {
    /// The owner lost track of the application when the host restarted.
    UntrackedAfterRestart {
        /// Host epoch in which the session and its application were created.
        abandoned_epoch: String,
        /// Host epoch in which the owner abandoned the session.
        recorded_epoch: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        application_operation_id: Option<String>,
    },
}

impl ApplicationDisposition {
    fn untracked_after_restart(session: &SessionRecord, current_epoch: &str) -> Self {
        Self::UntrackedAfterRestart {
            abandoned_epoch: session.host_epoch.clone(),
            recorded_epoch: current_epoch.to_owned(),
            application_operation_id: session.application_operation_id.clone(),
        }
    }

    /// A disposition belongs only to an old-epoch session that has no
    /// retirement claim, and must agree with that session's own identity.
    pub(super) fn valid_for(&self, session: &SessionRecord, current_epoch: &str) -> bool {
        let Self::UntrackedAfterRestart {
            abandoned_epoch,
            recorded_epoch,
            application_operation_id,
        } = self;
        session.host_epoch != current_epoch
            && !session.application_retired
            && session.application_retirement_proof.is_none()
            && *abandoned_epoch == session.host_epoch
            && valid_id(recorded_epoch)
            && recorded_epoch != abandoned_epoch
            && *application_operation_id == session.application_operation_id
    }
}

/// Annotate the terminal response of an abandoned session's release. Returns
/// true when deferred restoration fenced the session for recovery instead of
/// retiring it; the caller must then retain the release work as evidence.
pub(super) fn finish_abandoned_release(
    session: &mut SessionRecord,
    deferred: bool,
    response: &mut Value,
) -> bool {
    if session.application_disposition.is_none() {
        return false;
    }
    response["application_retired"] = json!(false);
    response["application"] = json!(UNTRACKED_APPLICATION);
    if !deferred {
        return false;
    }
    session.phase = SessionPhase::RecoveryRequired;
    response["status"] = json!("recovery_required");
    response["reason"] = json!("restart_abandonment_restoration_deferred");
    true
}

impl Coordinator {
    /// The session owner abandons a restart-interrupted session it can no
    /// longer track. The frozen release graph runs as recovery-authorized work;
    /// any rejected, deferred or uncertain cleanup returns it to recovery.
    pub(super) fn abandon_restarted_session(
        &mut self,
        caller: &str,
        initiator: &str,
        payload: Value,
        key: String,
    ) -> ModuleResult<Value> {
        let mut next = self.shared.borrow().value.clone();
        let target: LifecycleTarget =
            serde_json::from_value(payload.get("target").cloned().ok_or("target_required")?)
                .map_err(|_| "invalid_target")?;
        let (session_id, generation) = current_session_target(&target, &next.host_epoch)?;
        let session = next.sessions.get(session_id).ok_or("unknown_session")?;
        if session.owner.caller != caller || session.owner.initiator != initiator {
            return Err("permission_denied".into());
        }
        require_abandonable_session(&next, session, generation)?;
        require_no_live_session_state(&next, session)?;
        require_known_obligation_owners(&next, session)?;
        let release_key = retained_release_key(&next, session)?;
        let abandoned_target = session_target(session);
        let mut work = supersede_release_receipt(&mut next, &release_key)?;
        work.recovery_authorized_epoch = Some(next.host_epoch.clone());
        record_untracked_application(&mut next, session_id)?;
        let response = json!({"status":"pending","operation_id":payload["operation_id"],
            "target":target,"abandoned_target":abandoned_target,"transition_id":work.transition_id,
            "application_retired":false,"application":UNTRACKED_APPLICATION});
        next.receipts.insert(
            key,
            Receipt {
                caller: caller.into(),
                initiator: initiator.into(),
                request: payload,
                response: response.clone(),
                work: Some(work),
                wait_target: None,
            },
        );
        save(&self.shared, next)?;
        Ok(response)
    }
}

/// Like `recover_session`, the request names the current epoch so a stale
/// owner cannot replay it after another restart.
fn current_session_target<'a>(
    target: &'a LifecycleTarget,
    current_epoch: &str,
) -> ModuleResult<(&'a str, u64)> {
    let (epoch, session_id, stream, generation) = operations::target_parts(target)?;
    if stream.is_some() || epoch != current_epoch {
        return Err("stale_abandonment_target".into());
    }
    Ok((session_id, generation))
}

fn session_target(session: &SessionRecord) -> LifecycleTarget {
    LifecycleTarget::Session {
        host_epoch: session.host_epoch.clone(),
        session_id: session.id.clone(),
        generation: session.generation,
    }
}

fn require_abandonable_session(
    journal: &Journal,
    session: &SessionRecord,
    generation: u64,
) -> ModuleResult<()> {
    if session.generation != generation
        || session.host_epoch == journal.host_epoch
        || session.phase != SessionPhase::RecoveryRequired
    {
        return Err("session_not_abandonable".into());
    }
    if session
        .streams
        .values()
        .any(|stream| stream.phase != StreamPhase::Retired)
    {
        return Err("streams_not_retired".into());
    }
    if session.application_retired || session.application_retirement_proof.is_some() {
        return Err("application_already_retired".into());
    }
    Ok(())
}

/// Inputs, actions, shared sources and composition outputs each have their own
/// release path; abandonment must not run session cleanup underneath them.
fn require_no_live_session_state(journal: &Journal, session: &SessionRecord) -> ModuleResult<()> {
    let (epoch, id) = (session.host_epoch.as_str(), session.id.as_str());
    if journal.inputs.has_unresolved_session(epoch, id)
        || journal.actions.has_unresolved_session(epoch, id)
        || journal
            .input_bindings
            .values()
            .any(|binding| binding.session_id == id)
    {
        return Err("session_inputs_unresolved".into());
    }
    if join::session_has_shared_lease(journal, id)
        || retention::has_composition_output(journal, id, None)
    {
        return Err("session_resources_retained".into());
    }
    Ok(())
}

/// Every retained admission must be settled and owned by this exact session
/// target, so the session release callback can retire it. An obligation with
/// no retained admission anywhere was already confirmed released by its own
/// stream cleanup, which is the only path that retires a settled admission.
fn require_known_obligation_owners(journal: &Journal, session: &SessionRecord) -> ModuleResult<()> {
    let authority = &journal.authority;
    let live_obligation = authority.admissions.values().any(|admission| {
        admission
            .plan
            .obligations
            .iter()
            .any(|obligation| session.obligations.contains(obligation))
    });
    if live_obligation
        || session
            .admission_ids
            .iter()
            .any(|id| authority.admissions.contains_key(id))
    {
        return Err("live_admission_retained".into());
    }
    let target = session_target(session);
    let owned_by_session = |id: &String| {
        session.admission_ids.contains(id)
            && session.admission_targets.get(id) == Some(&target)
            && matches!(
                session.admission_phases.get(id),
                Some(LifecyclePhase::SessionPrepare | LifecyclePhase::SessionRelease)
            )
            && journal
                .admission_transitions
                .get(id)
                .is_some_and(|transition| valid_id(transition))
    };
    let stream_admission_retained = session
        .admission_ids
        .iter()
        .any(|id| authority.settled.contains_key(id) && !owned_by_session(id));
    if stream_admission_retained {
        return Err("stream_cleanup_unresolved".into());
    }
    let foreign_holder = session.obligations.iter().any(|obligation| {
        authority.settled.iter().any(|(id, admission)| {
            admission.plan.obligations.contains(obligation) && !owned_by_session(id)
        })
    });
    if foreign_holder {
        return Err("obligation_owner_unknown".into());
    }
    Ok(())
}

/// The only retained work for the session must be its own undispatched
/// release, fenced while waiting for application retirement. Any provider
/// progress or failed-start evidence means the restart's effect is unknown.
fn retained_release_key(journal: &Journal, session: &SessionRecord) -> ModuleResult<String> {
    let mut retained = journal.receipts.iter().filter(|(_, receipt)| {
        receipt.work.as_ref().is_some_and(|work| {
            operations::target_parts(&work.target)
                .is_ok_and(|(epoch, id, _, _)| epoch == session.host_epoch && id == session.id)
        })
    });
    let (key, receipt) = retained.next().ok_or("release_work_unavailable")?;
    if retained.next().is_some() {
        return Err("session_work_retained".into());
    }
    let work = receipt.work.as_ref().expect("filtered retained work");
    if !owner_release_receipt(receipt, work, session) {
        return Err("release_work_unavailable".into());
    }
    if !undispatched_release(work) {
        return Err("restart_effect_unknown".into());
    }
    Ok(key.clone())
}

fn owner_release_receipt(receipt: &Receipt, work: &Work, session: &SessionRecord) -> bool {
    let target = session_target(session);
    let request_matches = match receipt.request["operation"].as_str() {
        Some("end_session") => receipt.request["target"] == json!(target),
        // An abandonment interrupted by another restart names its own epoch.
        Some("recover_session") => {
            receipt.request["restart_abandonment"] == true
                && session.application_disposition.is_some()
        }
        _ => false,
    };
    receipt.caller == session.owner.caller
        && receipt.initiator == session.owner.initiator
        && request_matches
        && receipt.response["status"] == "recovery_required"
        && work.phase == LifecyclePhase::SessionRelease
        && work.target == target
}

/// Mirrors `restartable_undispatched_release` for streams: no provider has
/// been called and the work is still fenced on application retirement.
fn undispatched_release(work: &Work) -> bool {
    work.waiting_retirement
        && !work.compacted_release
        && work.candidate.is_none()
        && work.index == 0
        && work.states.is_empty()
        && work.composition_progress.is_none()
        && work.owner_observations.is_empty()
        && work.reprepare.is_none()
        && work.failure.is_none()
        && work.failed_preparation.is_none()
        && work.action_transfer.is_none()
        && work.cleanup_blockers.is_empty()
}

/// Move the frozen release work out of its original receipt. The original
/// stays as a rejected tombstone, as restarted stream recovery does.
fn supersede_release_receipt(journal: &mut Journal, key: &str) -> ModuleResult<Work> {
    let receipt = journal
        .receipts
        .get_mut(key)
        .ok_or("release_work_unavailable")?;
    let work = receipt.work.take().ok_or("release_work_unavailable")?;
    receipt.response["status"] = json!("rejected");
    receipt.response["reason"] = json!(SUPERSEDED_REASON);
    Ok(work)
}

fn record_untracked_application(journal: &mut Journal, session_id: &str) -> ModuleResult<()> {
    let current_epoch = journal.host_epoch.clone();
    let session = journal
        .sessions
        .get_mut(session_id)
        .ok_or("unknown_session")?;
    session.application_disposition = Some(ApplicationDisposition::untracked_after_restart(
        session,
        &current_epoch,
    ));
    session.phase = SessionPhase::Ending;
    Ok(())
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    pub(in crate::coordinator) const OWNER: &str = "amaterasu.nvhttp";
    const OLD_EPOCH: &str = "old-epoch";

    pub(in crate::coordinator) fn open_coordinator(
        name: &str,
    ) -> (Coordinator, std::path::PathBuf) {
        let path = std::env::temp_dir().join(format!("{name}-{}", identifier().unwrap()));
        std::fs::create_dir(&path).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let mut coordinator = Coordinator::open(&path.join("journal.json")).unwrap();
        coordinator.configure_for_test(LifecycleSettings::default());
        (coordinator, path)
    }

    pub(in crate::coordinator) fn old_target(id: &str) -> LifecycleTarget {
        LifecycleTarget::Session {
            host_epoch: OLD_EPOCH.into(),
            session_id: id.into(),
            generation: 1,
        }
    }

    /// Insert a session stranded by a restart between `end_session` and
    /// `application_retired`, with its fenced, undispatched release work.
    pub(in crate::coordinator) fn insert_stranded(coordinator: &Coordinator, id: &str) {
        let target = old_target(id);
        let session: SessionRecord = serde_json::from_value(json!({
            "host_epoch":OLD_EPOCH, "id":id, "generation":1, "phase":"recovery_required",
            "owner":{"caller":OWNER,"initiator":OWNER}, "delegates":[],
            "application_ref":{"kind":"application","app_id":"7"}, "settings_revision":"one",
            "application_retired":false, "application_operation_id":"launch-7",
            "provider_graph":{"order":[],"dependencies":{}},
            "streams":{"stream":{"generation":1,"phase":"retired","candidate":null,
                "required":["capture"],"acknowledgements":["capture"],"retirements":["capture"]}}
        }))
        .unwrap();
        let mut work = coordinator
            .new_work(LifecyclePhase::SessionRelease, target.clone(), &json!({}))
            .unwrap();
        work.reason = Some(ReleaseReason::End);
        work.waiting_retirement = true;
        let operation_id = format!("{id}-end");
        let mut journal = coordinator.shared.borrow().value.clone();
        journal.sessions.insert(id.into(), session);
        journal.receipts.insert(
            receipt_key(OWNER, OWNER, &operation_id),
            Receipt {
                caller: OWNER.into(),
                initiator: OWNER.into(),
                request: json!({"operation":"end_session","operation_id":operation_id,"target":target}),
                response: json!({"status":"recovery_required","reason":"coordinator_restarted",
                    "operation_id":operation_id,"target":target,"fenced":true}),
                work: Some(work),
                wait_target: None,
            },
        );
        save(&coordinator.shared, journal).unwrap();
    }

    pub(in crate::coordinator) fn abandon_request(coordinator: &Coordinator, id: &str) -> Value {
        let epoch = coordinator.shared.borrow().value.host_epoch.clone();
        json!({"operation":"recover_session","restart_abandonment":true,"operation_id":format!("{id}-abandon"),
            "target":{"kind":"session","host_epoch":epoch,"session_id":id,"generation":1}})
    }

    fn edit_journal(coordinator: &Coordinator, edit: impl FnOnce(&mut Journal)) {
        let mut journal = coordinator.shared.borrow().value.clone();
        edit(&mut journal);
        save(&coordinator.shared, journal).unwrap();
    }

    fn release_work<'a>(journal: &'a mut Journal, id: &str) -> &'a mut Work {
        journal
            .receipts
            .get_mut(&receipt_key(OWNER, OWNER, &format!("{id}-end")))
            .and_then(|receipt| receipt.work.as_mut())
            .unwrap()
    }

    fn refusal(coordinator: &mut Coordinator, caller: &str, request: Value) -> String {
        let response = coordinator.handle(caller, caller, request);
        assert_eq!(response["status"], "rejected", "{response}");
        response["reason"].as_str().unwrap().to_owned()
    }

    fn settled_admission(obligation: &Value) -> crate::prepare_authority::Admission {
        let method = &obligation["cleanup_method"];
        let selection = json!({"participant":{"provider":"display","plan_method":method,
            "callback_method":method,"status_method":method,"binding_validate_method":null,
            "capabilities":[],"cleanup_methods":[],"settings_revision":"one","settings":{}},
            "approvals":[],"capabilities":[],"cleanup_methods":[],"authorized_borrowers":[]});
        let plan = json!({"plan_id":"plan","revision":1,"resources":[],"mutation_domains":[],
            "required_capabilities":[],"constraints":{},"obligations":[obligation],"affected_consumers":[],
            "consumer_change":"live","candidate_constraint":null,"allowed_degradation":[]});
        serde_json::from_value(
            json!({"provider":"display","selection":selection,"plan":plan,"rights":[]}),
        )
        .unwrap()
    }

    fn obligation(id: &str) -> Value {
        json!({"obligation_id":id,"provider":"display","resource_id":id,"mutation_domain":null,
            "depends_on":[],"payload":{},"cleanup_method":{"service":"display/1",
                "method":"lifecycle_callback","interface_major":1,"interface_minor":0,"schema_revision":1}})
    }

    /// The startup sweep must find stranded sessions even when retained
    /// history makes an unscoped snapshot exceed the broker result limit.
    #[test]
    fn restart_stranded_listing_is_owner_scoped_and_bounded() {
        let (mut coordinator, _root) = open_coordinator("amaterasu-stranded-listing");
        for id in 0..70 {
            insert_stranded(&coordinator, &format!("stranded-{id}"));
        }
        let mut journal = coordinator.shared.borrow().value.clone();
        let mut foreign = journal.sessions["stranded-0"].clone();
        foreign.id = "foreign".into();
        foreign.owner.caller = "other".into();
        foreign.owner.initiator = "other".into();
        journal.sessions.insert("foreign".into(), foreign);
        let mut current = journal.sessions["stranded-1"].clone();
        current.id = "current".into();
        current.host_epoch = journal.host_epoch.clone();
        journal.sessions.insert("current".into(), current);
        save(&coordinator.shared, journal).unwrap();

        let listing = coordinator.handle(
            OWNER,
            OWNER,
            json!({"operation":"snapshot","restart_stranded":true}),
        );
        assert_eq!(listing["status"], "ok", "{listing}");
        assert_eq!(listing["truncated"], true);
        let sessions = listing["sessions"].as_array().unwrap();
        assert_eq!(sessions.len(), 64);
        assert!(sessions.iter().all(|session| {
            session["host_epoch"] == OLD_EPOCH
                && session["session_id"]
                    .as_str()
                    .unwrap()
                    .starts_with("stranded-")
        }));
        assert!(serde_json::to_vec(&listing).unwrap().len() <= 16_384);
    }

    #[test]
    fn only_the_owner_abandons_an_old_epoch_undispatched_release() {
        let (mut coordinator, path) = open_coordinator("restart-abandon-owner");
        insert_stranded(&coordinator, "session");
        let request = abandon_request(&coordinator, "session");
        assert_eq!(
            refusal(&mut coordinator, "foreign", request.clone()),
            "permission_denied"
        );
        let mut stale = request.clone();
        stale["target"]["host_epoch"] = json!(OLD_EPOCH);
        assert_eq!(
            refusal(&mut coordinator, OWNER, stale),
            "stale_abandonment_target"
        );
        let response = coordinator.handle(OWNER, OWNER, request.clone());
        assert_eq!(response["status"], "pending", "{response}");
        assert_eq!(response["abandoned_target"], json!(old_target("session")));
        assert_eq!(coordinator.handle(OWNER, OWNER, request), response);
        let journal = coordinator.shared.borrow().value.clone();
        let session = &journal.sessions["session"];
        assert!(session.phase == SessionPhase::Ending);
        assert!(!session.application_retired);
        assert!(session.application_retirement_proof.is_none());
        assert!(
            session
                .application_disposition
                .as_ref()
                .is_some_and(|disposition| {
                    disposition.valid_for(session, &journal.host_epoch)
                        && *disposition
                            == ApplicationDisposition::untracked_after_restart(
                                session,
                                &journal.host_epoch,
                            )
                })
        );
        let original = &journal.receipts[&receipt_key(OWNER, OWNER, "session-end")];
        assert!(original.work.is_none());
        assert_eq!(original.response["status"], "rejected");
        assert_eq!(original.response["reason"], SUPERSEDED_REASON);
        let work = journal.receipts[&receipt_key(OWNER, OWNER, "session-abandon")]
            .work
            .as_ref()
            .unwrap();
        assert_eq!(
            work.recovery_authorized_epoch.as_deref(),
            Some(journal.host_epoch.as_str())
        );
        assert_eq!(work.target, old_target("session"));
        coordinator
            .advance(&mut |_| panic!("an empty release graph must not dispatch a provider"))
            .unwrap();
        let result = coordinator.handle(
            OWNER,
            OWNER,
            json!({"operation":"lookup","operation_id":"session-abandon"}),
        );
        assert_eq!(result["status"], "succeeded", "{result}");
        assert_eq!(result["application_retired"], false);
        assert_eq!(result["application"], UNTRACKED_APPLICATION);
        let journal = coordinator.shared.borrow().value.clone();
        assert!(journal.sessions["session"].phase == SessionPhase::Ended);
        assert!(!journal.sessions["session"].application_retired);
        drop(coordinator);
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn abandonment_refuses_current_epoch_live_streams_and_retired_applications() {
        let (mut coordinator, path) = open_coordinator("restart-abandon-state");
        insert_stranded(&coordinator, "session");
        let request = abandon_request(&coordinator, "session");
        let current = coordinator.shared.borrow().value.host_epoch.clone();
        edit_journal(&coordinator, |journal| {
            journal.sessions.get_mut("session").unwrap().host_epoch = current;
        });
        assert_eq!(
            refusal(&mut coordinator, OWNER, request.clone()),
            "session_not_abandonable"
        );
        edit_journal(&coordinator, |journal| {
            let session = journal.sessions.get_mut("session").unwrap();
            session.host_epoch = OLD_EPOCH.into();
            session.streams.get_mut("stream").unwrap().phase = StreamPhase::RecoveryRequired;
        });
        assert_eq!(
            refusal(&mut coordinator, OWNER, request.clone()),
            "streams_not_retired"
        );
        edit_journal(&coordinator, |journal| {
            let session = journal.sessions.get_mut("session").unwrap();
            session.streams.get_mut("stream").unwrap().phase = StreamPhase::Retired;
            session.application_retired = true;
        });
        assert_eq!(
            refusal(&mut coordinator, OWNER, request),
            "application_already_retired"
        );
        assert!(
            coordinator.shared.borrow().value.receipts[&receipt_key(OWNER, OWNER, "session-end")]
                .work
                .is_some()
        );
        drop(coordinator);
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn dispatched_or_uncertain_release_work_is_never_abandoned() {
        type WorkMutation = fn(&mut Work);
        let mutations: [(&str, WorkMutation); 4] = [
            ("dispatched", |work| work.index = 1),
            ("retirement_passed", |work| work.waiting_retirement = false),
            ("blocked", |work| {
                work.cleanup_blockers.insert("display".into());
            }),
            ("uncertain_failed_preparation", |work| {
                work.failed_preparation = Some(FailedPreparation {
                    reason: "provider_rejected_or_requires_recovery".into(),
                    uncertain: true,
                    states: BTreeMap::new(),
                });
            }),
        ];
        for (name, mutate) in mutations {
            let (mut coordinator, path) = open_coordinator("restart-abandon-work");
            insert_stranded(&coordinator, "session");
            edit_journal(&coordinator, |journal| {
                mutate(release_work(journal, "session"))
            });
            let request = abandon_request(&coordinator, "session");
            assert_eq!(
                refusal(&mut coordinator, OWNER, request),
                "restart_effect_unknown",
                "{name}"
            );
            let journal = coordinator.shared.borrow().value.clone();
            assert!(
                journal.sessions["session"]
                    .application_disposition
                    .is_none()
            );
            assert!(
                journal.receipts[&receipt_key(OWNER, OWNER, "session-end")]
                    .work
                    .is_some()
            );
            drop(coordinator);
            std::fs::remove_dir_all(path).unwrap();
        }
    }

    #[test]
    fn abandonment_requires_session_owned_settled_admissions() {
        let (mut coordinator, path) = open_coordinator("restart-abandon-obligations");
        insert_stranded(&coordinator, "session");
        let request = abandon_request(&coordinator, "session");
        let session_obligation = obligation("cleanup-session");
        edit_journal(&coordinator, |journal| {
            let session = journal.sessions.get_mut("session").unwrap();
            session
                .obligations
                .push(serde_json::from_value(session_obligation.clone()).unwrap());
            journal
                .authority
                .settled
                .insert("foreign".into(), settled_admission(&session_obligation));
            journal
                .admission_transitions
                .insert("foreign".into(), "transition".into());
        });
        assert_eq!(
            refusal(&mut coordinator, OWNER, request.clone()),
            "obligation_owner_unknown"
        );
        edit_journal(&coordinator, |journal| {
            let session = journal.sessions.get_mut("session").unwrap();
            session.admission_ids.insert("foreign".into());
            session.admission_targets.insert(
                "foreign".into(),
                LifecycleTarget::Stream {
                    host_epoch: OLD_EPOCH.into(),
                    session_id: "session".into(),
                    stream_id: "stream".into(),
                    generation: 1,
                },
            );
            session
                .admission_phases
                .insert("foreign".into(), LifecyclePhase::StreamPrepare);
        });
        assert_eq!(
            refusal(&mut coordinator, OWNER, request.clone()),
            "stream_cleanup_unresolved"
        );
        edit_journal(&coordinator, |journal| {
            let admission = journal.authority.settled.remove("foreign").unwrap();
            journal
                .authority
                .admissions
                .insert("foreign".into(), admission);
        });
        assert_eq!(
            refusal(&mut coordinator, OWNER, request.clone()),
            "live_admission_retained"
        );
        edit_journal(&coordinator, |journal| {
            let admission = journal.authority.admissions.remove("foreign").unwrap();
            journal
                .authority
                .settled
                .insert("foreign".into(), admission);
            let session = journal.sessions.get_mut("session").unwrap();
            session
                .admission_targets
                .insert("foreign".into(), old_target("session"));
            session
                .admission_phases
                .insert("foreign".into(), LifecyclePhase::SessionPrepare);
        });
        let response = coordinator.handle(OWNER, OWNER, request);
        assert_eq!(response["status"], "pending", "{response}");
        let journal = coordinator.shared.borrow().value.clone();
        assert_eq!(journal.sessions["session"].obligations.len(), 1);
        assert!(journal.authority.settled.contains_key("foreign"));
        drop(coordinator);
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn disposition_is_rejected_on_current_epoch_or_beside_a_retirement() {
        let journal: Journal = serde_json::from_value(json!({
            "version":3,"host_epoch":"current","receipts":{},"sessions":{"session":{
                "host_epoch":OLD_EPOCH,"id":"session","generation":1,"phase":"ended",
                "owner":{"caller":OWNER,"initiator":OWNER},"delegates":[],"application_ref":{},
                "settings_revision":"one","application_retired":false,"streams":{},
                "application_disposition":{"kind":"untracked_after_restart",
                    "abandoned_epoch":OLD_EPOCH,"recorded_epoch":"current"}}}
        }))
        .unwrap();
        assert!(validate_journal(&journal).is_ok());
        let roundtrip: Journal =
            serde_json::from_slice(&serde_json::to_vec(&journal).unwrap()).unwrap();
        assert!(validate_journal(&roundtrip).is_ok());
        let mut current_epoch = journal.clone();
        current_epoch
            .sessions
            .get_mut("session")
            .unwrap()
            .host_epoch = "current".into();
        assert!(validate_journal(&current_epoch).is_err());
        let mut retired = journal.clone();
        retired
            .sessions
            .get_mut("session")
            .unwrap()
            .application_retired = true;
        assert!(validate_journal(&retired).is_err());
        let mut mismatched = journal;
        mismatched
            .sessions
            .get_mut("session")
            .unwrap()
            .application_disposition = Some(ApplicationDisposition::UntrackedAfterRestart {
            abandoned_epoch: "another".into(),
            recorded_epoch: "current".into(),
            application_operation_id: None,
        });
        assert!(validate_journal(&mismatched).is_err());
    }
}
