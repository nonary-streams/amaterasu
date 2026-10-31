//! Explicit, owner-authenticated reconciliation of persisted callback receipts.
use super::*;

impl Coordinator {
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
            owner_observations: Default::default(),
            reprepare: None,
            phase: LifecyclePhase::StreamPrepare,
            target: old_target.clone(),
            intent: Some(StreamIntent::Initial),
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
            application: None,
            application_operation_id: None,
            admission_ids: Default::default(),
            admission_targets: Default::default(),
            admission_phases: Default::default(),
            provider_settings: Default::default(),
            resource_generations: Default::default(),
            streams: [(
                "stream".into(),
                StreamRecord {
                    provider_graph: None,
                    generation: 1,
                    phase: StreamPhase::RecoveryRequired,
                    candidate: None,
                    required: ["capture".into()].into(),
                    acknowledgements: BTreeSet::new(),
                    retirements: BTreeSet::new(),
                    binding_owner: String::new(),
                    preparation_transition: "transition".into(),
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
