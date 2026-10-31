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
pub(super) fn checked_target<'a>(
    journal: &'a Journal,
    target: &LifecycleTarget,
    caller: &str,
    initiator: &str,
) -> ModuleResult<(&'a SessionRecord, Option<String>)> {
    let (epoch, id, stream, generation) = target_parts(target)?;
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

impl Coordinator {
    pub(super) fn snapshot(&self, caller: &str, initiator: &str) -> Value {
        let journal = &self.shared.borrow().value;
        let sessions: Vec<_> = journal.sessions.values().filter(|s| allowed(s,caller,initiator)).map(|s| {
            let streams: Vec<_> = s.streams.iter().map(|(id,stream)| json!({"stream_id":id,"generation":stream.generation,"phase":stream.phase,"candidate":stream.candidate})).collect();
            json!({"session_id":s.id,"generation":s.generation,"phase":s.phase,"application_ref":s.application_ref,"streams":streams})
        }).collect();
        json!({"status":"ok","host_epoch":journal.host_epoch,"sessions":sessions})
    }

    fn new_work(
        &self,
        phase: LifecyclePhase,
        target: LifecycleTarget,
        payload: &Value,
    ) -> ModuleResult<Work> {
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
            let current =
                self.new_work(LifecyclePhase::SessionPrepare, target.clone(), &payload)?;
            response = json!({"status":"pending","host_epoch":next.host_epoch,"session_id":id,"generation":1,"target":target,"transition_id":current.transition_id});
            next.sessions.insert(
                id.clone(),
                SessionRecord {
                    provider_graph: current.provider_graph.clone(),
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
                    streams: BTreeMap::new(),
                    obligations: vec![],
                    required_consumers: None,
                    application: None,
                    application_operation_id,
                    admission_ids: BTreeSet::new(),
                    admission_targets: BTreeMap::new(),
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
                    let mut current =
                        self.new_work(LifecyclePhase::StreamPrepare, target.clone(), &payload)?;
                    let mode = current.video.as_ref().ok_or("negotiated_video_required")?;
                    if mode.width == 0 || mode.height == 0 || mode.refresh_millihz == 0 {
                        return Err("invalid_video".into());
                    }
                    let stream_id = identifier()?;
                    current.target = LifecycleTarget::Stream {
                        host_epoch: next.host_epoch.clone(),
                        session_id: session_id.clone(),
                        stream_id: stream_id.clone(),
                        generation: 1,
                    };
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
                            provider_settings: Default::default(),
                        },
                    );
                    response = json!({"status":"pending","target":current.target,"transition_id":current.transition_id});
                    work = Some(current);
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
