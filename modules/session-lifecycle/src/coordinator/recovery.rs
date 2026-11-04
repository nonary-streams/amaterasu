//! Explicit, owner-authenticated reconciliation of persisted callback receipts.
use super::*;

impl Coordinator {
    /// Resolve an exact retained admission for a native-retired consumer. This
    /// read-only query cannot retire resources, adopt capture or revive an epoch.
    pub(super) fn resolve_stream_preparation(
        &self,
        caller: &str,
        initiator: &str,
        payload: Value,
    ) -> Value {
        use amaterasu_extension_api::{
            StreamPreparationLookup, StreamPreparationResolution,
            StreamPreparationResolutionStatus, validate_stream_preparation_lookup,
        };
        let Ok(request) = serde_json::from_value::<StreamPreparationLookup>(payload) else {
            return rejection("invalid_preparation_lookup");
        };
        if caller != initiator
            || !self.settings.consumers.capture.contains(caller)
            || !self
                .settings
                .operation_grants
                .get(caller)
                .is_some_and(|grants| grants.contains("resolve_stream_preparation"))
        {
            return rejection("permission_denied");
        }
        if validate_stream_preparation_lookup(&request).is_err() {
            return rejection("invalid_preparation_lookup");
        }
        let Ok((epoch, id, None, generation)) = operations::target_parts(&request.session) else {
            return rejection("invalid_target");
        };
        let journal = &self.shared.borrow().value;
        let Some(session) = journal.sessions.get(id) else {
            return rejection("unknown_session");
        };
        if session.host_epoch != epoch
            || session.generation != generation
            || !session.delegates.contains(caller)
            || !(session.delegates.contains(&request.preparation_caller)
                || session.owner.caller == request.preparation_caller)
        {
            return rejection("permission_denied");
        }
        let mut receipts = journal.receipts.values().filter(|receipt| {
            receipt.caller == request.preparation_caller
                && receipt.initiator == request.preparation_caller
                && receipt.request["operation_id"] == request.preparation_operation_id
                && matches!(
                    receipt.request["operation"].as_str(),
                    Some("prepare_stream" | "resume_stream")
                )
                && receipt.request["target"] == json!(request.session)
        });
        let receipt = receipts.next();
        if receipts.next().is_some() {
            return rejection("preparation_identity_ambiguous");
        }
        let (status, target, preparation_status) = if let Some(receipt) = receipt {
            let Ok(target) =
                serde_json::from_value::<LifecycleTarget>(receipt.response["target"].clone())
            else {
                return rejection("preparation_target_unavailable");
            };
            let Ok((target_epoch, target_session, Some(stream_id), stream_generation)) =
                operations::target_parts(&target)
            else {
                return rejection("preparation_target_unavailable");
            };
            if target_epoch != epoch
                || target_session != id
                || session
                    .streams
                    .get(stream_id)
                    .is_none_or(|stream| stream.generation != stream_generation)
            {
                return rejection("preparation_target_unavailable");
            }
            (
                StreamPreparationResolutionStatus::Succeeded,
                Some(target),
                receipt.response["status"].as_str().map(str::to_owned),
            )
        } else {
            (
                if epoch == journal.host_epoch {
                    StreamPreparationResolutionStatus::Pending
                } else {
                    StreamPreparationResolutionStatus::Missing
                },
                None,
                None,
            )
        };
        let response = StreamPreparationResolution {
            status,
            session: request.session.clone(),
            preparation_operation_id: request.preparation_operation_id.clone(),
            preparation_caller: request.preparation_caller.clone(),
            target,
            preparation_status,
        };
        if amaterasu_extension_api::validate_stream_preparation_resolution(&request, &response)
            .is_err()
        {
            return rejection("preparation_resolution_unverified");
        }
        serde_json::to_value(response)
            .unwrap_or_else(|_| rejection("invalid_preparation_resolution"))
    }

    /// A selected capture consumer reconciles its own durable stream marker only
    /// after retiring native resources. Old epochs cannot authorize new effects;
    /// this path runs the frozen providers' release graph, never their prepare.
    pub(super) fn retire_restarted_stream(
        &mut self,
        caller: &str,
        initiator: &str,
        payload: Value,
        key: String,
    ) -> ModuleResult<Value> {
        if caller != initiator || !self.settings.consumers.capture.contains(caller) {
            return Err("permission_denied".into());
        }
        let target: LifecycleTarget =
            serde_json::from_value(payload["target"].clone()).map_err(|_| "invalid_target")?;
        let (epoch, id, stream_id, generation) = operations::target_parts(&target)?;
        let stream_id = stream_id.ok_or("stream_target_required")?;
        let mut next = self.shared.borrow().value.clone();
        let session = next.sessions.get(id).ok_or("unknown_session")?;
        let stream = session.streams.get(stream_id).ok_or("unknown_stream")?;
        let capture_retired = stream.retirements.contains("capture");
        if epoch == next.host_epoch
            || session.host_epoch != epoch
            || !session.capture_binding_mode.is_external()
            || !session.delegates.contains(caller)
            || stream.generation != generation
            || stream.phase != StreamPhase::RecoveryRequired
            || stream.required != BTreeSet::from(["capture".into()])
            || next.receipts.values().any(|receipt| {
                receipt.work.as_ref().is_some_and(|work| {
                    work.target == target
                        && !rejected_read_only_release(work)
                        && !restartable_undispatched_release(receipt, work, caller, capture_retired)
                })
            })
        {
            return Err("stream_not_recoverable".into());
        }
        let evidence = &payload["retirement_receipt"];
        if evidence["response"]["code"] != "not_found"
            || evidence["operation_id"].as_str().is_none_or(str::is_empty)
            || evidence["service"].as_str().is_none_or(str::is_empty)
        {
            return Err("native_retirement_unconfirmed".into());
        }
        // A settled ResourcePlan rejection has no admitted effect. Preserve its
        // rejection receipt while allowing a fresh cleanup attempt; unknown or
        // effectful callbacks remain fenced by the check above.
        let resumable: Vec<_> = next
            .receipts
            .iter()
            .filter_map(|(key, receipt)| {
                receipt
                    .work
                    .as_ref()
                    .filter(|work| {
                        work.target == target
                            && restartable_undispatched_release(
                                receipt,
                                work,
                                caller,
                                capture_retired,
                            )
                    })
                    .map(|_| key.clone())
            })
            .collect();
        if resumable.len() > 1 {
            return Err("stream_not_recoverable".into());
        }
        let retained_work = resumable.first().map(|key| {
            let receipt = next.receipts.get_mut(key).expect("selected receipt");
            receipt.response["status"] = json!("rejected");
            receipt.response["reason"] = json!("superseded_by_restart_recovery");
            receipt.work.take().expect("selected release work")
        });
        for receipt in next.receipts.values_mut() {
            if receipt
                .work
                .as_ref()
                .is_some_and(|work| work.target == target && rejected_read_only_release(work))
            {
                receipt.work = None;
                receipt.response["status"] = json!("rejected");
            }
        }
        let resumed_existing_work = retained_work.is_some();
        let mut work = match retained_work {
            Some(work) => work,
            None => self.new_work(LifecyclePhase::StreamRelease, target.clone(), &payload)?,
        };
        work.recovery_authorized_epoch = Some(next.host_epoch.clone());
        if !resumed_existing_work {
            work.use_release_graph(stream.provider_graph.clone());
            work.waiting_retirement = true;
            work.join_context = stream.join_context.clone();
            if work.join_context.is_some() {
                work.intent = Some(StreamIntent::Join);
            }
        }
        if work.join_context != stream.join_context
            || (work.join_context.is_some() && work.intent != Some(StreamIntent::Join))
        {
            return Err("shared_source_recovery_context_changed".into());
        }
        work.reason = Some(ReleaseReason::Pause);
        let stream = next
            .sessions
            .get_mut(id)
            .unwrap()
            .streams
            .get_mut(stream_id)
            .unwrap();
        stream.phase = StreamPhase::Quiescing;
        stream.retirements.insert("capture".into());
        let response = json!({"status":"pending", "operation_id":payload["operation_id"],
            "target":target,"plan_revision":null,"transition_id":work.transition_id});
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

    /// Re-arm status polling only for a callback whose provider receipt was
    /// durably recorded before restart. Unknown receipt IDs never trigger replay.
    pub(super) fn recover_session(
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
        let (epoch, session_id, stream, generation) = super::operations::target_parts(&target)?;
        if stream.is_some() || epoch != next.host_epoch {
            return Err("stale_recovery_target".into());
        }
        let session = next.sessions.get(session_id).ok_or("unknown_session")?;
        if session.owner.caller != caller || session.owner.initiator != initiator {
            return Err("permission_denied".into());
        }
        if session.generation != generation || session.phase != SessionPhase::RecoveryRequired {
            return Err("session_not_recoverable".into());
        }
        let original = payload
            .get("original_operation_id")
            .and_then(Value::as_str)
            .filter(|v| valid_id(v))
            .ok_or("original_operation_required")?;
        let selected_caller = payload.get("original_caller").and_then(Value::as_str);
        let matches: Vec<String> = next
            .receipts
            .iter()
            .filter_map(|(receipt_key, receipt)| {
                let work = receipt.work.as_ref()?;
                let (_, work_session, _, _) = super::operations::target_parts(&work.target).ok()?;
                (receipt.request.get("operation_id").and_then(Value::as_str) == Some(original)
                    && work_session == session_id
                    && selected_caller.is_none_or(|selected| receipt.caller == selected))
                .then(|| receipt_key.clone())
            })
            .collect();
        if matches.len() != 1 {
            return Err("unknown_or_ambiguous_original_operation".into());
        }
        let original_key = &matches[0];
        let receipt = next
            .receipts
            .get_mut(original_key)
            .ok_or("unknown_original_operation")?;
        if receipt.response["status"] != "recovery_required" {
            return Err("operation_not_recoverable".into());
        }
        let work = receipt.work.as_mut().ok_or("recovery_work_unavailable")?;
        let (old_epoch, old_session, old_stream, _) =
            super::operations::target_parts(&work.target)?;
        if old_session != session_id || old_epoch != session.host_epoch || old_epoch == epoch {
            return Err("recovery_target_mismatch".into());
        }
        let provider = work
            .providers
            .get(work.index)
            .ok_or("recovery_provider_unavailable")?
            .clone();
        let state = work
            .states
            .get_mut(&provider)
            .ok_or("recovery_provider_state_unavailable")?;
        if state.request.operation != amaterasu_extension_api::ProviderOperation::LifecycleCallback
            || state.pending_receipt.as_ref().is_none_or(String::is_empty)
            || !matches!(
                state.phase,
                crate::prepare_driver::PreparePhase::Callback
                    | crate::prepare_driver::PreparePhase::CallbackStatus
                    | crate::prepare_driver::PreparePhase::RecoveryRequired
            )
        {
            return Err("provider_receipt_unknown".into());
        }
        // Status polls identify the original provider effect. Never issue the
        // old callback again merely because a process restarted.
        state.phase = crate::prepare_driver::PreparePhase::CallbackStatus;
        state.dispatch = None;
        state.status_polls = 0;
        work.recovery_authorized_epoch = Some(epoch.into());
        let phase = work.phase.clone();
        let old_stream = old_stream.map(str::to_owned);
        receipt.response["status"] = json!("pending");
        receipt.response["reason"] = json!("reconciliation_in_progress");
        let session = next.sessions.get_mut(session_id).unwrap();
        match phase {
            LifecyclePhase::SessionPrepare => session.phase = SessionPhase::Preparing,
            LifecyclePhase::StreamPrepare => {
                let stream = session
                    .streams
                    .get_mut(old_stream.as_deref().ok_or("stream_target_required")?)
                    .ok_or("unknown_stream")?;
                stream.phase = StreamPhase::Preparing;
            }
            LifecyclePhase::StreamRelease => {
                let stream = session
                    .streams
                    .get_mut(old_stream.as_deref().ok_or("stream_target_required")?)
                    .ok_or("unknown_stream")?;
                stream.phase = StreamPhase::Quiescing;
            }
            LifecyclePhase::SessionRelease => session.phase = SessionPhase::Ending,
        }
        let operation_id = payload["operation_id"].clone();
        let response = json!({"status":"succeeded","operation_id":operation_id,"target":target,"reconciliation_started":true,"original_operation_id":original});
        next.recovery_sources
            .insert(key.clone(), original_key.clone());
        next.receipts.insert(
            key,
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
        Ok(response)
    }
}

fn restartable_undispatched_release(
    receipt: &Receipt,
    work: &Work,
    caller: &str,
    capture_retired: bool,
) -> bool {
    receipt.caller == caller
        && receipt.initiator == caller
        && (receipt.request["operation"] == "pause_stream"
            || (capture_retired && receipt.request["operation"] == "retire_stream"))
        && receipt.request["target"] == json!(work.target)
        && receipt.response["status"] == "recovery_required"
        && work.phase == LifecyclePhase::StreamRelease
        && (work.waiting_retirement || capture_retired)
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

fn rejected_read_only_release(work: &Work) -> bool {
    work.phase == LifecyclePhase::StreamRelease
        && !work.states.is_empty()
        && work.states.values().all(|state| {
            state.phase == crate::prepare_driver::PreparePhase::Rejected
                && state.request.operation
                    == amaterasu_extension_api::ProviderOperation::ResourcePlan
                && state.admission_id.is_none()
                && state.pending_receipt.is_none()
                && matches!(
                    state.response,
                    Some(amaterasu_extension_api::ProviderResponse::Rejected { .. })
                )
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    #[test]
    fn owner_requeues_known_delegate_receipt_without_replaying_callback() {
        let dir = std::env::temp_dir().join(format!(
            "amaterasu-recovery-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&dir).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let mut coordinator = Coordinator::open(&dir.join("journal.json")).unwrap();
        let mut next = coordinator.shared.borrow().value.clone();
        let current = next.host_epoch.clone();
        let old_target = LifecycleTarget::Stream {
            host_epoch: "old".into(),
            session_id: "session".into(),
            stream_id: "stream".into(),
            generation: 1,
        };
        let participant: crate::adapter::ParticipantSettings=serde_json::from_value(json!({"provider":"display","plan_method":{"service":"display/1","method":"plan","interface_major":1,"interface_minor":0,"schema_revision":1},"callback_method":{"service":"display/1","method":"callback","interface_major":1,"interface_minor":0,"schema_revision":1},"status_method":{"service":"display/1","method":"status","interface_major":1,"interface_minor":0,"schema_revision":1},"settings_revision":"1","settings":{},"capabilities":[],"cleanup_methods":[]})).unwrap();
        let request: amaterasu_extension_api::ProviderRequest=serde_json::from_value(json!({"provider_sequence":4,"operation":"lifecycle_callback","version":1,"transition_id":"transition","operation_id":"effect","target":old_target,"phase":"stream.prepare","settings_revision":"1","settings":{},"intent":"initial","reason":null,"requested_video":null,"consumer_requirements":{"execution_domain":"seat","capture_providers":["capture"],"selector_schemas":["selector/1"]},"remaining_ms":5000,"plan_revision":null,"admission_id":null,"approved_plan":null})).unwrap();
        let state = PrepareState {
            provider: "display".into(),
            participant,
            request,
            phase: crate::prepare_driver::PreparePhase::CallbackStatus,
            plan: None,
            admission_id: None,
            pending_receipt: Some("provider-receipt".into()),
            pending_dispatch: None,
            status_polls: 1,
            last_sequence: 4,
            dispatch: None,
            terminal_dispatch: None,
            response: None,
        };
        let work = Work {
            composition_progress: None,
            compacted_release: false,
            owner_observations: Default::default(),
            processing_decision: None,
            reprepare: None,
            phase: LifecyclePhase::StreamPrepare,
            target: old_target.clone(),
            intent: Some(StreamIntent::Initial),
            join_context: None,
            reason: None,
            video: None,
            transition_id: "transition".into(),
            providers: vec!["display".into()],
            provider_graph: None,
            cleanup_blockers: BTreeSet::new(),
            index: 0,
            states: [("display".into(), state)].into(),
            candidate: None,
            waiting_retirement: false,
            failure: None,
            failed_preparation: None,
            action_transfer: None,
            recovery_authorized_epoch: None,
        };
        let session = SessionRecord {
            processing_decision: None,
            provider_graph: None,
            host_epoch: "old".into(),
            id: "session".into(),
            generation: 1,
            phase: SessionPhase::RecoveryRequired,
            owner: crate::action_authority::Principal {
                caller: "nvhttp".into(),
                initiator: "nvhttp".into(),
            },
            delegates: ["rtsp".into()].into(),
            application_ref: json!({"kind":"desktop"}),
            settings_revision: "1".into(),
            application_retired: false,
            application_retirement_proof: None,
            application: None,
            application_operation_id: None,
            admission_ids: Default::default(),
            admission_targets: Default::default(),
            capture_binding_mode: Default::default(),
            prepared_plan_ids: Default::default(),
            admission_phases: Default::default(),
            provider_settings: Default::default(),
            resource_generations: Default::default(),
            streams: [(
                "stream".into(),
                StreamRecord {
                    join_context: None,
                    provider_graph: None,
                    generation: 1,
                    phase: StreamPhase::RecoveryRequired,
                    candidate: None,
                    required: ["capture".into()].into(),
                    acknowledgements: BTreeSet::new(),
                    retirements: BTreeSet::new(),
                    binding_owner: String::new(),
                    preparation_transition: "transition".into(),
                    snapshot_scopes: None,
                    admitted_live_options: None,
                    provider_settings: Default::default(),
                },
            )]
            .into(),
            obligations: vec![],
            required_consumers: None,
        };
        next.sessions.insert("session".into(), session);
        let old_key = receipt_key("rtsp", "rtsp", "original");
        next.receipts.insert(
            old_key.clone(),
            Receipt {
                caller: "rtsp".into(),
                initiator: "rtsp".into(),
                request: json!({"operation_id":"original"}),
                response: json!({"status":"recovery_required","operation_id":"original"}),
                work: Some(work),
                wait_target: None,
            },
        );
        save(&coordinator.shared, next).unwrap();
        let target = LifecycleTarget::Session {
            host_epoch: current.clone(),
            session_id: "session".into(),
            generation: 1,
        };
        let payload = json!({"operation":"recover_session","operation_id":"recover","target":target,"original_operation_id":"original"});
        assert!(
            coordinator
                .recover_session(
                    "attacker",
                    "attacker",
                    payload.clone(),
                    receipt_key("attacker", "attacker", "recover")
                )
                .is_err()
        );
        let result = coordinator
            .recover_session(
                "nvhttp",
                "nvhttp",
                payload,
                receipt_key("nvhttp", "nvhttp", "recover"),
            )
            .unwrap();
        assert_eq!(result["reconciliation_started"], true);
        let journal = coordinator.shared.borrow();
        let work = journal.value.receipts[&old_key].work.as_ref().unwrap();
        assert_eq!(
            work.recovery_authorized_epoch.as_deref(),
            Some(current.as_str())
        );
        let state = &work.states["display"];
        assert_eq!(
            state.phase,
            crate::prepare_driver::PreparePhase::CallbackStatus
        );
        assert_eq!(
            state.request.operation,
            amaterasu_extension_api::ProviderOperation::LifecycleCallback
        );
        assert_eq!(state.request.operation_id, "effect");
        assert!(state.dispatch.is_none());
        drop(journal);
        drop(coordinator);
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[cfg(test)]
mod stream_restart_tests {
    use super::*;

    #[test]
    fn retired_process_recovery_releases_only_the_exact_delegated_stream() {
        let path = std::env::temp_dir().join(format!("stream-recovery-{}", identifier().unwrap()));
        std::fs::create_dir(&path).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let mut coordinator = Coordinator::open(&path.join("journal.json")).unwrap();
        let mut settings = LifecycleSettings {
            capture_binding_mode: amaterasu_extension_api::CaptureBindingMode::External,
            ..LifecycleSettings::default()
        };
        settings.consumers.capture.insert("capture".into());
        coordinator.configure_for_test(settings);
        let mut journal = coordinator.shared.borrow().value.clone();
        let session: SessionRecord = serde_json::from_value(json!({
            "capture_binding_mode":"external", "host_epoch":"old-epoch", "id":"session", "generation":1,
            "phase":"recovery_required", "owner":{"caller":"transport","initiator":"client"},
            "delegates":["capture"], "application_ref":{}, "settings_revision":"one", "application_retired":false,
            "streams":{"stream":{"generation":1,"phase":"recovery_required","candidate":null,
                "required":["capture"],"acknowledgements":["capture"],"retirements":[],
                "provider_graph":{"order":[],"dependencies":{}}}}
        })).unwrap();
        journal.sessions.insert("session".into(), session);
        save(&coordinator.shared, journal).unwrap();
        let target = json!({"kind":"stream","host_epoch":"old-epoch","session_id":"session","stream_id":"stream","generation":1});
        let request = json!({"operation":"retire_stream","operation_id":"recover", "restart_recovery":true,
            "target":target,"retirement_receipt":{"service":"video/1","operation_id":"original-video","response":{"code":"not_found"}}});
        assert_eq!(
            coordinator.handle("foreign", "foreign", request.clone())["status"],
            "rejected"
        );
        let mut missing = request.clone();
        missing["retirement_receipt"]["response"]["code"] = json!("pending");
        assert_eq!(
            coordinator.handle("capture", "capture", missing)["reason"],
            "native_retirement_unconfirmed"
        );
        let mut stale = request.clone();
        stale["target"]["generation"] = json!(2);
        assert_eq!(
            coordinator.handle("capture", "capture", stale)["reason"],
            "stream_not_recoverable"
        );
        let target_value: LifecycleTarget = serde_json::from_value(target.clone()).unwrap();
        let mut work = coordinator
            .new_work(LifecyclePhase::StreamRelease, target_value, &json!({}))
            .unwrap();
        work.waiting_retirement = true;
        let transition = work.transition_id.clone();
        let mut journal = coordinator.shared.borrow().value.clone();
        journal.receipts.insert(
            "old-pause".into(),
            Receipt {
                caller: "capture".into(),
                initiator: "capture".into(),
                request: json!({"operation":"pause_stream","operation_id":"pause","target":target}),
                response: json!({"status":"recovery_required","reason":"coordinator_restarted"}),
                work: Some(work),
                wait_target: None,
            },
        );
        save(&coordinator.shared, journal).unwrap();
        let mut unsafe_journal = coordinator.shared.borrow().value.clone();
        unsafe_journal
            .receipts
            .get_mut("old-pause")
            .unwrap()
            .work
            .as_mut()
            .unwrap()
            .waiting_retirement = false;
        save(&coordinator.shared, unsafe_journal).unwrap();
        assert_eq!(
            coordinator.handle("capture", "capture", request.clone())["reason"],
            "stream_not_recoverable"
        );
        let mut journal = coordinator.shared.borrow().value.clone();
        // Capture may have retired before the coordinator was interrupted.
        // With no provider dispatch or admission, restart can safely resume
        // the same release after verifying native retirement again.
        journal
            .sessions
            .get_mut("session")
            .unwrap()
            .streams
            .get_mut("stream")
            .unwrap()
            .retirements
            .insert("capture".into());
        journal.receipts.get_mut("old-pause").unwrap().request["operation"] =
            json!("retire_stream");
        save(&coordinator.shared, journal).unwrap();
        assert_eq!(
            coordinator.handle("capture", "capture", request.clone())["status"],
            "pending"
        );
        assert_eq!(
            coordinator.shared.borrow().value.receipts["old-pause"].response["reason"],
            "superseded_by_restart_recovery"
        );
        assert!(
            coordinator
                .shared
                .borrow()
                .value
                .receipts
                .values()
                .any(|receipt| receipt
                    .work
                    .as_ref()
                    .is_some_and(|work| work.transition_id == transition))
        );
        coordinator
            .advance(&mut |_| panic!("empty release graph must not dispatch a provider"))
            .unwrap();
        let result = coordinator.handle(
            "capture",
            "capture",
            json!({"operation":"lookup","operation_id":"recover"}),
        );
        assert_eq!(result["status"], "succeeded", "{result}");
        assert_eq!(result["target"], target);
        assert_eq!(coordinator.handle("capture", "capture", request), result);
        drop(coordinator);
        std::fs::remove_dir_all(path).unwrap();
    }
}
