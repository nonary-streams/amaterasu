use super::*;

pub(super) fn target_parts(
    target: &LifecycleTarget,
) -> ModuleResult<(&str, &str, Option<&str>, u64)> {
    match target {
        LifecycleTarget::Session {
            host_epoch,
            session_id,
            generation,
        } => Ok((host_epoch, session_id, None, *generation)),
        LifecycleTarget::Stream {
            host_epoch,
            session_id,
            stream_id,
            generation,
        } => Ok((host_epoch, session_id, Some(stream_id), *generation)),
        LifecycleTarget::Validation { .. } => Err("session_target_required".into()),
    }
}
pub(super) fn allowed(session: &SessionRecord, caller: &str, initiator: &str) -> bool {
    (session.owner.caller == caller && session.owner.initiator == initiator)
        || (caller == initiator && session.delegates.contains(caller))
}

fn require_unused_stream_id(session: &SessionRecord, stream_id: &str) -> ModuleResult<()> {
    if session.streams.contains_key(stream_id) {
        return Err("stream_identifier_collision".into());
    }
    Ok(())
}
fn optionally_retire_closed_history(
    journal: &mut Journal,
    retire: impl FnOnce(&mut Journal) -> ModuleResult<()>,
) -> ModuleResult<()> {
    let mut candidate = journal.clone();
    match retire(&mut candidate) {
        Ok(()) => *journal = candidate,
        Err(error)
            if error == "closed_stream_history_in_use"
                || error == "closed_stream_history_not_ready"
                || error == "control_replay_fence_capacity" => {}
        Err(error) => return Err(error),
    }
    Ok(())
}

pub(super) fn checked_target<'a>(
    journal: &'a Journal,
    target: &LifecycleTarget,
    caller: &str,
    initiator: &str,
) -> ModuleResult<(&'a SessionRecord, Option<String>)> {
    let (epoch, id, stream, generation) = target_parts(target)?;
    if retention::session_expired(journal, target) {
        return Err("session_expired".into());
    }
    if epoch != journal.host_epoch {
        return Err("stale_host_epoch".into());
    }
    let session = journal.sessions.get(id).ok_or("unknown_session")?;
    if !allowed(session, caller, initiator) {
        return Err("permission_denied".into());
    }
    if session.host_epoch != epoch {
        return Err("stale_host_epoch".into());
    }
    if let Some(stream) = stream {
        if session
            .streams
            .get(stream)
            .is_none_or(|s| s.generation != generation)
        {
            return Err("stale_generation".into());
        }
    } else if session.generation != generation {
        return Err("stale_generation".into());
    }
    Ok((session, stream.map(str::to_owned)))
}

/// Reprepare only when a durable action proof identifies the exact removed root.
fn required_reprepare(
    journal: &Journal,
    settings: &LifecycleSettings,
    session: &SessionRecord,
) -> ModuleResult<Option<(String, u64, String)>> {
    let mut required = None;
    let mut owners = BTreeSet::new();
    for policy in &settings.finalize_policies {
        let owner = &policy.owner_provider;
        if !owners.insert(owner) {
            continue;
        }
        let Some(generation) = session.resource_generations.get(owner).copied() else {
            continue;
        };
        let mut roots = journal.composition.records().filter(|root| {
            root.state == composition::OutputState::Retained
                && root.output.producer == *owner
                && matches!(&root.output.origin, LifecycleTarget::Session { host_epoch, session_id, generation }
                    if host_epoch == &session.host_epoch && session_id == &session.id && *generation == session.generation)
        });
        if let Some(root) = roots.next() {
            if roots.next().is_some()
                || resource_generation::current_root_generation(journal, root)? != generation
            {
                return Err("resource_root_ambiguous_or_stale".into());
            }
            continue;
        }
        let marker = action_disposition::resolved_at(journal, &session.id, owner, generation)
            .ok_or("resource_removal_proof_missing")?;
        if !action_disposition::root_cleanup_complete(journal, &session.id, owner, generation) {
            return Err("resource_cleanup_unresolved".into());
        }
        if marker.resource_generation != generation
            || marker.owner != *owner
            || session.application_retired
            || session
                .streams
                .values()
                .any(|stream| stream.phase != StreamPhase::Retired)
            || required.is_some()
        {
            return Err("resource_reprepare_not_authorized".into());
        }
        required = Some((
            owner.clone(),
            generation,
            marker.resource.incarnation.clone(),
        ));
    }
    Ok(required)
}

impl Coordinator {
    pub(super) fn snapshot(&self, caller: &str, initiator: &str) -> Value {
        let journal = &self.shared.borrow().value;
        let sessions: Vec<_> = journal.sessions.values().filter(|s| allowed(s,caller,initiator)).map(|s| {
            let streams: Vec<_> = s.streams.iter().map(|(id,stream)| json!({"stream_id":id,"generation":stream.generation,"phase":stream.phase,"candidate":stream.candidate})).collect();
            json!({"session_id":s.id,"generation":s.generation,"phase":s.phase,"application_ref":s.application_ref,"streams":streams})
        }).collect();
        json!({"status":"ok","host_epoch":journal.host_epoch,"sessions":sessions})
    }

    pub(super) fn new_work(
        &self,
        phase: LifecyclePhase,
        target: LifecycleTarget,
        payload: &Value,
    ) -> ModuleResult<Work> {
        if let Some(value) = payload
            .get("client_context")
            .filter(|value| !value.is_null())
        {
            let context: amaterasu_extension_api::ClientContext =
                serde_json::from_value(value.clone()).map_err(|_| "invalid_client_context")?;
            if !context.validate() {
                return Err("invalid_client_context".into());
            }
        }
        let order = self.settings.resolved_participants()?;
        let dependencies = self
            .settings
            .participants
            .iter()
            .map(|participant| (participant.provider.clone(), participant.depends_on.clone()))
            .collect();
        let graph = ProviderGraph {
            order,
            dependencies,
        };
        let mut providers = graph.order.clone();
        if matches!(
            phase,
            LifecyclePhase::StreamRelease | LifecyclePhase::SessionRelease
        ) {
            providers.reverse();
        }
        Ok(Work {
            composition_progress: None,
            compacted_release: false,
            owner_observations: Default::default(),
            processing_decision: None,
            reprepare: None,
            phase,
            target,
            intent: payload
                .get("intent")
                .cloned()
                .map(serde_json::from_value)
                .transpose()
                .map_err(|_| "invalid_intent")?,
            reason: payload
                .get("reason")
                .cloned()
                .map(serde_json::from_value)
                .transpose()
                .map_err(|_| "invalid_reason")?,
            video: payload
                .get("requested_video")
                .cloned()
                .map(serde_json::from_value)
                .transpose()
                .map_err(|_| "invalid_video")?,
            transition_id: identifier()?,
            providers,
            provider_graph: Some(graph),
            cleanup_blockers: BTreeSet::new(),
            index: 0,
            states: BTreeMap::new(),
            candidate: None,
            waiting_retirement: false,
            failure: None,
            failed_preparation: None,
            action_transfer: None,
            recovery_authorized_epoch: None,
        })
    }

    pub(super) fn admit(
        &mut self,
        caller: &str,
        initiator: &str,
        payload: Value,
        key: String,
    ) -> ModuleResult<Value> {
        let mut next = self.shared.borrow().value.clone();
        let operation = payload["operation"].as_str().ok_or("invalid_request")?;
        // A transferred action may mutate a domain shared across sessions. Its
        // domain set is not known until fresh planning, so conservatively fence
        // new preparation until the transfer completes or is reconciled.
        if matches!(operation, "prepare" | "prepare_stream" | "resume_stream")
            && next.receipts.values().any(|receipt| {
                receipt
                    .work
                    .as_ref()
                    .is_some_and(|work| work.action_transfer.is_some())
            })
        {
            return Err("action_transition_in_progress".into());
        }
        let mut work = None;
        let mut wait_target = None;
        let mut response;
        if operation == "prepare" {
            let application_operation_id = payload
                .get("application_operation_id")
                .map(|value| {
                    value
                        .as_str()
                        .filter(|id| valid_id(id))
                        .map(str::to_owned)
                        .ok_or("invalid_application_operation_id")
                })
                .transpose()?;
            if next.sessions.len() >= MAX_SESSIONS {
                return Err("session_capacity_exhausted".into());
            }
            let id = identifier()?;
            let target = LifecycleTarget::Session {
                host_epoch: next.host_epoch.clone(),
                session_id: id.clone(),
                generation: 1,
            };
            if retention::session_expired(&next, &target) {
                return Err("session_identity_expired".into());
            }
            let current =
                self.new_work(LifecyclePhase::SessionPrepare, target.clone(), &payload)?;
            response = json!({"status":"pending","host_epoch":next.host_epoch,"session_id":id,"generation":1,"target":target,"transition_id":current.transition_id});
            next.sessions.insert(
                id.clone(),
                SessionRecord {
                    processing_decision: None,
                    provider_graph: current.provider_graph.clone(),
                    capture_binding_mode: self.settings.capture_binding_mode,
                    host_epoch: next.host_epoch.clone(),
                    id,
                    generation: 1,
                    phase: SessionPhase::Preparing,
                    owner: crate::action_authority::Principal {
                        caller: caller.into(),
                        initiator: initiator.into(),
                    },
                    delegates: self.settings.session_delegates.clone(),
                    application_ref: payload
                        .get("application_ref")
                        .cloned()
                        .unwrap_or(Value::Null),
                    settings_revision: payload
                        .get("settings_revision")
                        .and_then(Value::as_str)
                        .unwrap_or("initial")
                        .into(),
                    application_retired: false,
                    application_retirement_proof: None,
                    streams: BTreeMap::new(),
                    obligations: vec![],
                    required_consumers: None,
                    application: None,
                    application_operation_id,
                    admission_ids: BTreeSet::new(),
                    admission_targets: BTreeMap::new(),
                    prepared_plan_ids: Default::default(),
                    admission_phases: BTreeMap::new(),
                    resource_generations: Default::default(),
                    provider_settings: BTreeMap::new(),
                },
            );
            work = Some(current);
        } else {
            let target: LifecycleTarget =
                serde_json::from_value(payload.get("target").cloned().ok_or("target_required")?)
                    .map_err(|_| "invalid_target")?;
            checked_target(&next, &target, caller, initiator)?;
            let end_after_retirement = operation == "end_session"
                && matches!(&target, LifecycleTarget::Session { session_id, .. }
                    if next.sessions.get(session_id).is_some_and(|session|
                        session.streams.values().all(|stream| stream.phase == StreamPhase::Retired)));
            if matches!(operation, "prepare_stream" | "resume_stream") {
                self.prepare_stream_history(&mut next, &target)?;
            } else if end_after_retirement {
                // Keep all source records if live references or finite
                // capacity prevent optional history reclamation.
                optionally_retire_closed_history(&mut next, |candidate| {
                    self.prepare_stream_history(candidate, &target)
                })?;
            }
            let (session, stream_id) = checked_target(&next, &target, caller, initiator)?;
            let session_id = session.id.clone();
            response = json!({"status":"succeeded","target":target,"plan_revision":payload.get("plan_revision")});
            match operation {
                "prepare_stream" | "resume_stream" => {
                    if stream_id.is_some()
                        || !matches!(session.phase, SessionPhase::Ready | SessionPhase::Paused)
                    {
                        return Err("session_not_ready".into());
                    }
                    if !session.application_ref.is_null()
                        && session.application_ref["kind"] != "desktop"
                        && session.application.is_none()
                    {
                        return Err("application_not_bound".into());
                    }
                    let reprepare = required_reprepare(&next, &self.settings, session)?;
                    if super::resource_generation::released_root_requires_reprepare_except(
                        &next,
                        session,
                        reprepare
                            .as_ref()
                            .map(|(owner, generation, _)| (owner.as_str(), *generation)),
                    ) {
                        return Err("owner_resource_reprepare_required".into());
                    }
                    let mut current =
                        self.new_work(LifecyclePhase::StreamPrepare, target.clone(), &payload)?;
                    if reprepare.is_some()
                        && serde_json::to_value(&current.provider_graph).ok()
                            != serde_json::to_value(&session.provider_graph).ok()
                    {
                        return Err("resource_reprepare_graph_changed".into());
                    }
                    let mode = current.video.as_ref().ok_or("negotiated_video_required")?;
                    if mode.width == 0 || mode.height == 0 || mode.refresh_millihz == 0 {
                        return Err("invalid_video".into());
                    }
                    let stream_id = identifier()?;
                    require_unused_stream_id(session, &stream_id)?;
                    current.target = LifecycleTarget::Stream {
                        host_epoch: next.host_epoch.clone(),
                        session_id: session_id.clone(),
                        stream_id: stream_id.clone(),
                        generation: 1,
                    };
                    if next.closed_streams.scope_fenced(&current.target) {
                        return Err("stream_identifier_retired".into());
                    }
                    let required: BTreeSet<String> = payload
                        .get("consumer_requirements")
                        .cloned()
                        .map(serde_json::from_value)
                        .transpose()
                        .map_err(|_| "invalid_consumers")?
                        .unwrap_or_else(|| ["capture".into(), "input".into()].into());
                    if required.is_empty()
                        || required
                            .iter()
                            .any(|x| !matches!(x.as_str(), "capture" | "input"))
                    {
                        return Err("invalid_consumers".into());
                    }
                    if session.capture_binding_mode != self.settings.capture_binding_mode
                        || (session.capture_binding_mode.is_external()
                            && required != BTreeSet::from(["capture".into()]))
                    {
                        return Err("consumer_policy_changed".into());
                    }
                    if !required.contains("capture") {
                        return Err("capture_consumer_required".into());
                    }
                    if session
                        .required_consumers
                        .as_ref()
                        .is_some_and(|frozen| frozen != &required)
                    {
                        return Err("consumer_policy_changed".into());
                    }
                    next.sessions
                        .get_mut(&session_id)
                        .unwrap()
                        .required_consumers = Some(required.clone());
                    next.sessions.get_mut(&session_id).unwrap().streams.insert(
                        stream_id,
                        StreamRecord {
                            provider_graph: current.provider_graph.clone(),
                            generation: 1,
                            phase: StreamPhase::Preparing,
                            candidate: None,
                            required,
                            acknowledgements: BTreeSet::new(),
                            retirements: BTreeSet::new(),
                            binding_owner: String::new(),
                            preparation_transition: current.transition_id.clone(),
                            snapshot_scopes: None,
                            admitted_live_options: None,
                            provider_settings: Default::default(),
                        },
                    );
                    response = json!({"status":"pending","target":current.target,"transition_id":current.transition_id});
                    if let Some((owner, old_generation, old_incarnation)) = reprepare {
                        let generation = old_generation
                            .checked_add(1)
                            .ok_or("resource_generation_exhausted")?;
                        let mut preparation = self.new_work(
                            LifecyclePhase::SessionPrepare,
                            target.clone(),
                            &payload,
                        )?;
                        preparation.providers = vec![owner.clone()];
                        preparation.intent = None;
                        preparation.reason = None;
                        preparation.video = None;
                        preparation.reprepare = Some(ReprepareContinuation {
                            owner: owner.clone(),
                            old_generation,
                            old_incarnation,
                            next: Box::new(current),
                        });
                        let session = next
                            .sessions
                            .get_mut(&session_id)
                            .ok_or("unknown_session")?;
                        session.resource_generations.insert(owner, generation);
                        session.phase = SessionPhase::Preparing;
                        work = Some(preparation);
                    } else {
                        work = Some(current);
                    }
                }
                "validate_candidate" => {
                    let stream = session
                        .streams
                        .get(stream_id.as_deref().ok_or("stream_target_required")?)
                        .unwrap();
                    if stream.phase != StreamPhase::Ready
                        || stream
                            .candidate
                            .as_ref()
                            .map(serde_json::to_value)
                            .transpose()
                            .map_err(|_| "invalid_binding")?
                            != payload.get("candidate").cloned()
                    {
                        return Err("candidate_mismatch".into());
                    }
                }
                "ack_stream" => {
                    if !session.capture_binding_mode.is_external()
                        || !self.settings.consumers.capture.contains(caller)
                        || payload.get("consumer").and_then(Value::as_str) != Some("capture")
                    {
                        return Err("permission_denied".into());
                    }
                    let stream = next
                        .sessions
                        .get_mut(&session_id)
                        .unwrap()
                        .streams
                        .get_mut(stream_id.as_deref().ok_or("stream_target_required")?)
                        .unwrap();
                    if stream.phase != StreamPhase::Ready
                        || stream.required != BTreeSet::from(["capture".into()])
                    {
                        return Err("invalid_stream_state".into());
                    }
                    stream.acknowledgements.insert("capture".into());
                }
                "ack_binding" => {
                    return self
                        .admit_binding_validation(caller, initiator, payload, key, target, true);
                }
                "binding_validate" => {
                    return self
                        .admit_binding_validation(caller, initiator, payload, key, target, false);
                }
                "start_stream" => {
                    let stream = next
                        .sessions
                        .get_mut(&session_id)
                        .unwrap()
                        .streams
                        .get_mut(stream_id.as_deref().ok_or("stream_target_required")?)
                        .unwrap();
                    if stream.phase != StreamPhase::Ready
                        || stream.acknowledgements != stream.required
                    {
                        return Err("consumers_not_acknowledged".into());
                    }
                    stream.phase = StreamPhase::Active;
                    response["all_consumers_acknowledged"] = json!(true);
                }
                "pause_stream" | "capture_invalidated" | "resource_invalidated" => {
                    let stream_id = stream_id.ok_or("stream_target_required")?;
                    let stream = next
                        .sessions
                        .get_mut(&session_id)
                        .unwrap()
                        .streams
                        .get_mut(&stream_id)
                        .unwrap();
                    let plan_revision = stream
                        .candidate
                        .as_ref()
                        .map(|binding| binding.plan_revision);
                    if let Some(expected) = payload.get("plan_revision")
                        && plan_revision.is_some_and(|revision| *expected != json!(revision))
                    {
                        return Err("plan_revision_mismatch".into());
                    }
                    if stream.phase == StreamPhase::Quiescing {
                        response = json!({"status":"pending","target":target,"fenced":true,"plan_revision":plan_revision});
                        wait_target = Some(target.clone());
                    } else {
                        if !matches!(
                            stream.phase,
                            StreamPhase::Active | StreamPhase::Ready | StreamPhase::Preparing
                        ) {
                            return Err("invalid_stream_state".into());
                        }
                        stream.phase = StreamPhase::Quiescing;
                        let mut current =
                            self.new_work(LifecyclePhase::StreamRelease, target.clone(), &payload)?;
                        current.use_release_graph(stream.provider_graph.clone());
                        current.reason = Some(ReleaseReason::Pause);
                        current.waiting_retirement = true;
                        self.fence_stream(&mut next, &target)?;
                        response = json!({"status":"pending","target":target,"fenced":true,"transition_id":current.transition_id,"plan_revision":plan_revision});
                        work = Some(current);
                    }
                }
                "retire_stream" => {
                    let stream_id = stream_id.ok_or("stream_target_required")?;
                    let stream = next
                        .sessions
                        .get_mut(&session_id)
                        .unwrap()
                        .streams
                        .get_mut(&stream_id)
                        .unwrap();
                    if stream.phase != StreamPhase::Quiescing {
                        return Err("stream_not_fenced".into());
                    }
                    let evidence = payload
                        .get("retirement_receipt")
                        .ok_or("retirement_receipt_required")?;
                    if evidence["response"]["state"] != "not_found"
                        && evidence["response"]["status"] != "not_found"
                        && evidence["response"]["code"] != "not_found"
                    {
                        return Err("native_retirement_unconfirmed".into());
                    }
                    // Only an installed capture adapter receives this operation grant.
                    let consumer = payload
                        .get("consumer")
                        .and_then(Value::as_str)
                        .unwrap_or("capture");
                    if consumer != "capture" || !self.settings.consumers.capture.contains(caller) {
                        return Err("permission_denied".into());
                    }
                    stream.retirements.insert(consumer.into());
                    // Native retirement permits cleanup; it is not itself proof
                    // that provider restoration has completed.
                    response["status"] = json!("pending");
                    wait_target = Some(target.clone());
                }
                "end_session" => {
                    if stream_id.is_some()
                        || matches!(
                            session.phase,
                            SessionPhase::Ending
                                | SessionPhase::Ended
                                | SessionPhase::RecoveryRequired
                        )
                    {
                        return Err("invalid_session_state".into());
                    }
                    let session = next.sessions.get_mut(&session_id).unwrap();
                    session.phase = SessionPhase::Ending;
                    for stream in session.streams.values_mut() {
                        if stream.phase != StreamPhase::Retired {
                            stream.phase = StreamPhase::Quiescing;
                        }
                    }
                    let retiring: Vec<_> = session
                        .streams
                        .iter()
                        .filter(|(_, s)| s.phase != StreamPhase::Retired)
                        .map(|(id, s)| (id.clone(), s.generation))
                        .collect();
                    for (stream_id, generation) in retiring {
                        let stream_target = LifecycleTarget::Stream {
                            host_epoch: next.host_epoch.clone(),
                            session_id: session_id.clone(),
                            stream_id: stream_id.clone(),
                            generation,
                        };
                        self.fence_stream(&mut next, &stream_target)?;
                        if !next.receipts.values().any(|r| {
                            r.work.as_ref().is_some_and(|w| {
                                w.target == stream_target
                                    && w.phase == LifecyclePhase::StreamRelease
                            })
                        }) {
                            let mut release = self.new_work(
                                LifecyclePhase::StreamRelease,
                                stream_target.clone(),
                                &json!({}),
                            )?;
                            let graph = next.sessions[&session_id].streams[&stream_id]
                                .provider_graph
                                .clone();
                            release.use_release_graph(graph);
                            release.reason = Some(ReleaseReason::End);
                            release.waiting_retirement = true;
                            let operation_id = identifier()?;
                            let request = json!({"operation":"pause_stream","operation_id":operation_id,"target":stream_target});
                            next.receipts.insert(receipt_key(caller,initiator,&operation_id),Receipt{caller:caller.into(),initiator:initiator.into(),request,response:json!({"status":"pending","operation_id":operation_id,"target":stream_target,"fenced":true}),work:Some(release),wait_target:None});
                        }
                    }
                    let mut current =
                        self.new_work(LifecyclePhase::SessionRelease, target.clone(), &payload)?;
                    current.use_release_graph(next.sessions[&session_id].provider_graph.clone());
                    current.reason = Some(ReleaseReason::End);
                    current.waiting_retirement = true;
                    response = json!({"status":"pending","target":target,"fenced":true,"transition_id":current.transition_id});
                    work = Some(current);
                }
                "bind_application" | "application_retired" => {
                    return self.application_operation(caller, initiator, payload, key, target);
                }
                _ => return Err("unsupported_operation".into()),
            }
        }
        response["operation_id"] = payload["operation_id"].clone();
        next.receipts.insert(
            key,
            Receipt {
                caller: caller.into(),
                initiator: initiator.into(),
                request: payload,
                response: response.clone(),
                work,
                wait_target,
            },
        );
        save(&self.shared, next)?;
        Ok(response)
    }
}

#[cfg(test)]
mod reprepare_tests {
    use super::*;

    #[test]
    fn optional_history_retirement_preserves_source_when_pinned() {
        let mut journal: Journal = serde_json::from_value(json!({
            "version":3,"host_epoch":"epoch","receipts":{},"sessions":{}
        }))
        .unwrap();
        for reason in [
            "closed_stream_history_in_use",
            "closed_stream_history_not_ready",
            "control_replay_fence_capacity",
        ] {
            optionally_retire_closed_history(&mut journal, |candidate| {
                candidate.host_epoch = "partial".into();
                Err(reason.into())
            })
            .unwrap();
            assert_eq!(journal.host_epoch, "epoch");
        }
        assert!(
            optionally_retire_closed_history(&mut journal, |candidate| {
                candidate.host_epoch = "partial".into();
                Err("corrupt_history".into())
            })
            .is_err()
        );
        assert_eq!(journal.host_epoch, "epoch");
        optionally_retire_closed_history(&mut journal, |candidate| {
            candidate.host_epoch = "committed".into();
            Ok(())
        })
        .unwrap();
        assert_eq!(journal.host_epoch, "committed");
    }

    #[test]
    fn existing_stream_identifier_cannot_be_replaced() {
        let session: SessionRecord = serde_json::from_value(json!({
            "host_epoch":"epoch", "id":"session", "generation":1, "phase":"paused",
            "owner":{"caller":"caller","initiator":"client"}, "delegates":[],
            "application_ref":null, "settings_revision":"one", "application_retired":false,
            "streams":{"existing":{"generation":1,"phase":"retired","candidate":null,
                "required":[],"acknowledgements":[],"retirements":[]}}
        }))
        .unwrap();
        assert!(require_unused_stream_id(&session, "existing").is_err());
        assert!(require_unused_stream_id(&session, "new").is_ok());
        assert_eq!(session.streams.len(), 1);
    }

    #[test]
    fn missing_removal_proof_fences_reprepare_without_changing_cursor() {
        let path = std::env::temp_dir().join(format!("reprepare-proof-{}", identifier().unwrap()));
        std::fs::create_dir(&path).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let coordinator = Coordinator::open(&path.join("journal.json")).unwrap();
        let epoch = coordinator.shared.borrow().value.host_epoch.clone();
        let mut journal = coordinator.shared.borrow().value.clone();
        let session: SessionRecord = serde_json::from_value(json!({
            "host_epoch": epoch, "id":"session", "generation":1, "phase":"paused",
            "owner":{"caller":"caller","initiator":"client"}, "delegates":[],
            "application_ref":null, "settings_revision":"one", "application_retired":false,
            "streams":{}, "resource_generations":{"owner":1}
        }))
        .unwrap();
        journal.sessions.insert("session".into(), session);
        let method = json!({"service":"owner/2","method":"finalize","interface_major":2,"interface_minor":0,"schema_revision":1});
        let mut settings = LifecycleSettings::default();
        settings.finalize_policies.push(
            serde_json::from_value(json!({
                "owner_provider":"owner", "owner_service":"owner/2", "method":method,
                "status_method":method, "output_capability":"display",
                "output_schema":{"id":"output/1","major":1,"minor":0,"digest":"a".repeat(64)},
                "capture_provider":"capture", "selector_schema":"selector",
                "required_access":"exclusive", "domain_scope":"resource", "mutation_domains":[]
            }))
            .unwrap(),
        );
        let before = journal.sessions["session"].resource_generations.clone();
        assert_eq!(
            required_reprepare(&journal, &settings, &journal.sessions["session"]).unwrap_err(),
            "resource_removal_proof_missing"
        );
        assert_eq!(journal.sessions["session"].resource_generations, before);
        let session_target = LifecycleTarget::Session {
            host_epoch: epoch.clone(),
            session_id: "session".into(),
            generation: 1,
        };
        let stream_target = LifecycleTarget::Stream {
            host_epoch: epoch,
            session_id: "session".into(),
            stream_id: "stream".into(),
            generation: 1,
        };
        let graph = ProviderGraph {
            order: vec!["owner".into()],
            dependencies: [("owner".into(), vec![])].into(),
        };
        let mut child = coordinator
            .new_work(LifecyclePhase::StreamPrepare, stream_target, &json!({}))
            .unwrap();
        child.provider_graph = Some(graph.clone());
        let mut parent = coordinator
            .new_work(LifecyclePhase::SessionPrepare, session_target, &json!({}))
            .unwrap();
        parent.provider_graph = Some(graph);
        parent.providers = vec!["owner".into()];
        parent.reprepare = Some(ReprepareContinuation {
            owner: "owner".into(),
            old_generation: 1,
            old_incarnation: "native".into(),
            next: Box::new(child),
        });
        assert!(parent.valid_reprepare_shape());
        assert!(!parent.reprepare_authorized(&journal));
        parent.reprepare.as_mut().unwrap().next.target = LifecycleTarget::Stream {
            host_epoch: "foreign".into(),
            session_id: "session".into(),
            stream_id: "stream".into(),
            generation: 1,
        };
        assert!(!parent.valid_reprepare_shape());
        std::fs::remove_dir_all(path).unwrap();
    }
}
