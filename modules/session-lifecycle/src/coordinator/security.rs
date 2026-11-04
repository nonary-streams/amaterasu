use super::*;
use crate::input_authority::{ActiveBinding, Outcome, ReservationResult};
use sha2::{Digest, Sha256};
use std::time::{Duration, Instant};

pub(super) struct ValidationJob {
    key: String,
    target: LifecycleTarget,
    binding: CaptureBinding,
    observation: Value,
    consumer: Option<String>,
    provider: String,
    request: Option<u64>,
    result: Option<Value>,
    operation_id: String,
    deadline: Instant,
    durable: bool,
    input_token: Option<String>,
    resource_generation: Option<u64>,
}
pub(super) struct RevocationJob {
    request: Option<u64>,
    result: Option<Value>,
    operation_id: String,
}

pub(super) fn fingerprint(
    request: &amaterasu_extension_api::ProviderRequest,
) -> ModuleResult<String> {
    let bytes = serde_json::to_vec(request).map_err(|e| e.to_string())?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}
fn target_for(binding: &ActiveBinding) -> LifecycleTarget {
    LifecycleTarget::Stream {
        host_epoch: binding.host_epoch.clone(),
        session_id: binding.session_id.clone(),
        stream_id: binding.stream_id.clone(),
        generation: binding.generation,
    }
}
// Binding metadata comes from a completed owner finalization and the exact
// retained producer callback, not from the consumer's request.
fn binding_resource_generation(
    journal: &Journal,
    target: &LifecycleTarget,
    binding: &CaptureBinding,
    provider: &str,
) -> ModuleResult<u64> {
    let (source, source_binding, _) =
        super::join::validation_source(journal, target, binding, provider)?;
    if &source != target {
        return binding_resource_generation(journal, &source, &source_binding, provider);
    }
    let (epoch, session_id, stream_id, generation) = operations::target_parts(target)?;
    let session = journal
        .sessions
        .get(session_id)
        .ok_or("resource_session_missing")?;
    let stream = stream_id
        .and_then(|id| session.streams.get(id))
        .ok_or("resource_stream_missing")?;
    if journal.host_epoch != epoch
        || session.host_epoch != epoch
        || stream.generation != generation
        || stream.binding_owner != provider
        || stream.candidate.as_ref() != Some(binding)
    {
        return Err("resource_binding_stale".into());
    }
    let mut results = journal.finalize_results.records().filter(|result| {
        result.request.target == *target
            && result.request.transition_id == stream.preparation_transition
            && result.selection.policy.owner_provider == provider
            && matches!(&result.applied, amaterasu_extension_api::ResourceControlResponse::Applied { candidate: Some(candidate), .. } if candidate.as_ref() == binding)
    });
    let result = results.next().ok_or("resource_finalize_missing")?;
    if results.next().is_some()
        || !result.validate_structure()
        || !result.validate_control_chain(journal)
    {
        return Err("resource_finalize_invalid".into());
    }
    let mut roots = journal.composition.records().filter(|root| {
        root.admission_id == result.selection.input.admission_id
            && root.transition_id == result.selection.input.transition_id
            && root.output == result.selection.input.output
            && root.output.producer == provider
    });
    let root = roots.next().ok_or("resource_root_missing")?;
    if roots.next().is_some() {
        return Err("resource_root_ambiguous".into());
    }
    resource_generation::current_root_generation(journal, root)
}

fn normalized_observation(value: &Value) -> ModuleResult<Value> {
    if !value.is_object()
        || serde_json::to_vec(value)
            .map_err(|_| "invalid_native_observation")?
            .len()
            > 8192
    {
        return Err("invalid_native_observation".into());
    }
    // Platform selectors and native evidence belong to the selected provider.
    // Preserve its complete observation; the coordinator only compares the
    // common negotiated mode and enforces the exact binding and caller.
    Ok(value.clone())
}

impl Coordinator {
    pub(super) fn read_provider_context(&self, caller: &str, payload: Value) -> Value {
        let Ok(query) =
            serde_json::from_value::<amaterasu_extension_api::ProviderContextRequest>(payload)
        else {
            return rejection("invalid_request");
        };
        if query.operation != "read_provider_context" {
            return rejection("invalid_request");
        }
        let request = query.provider_request;
        let Ok((epoch, session_id, _, _)) = super::operations::target_parts(&request.target) else {
            return rejection("invalid_target");
        };
        let Ok(digest) = fingerprint(&request) else {
            return rejection("invalid_request");
        };
        let verification = json!({"host_epoch":epoch,"provider_sequence":request.provider_sequence,
            "operation_id":request.operation_id,"transition_id":request.transition_id,
            "target":request.target,"fingerprint":digest});
        if self.verify_provider_request(caller, &verification, false)["status"] != "succeeded" {
            return rejection("permission_denied");
        }
        let shared = self.shared.borrow();
        let Some(session) = shared.value.sessions.get(session_id) else {
            return rejection("unknown_session");
        };
        let mut receipts = shared.value.receipts.values().filter(|receipt| {
            receipt.work.as_ref().is_some_and(|work| {
                work.target == request.target && work.transition_id == request.transition_id
            })
        });
        let Some(receipt) = receipts.next() else {
            return rejection("context_unavailable");
        };
        if receipts.next().is_some() {
            return rejection("context_ambiguous");
        }
        let client = match receipt.request.get("client_context") {
            None | Some(Value::Null) => None,
            Some(value) => match serde_json::from_value::<amaterasu_extension_api::ClientContext>(
                value.clone(),
            ) {
                Ok(client) if client.validate() => Some(client),
                _ => return rejection("invalid_client_context"),
            },
        };
        json!(amaterasu_extension_api::ProviderContextResponse {
            status: "succeeded".into(),
            context: amaterasu_extension_api::ProviderContext {
                target: request.target,
                application: session.application_ref.clone(),
                requested_video: request.requested_video,
                client,
            },
        })
    }

    pub(super) fn verify_provider(&self, caller: &str, payload: &Value) -> Value {
        self.verify_provider_request(caller, payload, true)
    }

    // Context reads need the same authenticated request checks as metadata
    // reads, but do not consume the potentially large recovery metadata.
    fn verify_provider_request(
        &self,
        caller: &str,
        payload: &Value,
        include_metadata: bool,
    ) -> Value {
        let journal = &self.shared.borrow().value;
        for receipt in journal.receipts.values() {
            if receipt.response["status"] != "pending" {
                continue;
            }
            let Some(work) = &receipt.work else {
                continue;
            };
            if payload["host_epoch"] != journal.host_epoch
                && work.recovery_authorized_epoch.as_deref() != Some(journal.host_epoch.as_str())
            {
                continue;
            }
            if work.waiting_retirement {
                continue;
            }
            let nested = work
                .composition_progress
                .as_ref()
                .and_then(|progress| progress.finalize.as_ref())
                .filter(|finalize| {
                    finalize.owner_plan.request.operation_id
                        == payload["operation_id"].as_str().unwrap_or("")
                });
            let observation = work.owner_observations.iter().find(|(_, observation)| {
                observation.request.operation_id == payload["operation_id"].as_str().unwrap_or("")
            });
            let (request, participant, provider) = if let Some((observation_key, observation)) =
                observation
            {
                let Some(dispatch) = observation.dispatch.as_ref() else {
                    continue;
                };
                let selected = self.selections.get(caller);
                let release_context_valid = if work.phase == LifecyclePhase::StreamRelease {
                    work.providers.get(work.index).is_some_and(|step| {
                        serde_json::from_str::<(String, String)>(observation_key)
                            .ok()
                            .as_ref()
                            == Some(&(step.clone(), caller.to_owned()))
                            && self
                                .resolve_release_observation_binding(work, step)
                                .is_ok_and(|(_, output)| {
                                    observation.request.dependency_inputs == [output]
                                })
                    })
                } else {
                    true
                };
                if !release_context_valid
                    || observation.participant.provider != caller
                    || observation.phase != super::plan_observation::PlanObservationPhase::Ready
                    || !matches!(
                        dispatch.state,
                        crate::provider_dispatch::DispatchState::OutcomeUnknown
                    )
                    || !super::observation_scheduler::validate_work_observations(journal, work)
                    || selected.is_none_or(|selection| {
                        !selection.approvals.iter().any(|approval| {
                            approval.method == observation.participant.plan_method
                                && approval.broker_approval == dispatch.call.approval
                                && super::composition::approval_matches_call(
                                    approval,
                                    &dispatch.call,
                                )
                        })
                    })
                {
                    continue;
                }
                (
                    &observation.request,
                    &observation.participant,
                    observation.participant.provider.as_str(),
                )
            } else if let Some(finalize) = nested {
                let state = &finalize.owner_plan;
                let selection = &finalize.selection;
                let selected = self.selections.get(caller);
                if state.provider != caller
                    || selection.policy.owner_provider != caller
                    || state.request.operation
                        != amaterasu_extension_api::ProviderOperation::ResourceFinalizePlan
                    || !matches!(
                        state.phase,
                        crate::prepare_driver::PreparePhase::Plan
                            | crate::prepare_driver::PreparePhase::PlanStatus
                    )
                    || state.request.target != work.target
                    || state.request.transition_id != work.transition_id
                    || selection.target != work.target
                    || selection.transition_id != work.transition_id
                    || if state.request.control_results.is_empty() {
                        state.request.dependency_inputs != vec![selection.current_output().clone()]
                    } else {
                        !super::composition_finalize::resolve_control_chain(
                            journal, work, selection,
                        )
                        .is_ok_and(|(output, controls)| {
                            state.request.dependency_inputs == vec![output]
                                && state.request.control_results == controls
                        })
                    }
                    || state.request.settings_revision != state.participant.settings_revision
                    || serde_json::to_value(&state.request.settings).ok().as_ref()
                        != Some(&state.participant.settings)
                    || !selection.validate_structure()
                    || super::finalize_backend::authenticated_current_output(
                        &journal.composition,
                        selection,
                    )
                    .is_err()
                    || journal.composition.finalize_selection(
                        &selection.target,
                        &selection.transition_id,
                        caller,
                        &selection.input.output.output_id,
                    ) != Some(selection)
                    || journal.composition.get(
                        caller,
                        &selection.input.output.output_id,
                        &selection.input.output.origin,
                    ) != Some(&selection.input)
                    || !self.settings.finalize_policies.contains(&selection.policy)
                    || selection
                        .schema_documents
                        .iter()
                        .any(|doc| !self.settings.schema_documents.contains(doc))
                    || selected.is_none_or(|selected| {
                        !selected.approvals.contains(&selection.finalize_approval)
                            || !selected
                                .approvals
                                .contains(&selection.finalize_status_approval)
                            || !selected.approvals.iter().any(|approval| {
                                approval.method == state.participant.plan_method
                                    && approval.broker_approval["provider"].as_str() == Some(caller)
                            })
                    })
                {
                    continue;
                }
                (&state.request, &state.participant, state.provider.as_str())
            } else {
                let Some(state) = work.states.get(caller) else {
                    continue;
                };
                (&state.request, &state.participant, state.provider.as_str())
            };
            if provider != caller
                || request.target != work.target
                || request.transition_id != work.transition_id
                || request.phase != work.phase
            {
                continue;
            }
            if request.phase == LifecyclePhase::SessionPrepare
                && request.operation == amaterasu_extension_api::ProviderOperation::ResourcePlan
                && journal
                    .sessions
                    .get(
                        super::operations::target_parts(&request.target)
                            .ok()
                            .map(|parts| parts.1)
                            .unwrap_or(""),
                    )
                    .is_some_and(|session| {
                        super::provider_observations::new_session_domains(journal, session, caller)
                            .is_ok_and(|domains| !domains.is_empty())
                    })
                && work.states.get(caller).is_none_or(|state| {
                    state.dispatch.as_ref().is_none_or(|dispatch| {
                        self.selections.get(caller).is_none_or(|selection| {
                            !selection.approvals.iter().any(|approval| {
                                approval.method == participant.plan_method
                                    && approval.broker_approval == dispatch.call.approval
                                    && super::composition::approval_matches_call(
                                        approval,
                                        &dispatch.call,
                                    )
                            })
                        })
                    })
                })
            {
                continue;
            }
            let Ok((target_epoch, session_id, stream_id, generation)) =
                super::operations::target_parts(&request.target)
            else {
                continue;
            };
            if payload["host_epoch"].as_str() != Some(target_epoch) {
                continue;
            }
            let Some(session) = journal.sessions.get(session_id) else {
                continue;
            };
            if session.host_epoch != target_epoch
                || match stream_id {
                    Some(id) => session
                        .streams
                        .get(id)
                        .is_none_or(|stream| stream.generation != generation),
                    None => session.generation != generation,
                }
            {
                continue;
            }
            if nested.is_some()
                && (session.host_epoch != target_epoch
                    || stream_id
                        .and_then(|id| session.streams.get(id))
                        .is_none_or(|stream| stream.generation != generation)
                    || nested.is_some_and(|finalize| {
                        finalize.selection.parent_generation != Some(session.generation)
                    }))
            {
                continue;
            }
            let phase_allowed = match request.phase {
                LifecyclePhase::SessionPrepare => session.phase == SessionPhase::Preparing,
                LifecyclePhase::StreamPrepare => stream_id
                    .and_then(|id| session.streams.get(id))
                    .is_some_and(|s| s.phase == StreamPhase::Preparing),
                LifecyclePhase::StreamRelease => stream_id
                    .and_then(|id| session.streams.get(id))
                    .is_some_and(|s| {
                        s.phase == StreamPhase::Quiescing && s.retirements == s.required
                    }),
                LifecyclePhase::SessionRelease => {
                    session.phase == SessionPhase::Ending
                        && session.application_retired
                        && session
                            .streams
                            .values()
                            .all(|s| s.phase == StreamPhase::Retired)
                }
            };
            if !phase_allowed {
                continue;
            }
            // The cursor is coordinator-owned and frozen into the exact
            // persisted provider request. It exists before the first output;
            // subsequent work must match its current session value.
            let resource_owner = participant.contract_version
                == amaterasu_extension_api::COMPOSITION_VERSION
                && self
                    .settings
                    .finalize_policies
                    .iter()
                    .any(|policy| policy.owner_provider == caller);
            let resource_generation = if resource_owner {
                match session.resource_generations.get(caller).copied() {
                    Some(generation)
                        if generation > 0 && request.resource_generation == Some(generation) =>
                    {
                        Some(generation)
                    }
                    _ => continue,
                }
            } else if request.resource_generation.is_some() {
                continue;
            } else {
                None
            };
            if payload["provider_sequence"] != request.provider_sequence
                || payload["operation_id"] != request.operation_id
                || payload["transition_id"] != request.transition_id
                || payload["target"] != json!(request.target)
                || fingerprint(request).ok().as_deref() != payload["fingerprint"].as_str()
            {
                continue;
            }
            if request.operation == amaterasu_extension_api::ProviderOperation::LifecycleCallback
                && request
                    .admission_id
                    .as_ref()
                    .is_none_or(|id| !journal.authority.admissions.contains_key(id))
            {
                continue;
            }
            if !include_metadata {
                return json!({"status":"succeeded"});
            }
            let Ok(metadata) = super::provider_observations::metadata(journal, session, caller)
            else {
                return rejection("provider_metadata_unavailable");
            };
            let mut result = payload.clone();
            if let (Some(destination), Some(fields)) =
                (result.as_object_mut(), metadata.as_object())
            {
                destination.remove("resource_generation");
                destination.extend(fields.clone());
            } else {
                return rejection("invalid_provider_request");
            }
            result["status"] = json!("succeeded");
            if let Some(generation) = resource_generation {
                result["resource_generation"] = json!(generation);
            }
            result["session_target"] = json!({"kind":"session","host_epoch":session.host_epoch,"session_id":session.id,"generation":session.generation});
            result["consumers_retired"] = json!(session.streams.iter().all(|(id, stream)| {
                (stream_id == Some(id.as_str()) && stream.phase == StreamPhase::Preparing)
                    || stream.phase == StreamPhase::Retired
                    || (stream.phase == StreamPhase::Quiescing
                        && stream.retirements == stream.required)
            }));
            if serde_json::to_vec(&result).map_or(true, |bytes| bytes.len() > 16 * 1024) {
                return rejection("provider_metadata_too_large");
            }
            return result;
        }
        rejection("unknown_provider_admission")
    }

    pub(super) fn admit_binding_validation(
        &mut self,
        caller: &str,
        initiator: &str,
        payload: Value,
        key: String,
        target: LifecycleTarget,
        ack: bool,
    ) -> ModuleResult<Value> {
        let mut next = self.shared.borrow().value.clone();
        let (session, stream_id) =
            super::operations::checked_target(&next, &target, caller, initiator)?;
        let stream = session
            .streams
            .get(stream_id.as_deref().ok_or("stream_target_required")?)
            .unwrap();
        if (ack && stream.phase != StreamPhase::Ready)
            || (!ack && stream.phase != StreamPhase::Active)
        {
            return Err("stream_not_ready".into());
        }
        let binding = stream
            .candidate
            .as_ref()
            .ok_or("candidate_missing")?
            .clone();
        let candidate_field = if ack { "candidate" } else { "binding" };
        if payload.get(candidate_field) != Some(&json!(binding))
            || payload["plan_revision"] != binding.plan_revision
        {
            return Err("candidate_mismatch".into());
        }
        let consumer = if ack {
            Some(
                payload["consumer"]
                    .as_str()
                    .ok_or("consumer_required")?
                    .to_owned(),
            )
        } else {
            None
        };
        let input_ack = consumer.as_deref() == Some("input");
        if consumer
            .as_deref()
            .is_some_and(|kind| !matches!(kind, "capture" | "input"))
        {
            return Err("unknown_consumer".into());
        }
        if if input_ack {
            !self.settings.consumers.input.contains(caller)
        } else {
            !self.settings.consumers.capture.contains(caller)
        } {
            return Err("permission_denied".into());
        }
        let input_token = if input_ack {
            Some(
                next.input_bindings
                    .iter()
                    .find(|(_, b)| target_for(b) == target)
                    .map(|(token, _)| token.clone())
                    .ok_or("input_binding_not_issued")?,
            )
        } else {
            None
        };
        let observation = if input_ack {
            Value::Null
        } else {
            let raw = if ack {
                &payload["observed_capture"]
            } else {
                &payload["observation"]
            };
            let observed = normalized_observation(raw)?;
            if observed["width"] != binding.mode.width
                || observed["height"] != binding.mode.height
                || observed["refresh_millihz"] != binding.mode.refresh_millihz
            {
                return Err("observed_mode_mismatch".into());
            }
            observed
        };
        let provider = stream.binding_owner.clone();
        let response = json!({"status":"pending","operation_id":payload["operation_id"],"target":target,"plan_revision":binding.plan_revision});
        if self.validations.len() >= 128 {
            return Err("validation_capacity_exhausted".into());
        }
        let receipt = Receipt {
            caller: caller.into(),
            initiator: initiator.into(),
            request: payload,
            response: response.clone(),
            work: None,
            wait_target: None,
        };
        if ack {
            next.receipts.insert(key.clone(), receipt);
            save(&self.shared, next)?;
        } else {
            if self.ephemeral.len() >= 512 {
                let oldest = self
                    .ephemeral
                    .iter()
                    .filter(|(_, (r, _))| r.response["status"] != "pending")
                    .min_by_key(|(_, (_, at))| *at)
                    .map(|(k, _)| k.clone());
                if let Some(oldest) = oldest {
                    self.ephemeral.remove(&oldest);
                } else {
                    return Err("validation_capacity_exhausted".into());
                }
            }
            self.ephemeral
                .insert(key.clone(), (receipt, Instant::now()));
        }
        self.validations.insert(
            key.clone(),
            ValidationJob {
                key,
                target,
                binding,
                observation,
                consumer,
                provider,
                request: None,
                result: None,
                operation_id: identifier()?,
                deadline: Instant::now() + Duration::from_secs(5),
                durable: ack,
                input_token,
                resource_generation: None,
            },
        );
        Ok(response)
    }

    pub(super) fn advance_validations(
        &mut self,
        send: &mut dyn FnMut(Command) -> ModuleResult<Response>,
    ) -> ModuleResult<()> {
        self.advance_revocations(send)?;
        let keys: Vec<_> = self.validations.keys().take(8).cloned().collect();
        for key in keys {
            let mut job = self.validations.remove(&key).unwrap();
            if Instant::now() >= job.deadline {
                if let Some(id) = job.request {
                    let _ = send(Command::Cancel { request: id });
                    let _ = send(Command::ReleaseResult { request: id });
                }
                self.finish_validation(&job, None)?;
                continue;
            }
            if let Some(result) = job.result.take() {
                if let Some(id) = job.request
                    && !matches!(
                        send(Command::ReleaseResult { request: id }),
                        Ok(Response::Ok)
                    )
                {
                    job.result = Some(result);
                    self.validations.insert(key, job);
                    continue;
                }
                self.finish_validation(&job, Some(result))?;
                continue;
            }
            if let Some(id) = job.request {
                match send(Command::TakeResult { request: id }) {
                    Ok(Response::Pending) => {}
                    Ok(Response::Result { payload }) => job.result = Some(payload),
                    _ => {
                        let _ = send(Command::ReleaseResult { request: id });
                        self.finish_validation(&job, None)?;
                        continue;
                    }
                }
            } else {
                if let Some(token) = &job.input_token {
                    match send(Command::Request {
                        service: "amaterasu.input/1".into(),
                        payload: json!({"action":"binding_status","operation_id":job.operation_id,"token":token,"target":job.target}),
                        timeout_ms: 5000,
                        operation_id: format!("input-status-{}", job.operation_id),
                        parent: None,
                    }) {
                        Ok(Response::Accepted { id }) => job.request = Some(id),
                        _ => {
                            self.finish_validation(&job, None)?;
                            continue;
                        }
                    }
                    self.validations.insert(key, job);
                    continue;
                }
                let Some(selection) = self.selections.get(&job.provider) else {
                    self.validations.insert(key, job);
                    continue;
                };
                let Some(method) = &selection.participant.binding_validate_method else {
                    self.finish_validation(&job, None)?;
                    continue;
                };
                let source = super::join::validation_source(
                    &self.shared.borrow().value,
                    &job.target,
                    &job.binding,
                    &job.provider,
                );
                let Ok((provider_target, provider_binding, transition)) = source else {
                    self.finish_validation(&job, None)?;
                    continue;
                };
                let mut payload = json!({"operation_id":job.operation_id,"transition_id":transition,"target":provider_target,"plan_revision":provider_binding.plan_revision,"binding":provider_binding,"observation":job.observation});
                if selection.participant.contract_version
                    == amaterasu_extension_api::COMPOSITION_VERSION
                {
                    let generation = binding_resource_generation(
                        &self.shared.borrow().value,
                        &job.target,
                        &job.binding,
                        &job.provider,
                    );
                    let Ok(generation) = generation else {
                        self.finish_validation(&job, None)?;
                        continue;
                    };
                    payload["resource_generation"] = json!(generation);
                    job.resource_generation = Some(generation);
                }
                match send(Command::CallApi {
                    service: method.service.clone(),
                    method: method.method.clone(),
                    interface_major: method.interface_major,
                    interface_minor: method.interface_minor,
                    payload,
                    timeout_ms: 5000,
                    operation_id: format!("binding-{}", job.operation_id),
                    parent: None,
                }) {
                    Ok(Response::Accepted { id }) => job.request = Some(id),
                    _ => {
                        self.finish_validation(&job, None)?;
                        continue;
                    }
                }
            }
            self.validations.insert(key, job);
        }
        Ok(())
    }

    fn finish_validation(
        &mut self,
        job: &ValidationJob,
        result: Option<Value>,
    ) -> ModuleResult<()> {
        let mut next = self.shared.borrow().value.clone();
        let generation_current = job.resource_generation.is_none_or(|expected| {
            binding_resource_generation(&next, &job.target, &job.binding, &job.provider)
                == Ok(expected)
        });
        let provider_source =
            super::join::validation_source(&next, &job.target, &job.binding, &job.provider).ok();
        let (_, session_id, stream_id, generation) = super::operations::target_parts(&job.target)?;
        let stream = next
            .sessions
            .get_mut(session_id)
            .and_then(|s| s.streams.get_mut(stream_id.unwrap()))
            .ok_or("unknown_stream")?;
        let valid = result.as_ref().is_some_and(|r| {
            if let Some(token) = &job.input_token {
                r["ok"] == true
                    && r["status"] == "bound"
                    && r["token"] == *token
                    && r["target"] == json!(job.target)
                    && r["geometry_revision"] == job.binding.geometry_revision
                    && r["lease"].as_str().is_some_and(|s| !s.is_empty())
            } else {
                r["status"] == "succeeded"
                    && provider_source
                        .as_ref()
                        .is_some_and(|(target, binding, _)| {
                            r["target"] == json!(target) && r["binding"] == json!(binding)
                        })
                    && r["observation"] == job.observation
                    && r["plan_revision"] == job.binding.plan_revision
            }
        }) && generation_current
            && stream.generation == generation
            && stream.candidate.as_ref() == Some(&job.binding)
            && if job.consumer.is_some() {
                stream.phase == StreamPhase::Ready
            } else {
                stream.phase == StreamPhase::Active
            };
        if valid && let Some(consumer) = &job.consumer {
            stream.acknowledgements.insert(consumer.clone());
        }
        let receipt = if job.durable {
            next.receipts
                .get_mut(&job.key)
                .ok_or("unknown_validation")?
        } else {
            &mut self
                .ephemeral
                .get_mut(&job.key)
                .ok_or("unknown_validation")?
                .0
        };
        receipt.response = if valid {
            json!({"status":"succeeded","operation_id":receipt.request["operation_id"],"target":job.target,"plan_revision":job.binding.plan_revision,"binding":job.binding,"observation":job.observation})
        } else {
            json!({"status":"rejected","operation_id":receipt.request["operation_id"],"reason":"binding_validation_failed"})
        };
        if job.durable {
            save(&self.shared, next)
        } else {
            Ok(())
        }
    }

    pub(super) fn fence_stream(
        &mut self,
        next: &mut Journal,
        target: &LifecycleTarget,
    ) -> ModuleResult<()> {
        self.ephemeral.retain(|_, (receipt, _)| {
            receipt.request["target"] != json!(target) || receipt.response["status"] == "pending"
        });
        let bindings: Vec<_> = next
            .input_bindings
            .iter()
            .filter(|(_, b)| target_for(b) == *target)
            .map(|(t, b)| (t.clone(), b.clone()))
            .collect();
        let no_input = bindings.is_empty();
        for (token, binding) in bindings {
            next.inputs.fence(&binding);
            next.revoked_inputs.insert(token);
        }
        let (epoch, session, stream, generation) = super::operations::target_parts(target)?;
        let action_target = crate::action_authority::ActionTarget::Stream {
            host_epoch: epoch.into(),
            session_id: session.into(),
            stream_id: stream.ok_or("stream_target_required")?.into(),
            expected_generation: generation,
        };
        next.actions
            .fence_target(&action_target)
            .map_err(|_| "action_fence_failed")?;
        if no_input
            && let Some(record) = next
                .sessions
                .get_mut(session)
                .and_then(|s| s.streams.get_mut(stream.unwrap()))
            && record.required.contains("input")
        {
            record.retirements.insert("input".into());
        }
        Ok(())
    }

    fn advance_revocations(
        &mut self,
        send: &mut dyn FnMut(Command) -> ModuleResult<Response>,
    ) -> ModuleResult<()> {
        let tokens: Vec<_> = self
            .shared
            .borrow()
            .value
            .revoked_inputs
            .iter()
            .take(8)
            .cloned()
            .collect();
        for token in tokens {
            let binding = self
                .shared
                .borrow()
                .value
                .input_bindings
                .get(&token)
                .cloned();
            let Some(binding) = binding else {
                continue;
            };
            let target = target_for(&binding);
            let transfer = {
                let shared = self.shared.borrow();
                shared.value.receipts.values().find_map(|receipt| {
                    let work = receipt.work.as_ref()?;
                    let transfer = work.action_transfer.as_ref()?;
                    if work.target != target || receipt.response["status"] != "pending" {
                        return None;
                    }
                    let reservation_id = transfer.input_reservation_id.as_ref()?;
                    let reservation = shared.value.inputs.resolve_running_reservation(reservation_id).ok()?;
                    (reservation.token == token).then(|| json!({"operation_id":transfer.binding.operation_id,"reservation_id":reservation_id}))
                })
            };
            let job = self
                .revocations
                .entry(token.clone())
                .or_insert(RevocationJob {
                    request: None,
                    result: None,
                    operation_id: identifier()?,
                });
            if let Some(result) = job.result.take() {
                if let Some(id) = job.request
                    && !matches!(
                        send(Command::ReleaseResult { request: id }),
                        Ok(Response::Ok)
                    )
                {
                    job.result = Some(result);
                    continue;
                }
                if result["status"] == "retired"
                    && result["token"] == token
                    && result["target"] == json!(target)
                {
                    let mut next = self.shared.borrow().value.clone();
                    if let Some(stream) = next
                        .sessions
                        .get_mut(&binding.session_id)
                        .and_then(|s| s.streams.get_mut(&binding.stream_id))
                    {
                        stream.retirements.insert("input".into());
                    }
                    next.input_bindings.remove(&token);
                    next.input_boot_epochs.remove(&token);
                    next.revoked_inputs.remove(&token);
                    save(&self.shared, next)?;
                    self.revocations.remove(&token);
                } else {
                    self.fail_revocation(&token)?;
                }
            } else if let Some(id) = job.request {
                match send(Command::TakeResult { request: id }) {
                    Ok(Response::Pending) => {}
                    Ok(Response::Result { payload }) => job.result = Some(payload),
                    _ => self.fail_revocation(&token)?,
                }
            } else {
                let response = send(Command::Request {
                    service: "amaterasu.input/1".into(),
                    payload: json!({"action":"revoke_binding","operation_id":job.operation_id,"token":token,"target":target,"transfer":transfer}),
                    timeout_ms: 5000,
                    operation_id: format!("revoke-{}", job.operation_id),
                    parent: None,
                });
                if let Ok(Response::Accepted { id }) = response {
                    job.request = Some(id);
                } else {
                    self.fail_revocation(&token)?;
                }
            }
        }
        Ok(())
    }

    fn fail_revocation(&mut self, token: &str) -> ModuleResult<()> {
        let mut next = self.shared.borrow().value.clone();
        if let Some(binding) = next.input_bindings.get(token) {
            if let Some(stream) = next
                .sessions
                .get_mut(&binding.session_id)
                .and_then(|session| session.streams.get_mut(&binding.stream_id))
            {
                stream.phase = StreamPhase::RecoveryRequired;
            }
            let target = target_for(binding);
            for receipt in next.receipts.values_mut() {
                if receipt.request["target"] == json!(target)
                    || (super::refresh_receipt::is_compact(receipt)
                        && receipt.response["target"] == json!(target))
                {
                    receipt.response["status"] = json!("recovery_required");
                    receipt.response["reason"] = json!("input_retirement_unconfirmed");
                }
            }
        }
        next.revoked_inputs.remove(token);
        save(&self.shared, next)?;
        self.revocations.remove(token);
        Ok(())
    }

    pub(super) fn security_operation(
        &mut self,
        caller: &str,
        initiator: &str,
        payload: &Value,
    ) -> Value {
        if matches!(
            payload["operation"].as_str(),
            Some(
                "authorize_action"
                    | "consume_action_permit"
                    | "complete_action"
                    | "begin_action_transition"
                    | "rebind_action_plan"
                    | "consume_input_reservation"
                    | "cancel_input_reservation"
                    | "inspect_action_effect"
                    | "verify_action_effect"
                    | "check_action_effect"
            )
        ) {
            return self.action_operation(caller, initiator, payload);
        }
        match self.input_operation(caller, initiator, payload) {
            Ok(value) => value,
            Err(reason) => rejection(&reason),
        }
    }

    fn input_operation(
        &mut self,
        caller: &str,
        initiator: &str,
        payload: &Value,
    ) -> ModuleResult<Value> {
        let mut next = self.shared.borrow().value.clone();
        let operation = payload["operation"].as_str().ok_or("invalid_request")?;
        // Existing reservations must be able to drain after their generation is
        // fenced. Revocation closes new admission, not completion of owned work.
        if operation == "finish_input_action" || operation == "lookup_input_action" {
            let id = payload["operation_id"]
                .as_str()
                .ok_or("operation_id_required")?;
            if operation == "lookup_input_action" {
                return serde_json::to_value(
                    next.inputs
                        .lookup_input_action(caller, initiator, id)
                        .map_err(|_| "unknown_reservation")?,
                )
                .map_err(|e| e.to_string());
            }
            let outcome = if payload["outcome"] == "completed" {
                Outcome::Completed
            } else if payload["outcome"] == "cancelled" {
                Outcome::Cancelled
            } else {
                Outcome::RecoveryRequired
            };
            let reservation_id = payload["reservation_id"]
                .as_str()
                .ok_or("reservation_required")?;
            let stored = next
                .inputs
                .resolve_reservation(reservation_id)
                .map_err(|_| "unknown_reservation")?;
            if stored.caller != caller || stored.operation_id != id {
                return Err("permission_denied".into());
            }
            let result = next
                .inputs
                .finish_input_action(caller, &stored.initiator, id, reservation_id, outcome)
                .map_err(|_| "input_reservation_rejected")?;
            save(&self.shared, next)?;
            return Ok(json!({"status":"succeeded","reservation_id":result.reservation_id}));
        }
        if operation == "issue_input_binding" {
            let id = payload["operation_id"]
                .as_str()
                .filter(|id| valid_id(id))
                .ok_or("operation_id_required")?;
            let key = receipt_key(caller, initiator, id);
            if let Some(old) = next.receipts.get(&key) {
                return if old.request == *payload {
                    Ok(old.response.clone())
                } else {
                    Err("operation_id_conflict".into())
                };
            }
            let target: LifecycleTarget =
                serde_json::from_value(payload["target"].clone()).map_err(|_| "invalid_target")?;
            let (session, stream_id) =
                super::operations::checked_target(&next, &target, caller, initiator)?;
            let stream_id = stream_id.ok_or("stream_target_required")?;
            let stream = &session.streams[&stream_id];
            if !matches!(stream.phase, StreamPhase::Ready | StreamPhase::Active)
                || !stream.acknowledgements.contains("capture")
            {
                return Err("capture_not_acknowledged".into());
            }
            let binding = stream.candidate.as_ref().ok_or("candidate_missing")?;
            let plan_revision = binding.plan_revision;
            // The installed policy selects the broker identity, never the request.
            let input_consumer = match self.settings.input_provider.as_ref() {
                Some(provider) if self.settings.consumers.input.contains(provider) => provider,
                Some(_) => return Err("input_consumer_unavailable".into()),
                None if self.settings.consumers.input.len() == 1 => self
                    .settings
                    .consumers
                    .input
                    .iter()
                    .next()
                    .expect("one consumer"),
                None if self.settings.consumers.input.is_empty() => {
                    return Err("input_consumer_unavailable".into());
                }
                None => return Err("input_consumer_ambiguous".into()),
            };
            let active = ActiveBinding {
                host_epoch: session.host_epoch.clone(),
                session_id: session.id.clone(),
                stream_id,
                generation: stream.generation,
                geometry_revision: binding.geometry_revision,
                input_consumer: input_consumer.clone(),
                allowed_actions: self.settings.input_actions.clone(),
            };
            let token = next
                .inputs
                .issue_input_binding(&active)
                .map_err(|_| "input_binding_unavailable")?;
            next.input_bindings.insert(token.clone(), active.clone());
            let result = json!({"status":"succeeded","operation_id":id,"token":token,"target":target,"geometry_revision":active.geometry_revision,"plan_revision":plan_revision});
            next.receipts.insert(
                key,
                Receipt {
                    caller: caller.into(),
                    initiator: initiator.into(),
                    request: payload.clone(),
                    response: result.clone(),
                    work: None,
                    wait_target: None,
                },
            );
            save(&self.shared, next)?;
            return Ok(result);
        }
        let token = payload["token"].as_str().ok_or("binding_token_required")?;
        if next.revoked_inputs.contains(token) {
            return Err("binding_revoked".into());
        }
        let binding = next
            .input_bindings
            .get(token)
            .cloned()
            .ok_or("unknown_input_binding")?;
        let session = next
            .sessions
            .get(&binding.session_id)
            .ok_or("unknown_session")?;
        let stream = session
            .streams
            .get(&binding.stream_id)
            .ok_or("unknown_stream")?;
        if binding.host_epoch != next.host_epoch
            || binding.host_epoch != session.host_epoch
            || binding.generation != stream.generation
            || !matches!(stream.phase, StreamPhase::Ready | StreamPhase::Active)
        {
            return Err("stale_input_binding".into());
        }
        if operation == "validate_input_binding" {
            next.inputs
                .validate_input_binding(caller, token, &binding)
                .map_err(|_| "permission_denied")?;
            let response = json!({"status":"succeeded","active":stream.phase==StreamPhase::Active,"target":target_for(&binding),"action_target":{"kind":"session","host_epoch":session.host_epoch,"session_id":session.id,"expected_generation":session.generation},"context":"active","application":session.application_ref["app_id"],"paired_client":null,"geometry_revision":binding.geometry_revision,"batch_epoch":format!("{}.{}.{}",binding.host_epoch,binding.stream_id,binding.generation),"allowed_actions":binding.allowed_actions});
            let boot_epoch = payload["input_boot_epoch"]
                .as_str()
                .filter(|epoch| valid_id(epoch))
                .or_else(|| (!self.configured).then_some("unit-test-input"))
                .ok_or("input_boot_epoch_required")?;
            if let Some(previous) = next.input_boot_epochs.get(token) {
                if previous != boot_epoch {
                    return Err("input_provider_restarted".into());
                }
            } else {
                next.input_boot_epochs
                    .insert(token.into(), boot_epoch.into());
                save(&self.shared, next)?;
            }
            return Ok(response);
        }
        if stream.phase != StreamPhase::Active {
            return Err("stream_not_active".into());
        }
        let id = payload["operation_id"]
            .as_str()
            .ok_or("operation_id_required")?;
        let result = match operation {
            "reserve_input_action" => {
                let action = payload["action_id"].as_str().ok_or("action_required")?;
                use crate::action_authority::ActionTarget;
                let session_target = ActionTarget::Session {
                    host_epoch: session.host_epoch.clone(),
                    session_id: session.id.clone(),
                    expected_generation: session.generation,
                };
                let stream_target = ActionTarget::Stream {
                    host_epoch: binding.host_epoch.clone(),
                    session_id: binding.session_id.clone(),
                    stream_id: binding.stream_id.clone(),
                    expected_generation: binding.generation,
                };
                let target = if payload.get("target").is_none() {
                    session_target.clone()
                } else {
                    serde_json::from_value(payload["target"].clone())
                        .map_err(|_| "invalid_target")?
                };
                if target != session_target && target != stream_target {
                    return Err("input_action_target_mismatch".into());
                }
                let reservation = next
                    .inputs
                    .reserve_input_action_for_target(
                        caller, initiator, id, token, action, &binding, &target,
                    )
                    .map_err(|_| "input_reservation_rejected")?;
                let (ReservationResult::Admitted(r) | ReservationResult::Existing(r)) = reservation;
                json!({"status":"reserved","operation_id":id,"reservation_id":r.reservation_id,"target":r.target,"action_id":action})
            }
            "lookup_input_action" => serde_json::to_value(
                next.inputs
                    .lookup_input_action(caller, initiator, id)
                    .map_err(|_| "unknown_reservation")?,
            )
            .map_err(|e| e.to_string())?,
            "finish_input_action" => {
                let outcome = if payload["outcome"] == "completed" {
                    Outcome::Completed
                } else if payload["outcome"] == "cancelled" {
                    Outcome::Cancelled
                } else {
                    Outcome::RecoveryRequired
                };
                let r = next
                    .inputs
                    .finish_input_action(
                        caller,
                        initiator,
                        id,
                        payload["reservation_id"]
                            .as_str()
                            .ok_or("reservation_required")?,
                        outcome,
                    )
                    .map_err(|_| "input_reservation_rejected")?;
                json!({"status":"succeeded","reservation_id":r.reservation_id})
            }
            _ => return Err("unsupported_operation".into()),
        };
        save(&self.shared, next)?;
        Ok(result)
    }
}

#[cfg(test)]
mod resource_generation_tests {
    use super::*;

    #[test]
    fn provider_epoch_uses_durable_owner_cursor_before_any_output() {
        let path = std::env::temp_dir().join(format!(
            "amaterasu-cursor-{}-{}",
            std::process::id(),
            identifier().unwrap()
        ));
        std::fs::create_dir(&path).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let mut coordinator = Coordinator::open(&path.join("journal.json")).unwrap();
        let epoch = coordinator.shared.borrow().value.host_epoch.clone();
        let target =
            json!({"kind":"session","host_epoch":epoch,"session_id":"session","generation":3});
        let method = json!({"service":"owner/2","method":"plan","interface_major":2,"interface_minor":0,"schema_revision":1});
        let schema = json!({"id":"schema","major":1,"minor":0,"digest":"a".repeat(64)});
        coordinator.settings.finalize_policies.push(serde_json::from_value(json!({"owner_provider":"owner","owner_service":"owner/2","method":method,"status_method":method,"output_capability":"output","output_schema":schema,"capture_provider":"capture","selector_schema":"selector","required_access":"exclusive","mutation_domains":["domain"]})).unwrap());
        let participant = json!({"provider":"owner","contract_version":2,"depends_on":[],"plan_method":method,"callback_method":method,"status_method":method,"capabilities":[],"cleanup_methods":[],"settings_revision":"rev","settings":{}});
        let request = json!({"version":2,"provider_sequence":1,"operation":"resource_plan","transition_id":"transition","operation_id":"owner-plan","target":target,"resource_generation":1,"phase":"session.prepare","settings_revision":"rev","settings":{},"intent":null,"reason":null,"requested_video":null,"consumer_requirements":{"execution_domain":"seat","capture_providers":["capture"],"selector_schemas":["selector"]},"remaining_ms":5000,"plan_revision":null,"admission_id":null,"approved_plan":null});
        let state = json!({"provider":"owner","participant":participant,"request":request,"phase":"plan","plan":null,"admission_id":null,"pending_receipt":null,"status_polls":0,"last_sequence":1,"dispatch":null,"response":null});
        let work = json!({"phase":"session.prepare","target":target,"intent":null,"reason":null,"video":null,"transition_id":"transition","providers":["owner"],"index":0,"states":{"owner":state},"candidate":null,"waiting_retirement":false});
        let session = json!({"host_epoch":epoch,"id":"session","generation":3,"phase":"preparing","owner":{"caller":"transport","initiator":"user"},"delegates":[],"application_ref":{},"settings_revision":"rev","application_retired":false,"streams":{},"resource_generations":{"owner":1}});
        let mut journal = coordinator.shared.borrow().value.clone();
        journal
            .sessions
            .insert("session".into(), serde_json::from_value(session).unwrap());
        journal.receipts.insert("work".into(), serde_json::from_value(json!({"caller":"transport","initiator":"user","request":{"client_context":{"client_ref":"client-1","display_mode":{"width":1920,"height":1080,"refresh_millihz":59940},"properties":{"vendor_option":{"enabled":false,"rate":0}}}},"response":{"status":"pending"},"work":work})).unwrap());
        save(&coordinator.shared, journal).unwrap();
        let typed: amaterasu_extension_api::ProviderRequest =
            serde_json::from_value(request).unwrap();
        let verification = json!({"host_epoch":epoch,"provider_sequence":1,"operation_id":"owner-plan","transition_id":"transition","target":target,"fingerprint":fingerprint(&typed).unwrap(),"resource_generation":99});
        let response = coordinator.verify_provider("owner", &verification);
        assert_eq!(response["status"], "succeeded");
        let context_request =
            json!({"operation":"read_provider_context", "provider_request":typed});
        let context = coordinator.read_provider_context("owner", context_request.clone());
        assert_eq!(context["status"], "succeeded");
        assert_eq!(context["context"]["target"], target);
        assert_eq!(context["context"]["application"], json!({}));
        assert_eq!(context["context"]["client"]["client_ref"], "client-1");
        assert_eq!(
            context["context"]["client"]["properties"],
            json!({"vendor_option":{"enabled":false,"rate":0}})
        );
        assert_eq!(
            context["context"]["client"]["display_mode"]["refresh_millihz"],
            59940
        );
        assert_eq!(
            coordinator.read_provider_context("foreign", context_request.clone())["status"],
            "rejected"
        );
        // Large recovery metadata must not deny an otherwise valid read-only
        // context request. The full metadata API still enforces its bound.
        let original_journal = coordinator.shared.borrow().value.clone();
        let mut crowded = original_journal.clone();
        let session = crowded.sessions.get_mut("session").unwrap();
        for index in 0..129 {
            session.obligations.push(
                serde_json::from_value(json!({
                    "provider":"owner", "resource_id":format!("resource-{index}"),
                    "obligation_id":format!("cleanup-{index}"),
                    "cleanup_method":method, "depends_on":[], "mutation_domain":null,
                    "payload":{}
                }))
                .unwrap(),
            );
        }
        save(&coordinator.shared, crowded).unwrap();
        assert_eq!(
            coordinator.verify_provider("owner", &verification)["reason"],
            "provider_metadata_unavailable"
        );
        assert_eq!(
            coordinator.read_provider_context("owner", context_request.clone())["status"],
            "succeeded"
        );
        let mut altered = context_request.clone();
        altered["provider_request"]["provider_sequence"] = json!(2);
        assert_eq!(
            coordinator.read_provider_context("owner", altered)["reason"],
            "permission_denied"
        );
        save(&coordinator.shared, original_journal).unwrap();
        let mut forged = context_request;
        forged["provider_request"]["requested_video"] =
            json!({"width":1920,"height":1080,"refresh_millihz":60000,"hdr":false});
        assert_eq!(
            coordinator.read_provider_context("owner", forged)["status"],
            "rejected"
        );
        assert_eq!(response["resource_generation"], 1);
        assert_eq!(
            coordinator.verify_provider("foreign", &verification)["status"],
            "rejected"
        );
        let original = coordinator.shared.borrow().value.clone();
        let mut stream_journal = original.clone();
        let stream_target: LifecycleTarget = serde_json::from_value(json!({"kind":"stream","host_epoch":epoch,"session_id":"session","stream_id":"stream","generation":4})).unwrap();
        stream_journal.sessions.get_mut("session").unwrap().streams.insert("stream".into(), serde_json::from_value(json!({"generation":4,"phase":"preparing","candidate":null,"required":[],"acknowledgements":[],"retirements":[]})).unwrap());
        let work = stream_journal
            .receipts
            .get_mut("work")
            .unwrap()
            .work
            .as_mut()
            .unwrap();
        work.phase = LifecyclePhase::StreamPrepare;
        work.target = stream_target.clone();
        let stream_request = &mut work.states.get_mut("owner").unwrap().request;
        stream_request.phase = LifecyclePhase::StreamPrepare;
        stream_request.target = stream_target.clone();
        let stream_verification = json!({"host_epoch":epoch,"provider_sequence":1,"operation_id":"owner-plan","transition_id":"transition","target":stream_target,"fingerprint":fingerprint(stream_request).unwrap()});
        let stream_context_request =
            json!({"operation":"read_provider_context", "provider_request":stream_request});
        stream_journal.receipts.get_mut("work").unwrap().request["client_context"] = json!({
            "client_ref":"resumed-client", "display_mode":null
        });
        save(&coordinator.shared, stream_journal).unwrap();
        assert_eq!(
            coordinator.read_provider_context("owner", stream_context_request)["context"]["client"]
                ["client_ref"],
            "resumed-client"
        );
        assert_eq!(
            coordinator.verify_provider("owner", &stream_verification)["resource_generation"],
            1
        );
        // Cleanup callbacks must also authorize with crowded history, but
        // only once all required consumers have retired.
        let prepared_stream = coordinator.shared.borrow().value.clone();
        let mut cleanup = prepared_stream.clone();
        let session = cleanup.sessions.get_mut("session").unwrap();
        for index in 0..129 {
            session.obligations.push(
                serde_json::from_value(json!({
                    "provider":"owner", "resource_id":format!("resource-{index}"),
                    "obligation_id":format!("cleanup-{index}"),
                    "cleanup_method":method, "depends_on":[], "mutation_domain":null,
                    "payload":{}
                }))
                .unwrap(),
            );
        }
        let stream = session.streams.get_mut("stream").unwrap();
        stream.phase = StreamPhase::Quiescing;
        stream.required.insert("capture".into());
        stream.retirements.insert("capture".into());
        let work = cleanup
            .receipts
            .get_mut("work")
            .unwrap()
            .work
            .as_mut()
            .unwrap();
        work.phase = LifecyclePhase::StreamRelease;
        let request = &mut work.states.get_mut("owner").unwrap().request;
        request.phase = LifecyclePhase::StreamRelease;
        let cleanup_context =
            json!({"operation":"read_provider_context", "provider_request":request});
        save(&coordinator.shared, cleanup.clone()).unwrap();
        assert_eq!(
            coordinator.read_provider_context("owner", cleanup_context.clone())["status"],
            "succeeded"
        );
        cleanup
            .sessions
            .get_mut("session")
            .unwrap()
            .streams
            .get_mut("stream")
            .unwrap()
            .retirements
            .clear();
        save(&coordinator.shared, cleanup).unwrap();
        assert_eq!(
            coordinator.read_provider_context("owner", cleanup_context)["reason"],
            "permission_denied"
        );
        save(&coordinator.shared, prepared_stream).unwrap();
        let mut wrong_stream = coordinator.shared.borrow().value.clone();
        wrong_stream
            .sessions
            .get_mut("session")
            .unwrap()
            .streams
            .get_mut("stream")
            .unwrap()
            .generation += 1;
        save(&coordinator.shared, wrong_stream).unwrap();
        assert_eq!(
            coordinator.verify_provider("owner", &stream_verification)["status"],
            "rejected"
        );
        save(&coordinator.shared, original).unwrap();
        let mut changed = coordinator.shared.borrow().value.clone();
        changed
            .sessions
            .get_mut("session")
            .unwrap()
            .resource_generations
            .insert("owner".into(), 2);
        save(&coordinator.shared, changed).unwrap();
        assert_eq!(
            coordinator.verify_provider("owner", &verification)["status"],
            "rejected"
        );
        drop(coordinator);
        std::fs::remove_dir_all(path).unwrap();
    }
}

#[cfg(test)]
mod binding_generation_tests {
    use super::*;

    fn fixture() -> (Journal, LifecycleTarget, CaptureBinding) {
        let target = json!({"kind":"stream","host_epoch":"epoch","session_id":"session","stream_id":"stream","generation":2});
        let binding = json!({"capture_provider":"capture","selector_schema":"selector/1","selector":{},"resource":{"provider":"owner","resource_id":"display","incarnation":"native","revision":2,"execution_domain":"seat","mutation_domain":null},"mode":{"width":1920,"height":1080,"refresh_millihz":60000,"hdr":false},"plan_revision":1,"geometry_revision":2,"generation":2});
        let session = json!({"host_epoch":"epoch","id":"session","generation":1,"phase":"ready","owner":{"caller":"transport","initiator":"client"},"delegates":[],"application_ref":null,"settings_revision":"revision","application_retired":false,"streams":{"stream":{"generation":2,"phase":"ready","candidate":binding,"binding_owner":"owner","required":[],"acknowledgements":[],"retirements":[]}},"resource_generations":{"owner":7}});
        (serde_json::from_value(json!({"version":3,"host_epoch":"epoch","receipts":{},"sessions":{"session":session}})).unwrap(), serde_json::from_value(target).unwrap(), serde_json::from_value(binding).unwrap())
    }

    #[test]
    fn no_finalized_owner_evidence_rejects_binding_generation() {
        let (journal, target, binding) = fixture();
        assert_eq!(
            binding_resource_generation(&journal, &target, &binding, "owner").unwrap_err(),
            "resource_finalize_missing"
        );
    }

    #[test]
    fn stale_target_binding_or_owner_never_selects_generation() {
        let (mut journal, target, mut binding) = fixture();
        assert_eq!(
            binding_resource_generation(&journal, &target, &binding, "other").unwrap_err(),
            "resource_binding_stale"
        );
        binding.geometry_revision += 1;
        assert_eq!(
            binding_resource_generation(&journal, &target, &binding, "owner").unwrap_err(),
            "resource_binding_stale"
        );
        binding.geometry_revision -= 1;
        journal.host_epoch = "new-epoch".into();
        assert_eq!(
            binding_resource_generation(&journal, &target, &binding, "owner").unwrap_err(),
            "resource_binding_stale"
        );
    }
}
