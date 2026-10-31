//! Application attachment and independently verified process retirement.
use super::*;
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ApplicationBinding {
    pub launch_id: u64,
    pub operation_id: String,
    pub owner: String,
}

/// Evidence is saved in the same transaction as the retirement flag. It is
/// historical only and does not authorize a new launch or a native effect.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum ApplicationRetirementProof {
    Desktop {
        target: LifecycleTarget,
    },
    Launch {
        target: LifecycleTarget,
        binding: ApplicationBinding,
        no_launch: bool,
        service: String,
        request_operation_id: String,
        broker_request: u64,
        result_request: u64,
        request_payload: Value,
        response: Value,
    },
}

fn retirement_payload(binding: &ApplicationBinding, no_launch: bool) -> Value {
    json!({"action":"verify_retirement","owner":binding.owner,"launch_id":if no_launch {None} else {Some(binding.launch_id)},"operation_id":binding.operation_id})
}

pub(super) fn valid_retirement_proof(session: &SessionRecord, target: &LifecycleTarget) -> bool {
    if !session.application_retired
        || !matches!(target, LifecycleTarget::Session { host_epoch, session_id, generation }
            if host_epoch == &session.host_epoch && session_id == &session.id && *generation == session.generation)
    {
        return false;
    }
    match &session.application_retirement_proof {
        Some(ApplicationRetirementProof::Desktop {
            target: proof_target,
        }) => {
            proof_target == target
                && session.application.is_none()
                && session.application_operation_id.is_none()
                && session.application_ref.get("kind").and_then(Value::as_str) == Some("desktop")
        }
        Some(ApplicationRetirementProof::Launch {
            target: proof_target,
            binding,
            no_launch,
            service,
            request_operation_id,
            broker_request,
            result_request,
            request_payload,
            response,
        }) => {
            proof_target == target
                && service == "amaterasu.launch/1"
                && *broker_request > 0
                && broker_request == result_request
                && valid_id(request_operation_id)
                && request_operation_id.starts_with("retirement-")
                && *request_payload == retirement_payload(binding, *no_launch)
                && serde_json::to_vec(response).is_ok_and(|bytes| bytes.len() <= 4096)
                && response["ok"] == true
                && response["retired"] == true
                && response["owner"] == binding.owner
                && response["operation_id"] == binding.operation_id
                && if *no_launch {
                    binding.launch_id == 0
                        && session.application.is_none()
                        && session.application_operation_id.as_deref()
                            == Some(binding.operation_id.as_str())
                        && session.owner.caller == binding.owner
                        && response["launch_id"].is_null()
                        && response["never_admitted"] == true
                } else {
                    binding.launch_id > 0
                        && session.application.as_ref() == Some(binding)
                        && response["launch_id"] == binding.launch_id
                }
        }
        None => false,
    }
}

pub(super) struct ApplicationJob {
    key: String,
    target: LifecycleTarget,
    binding: ApplicationBinding,
    request: Option<u64>,
    result: Option<(u64, Value)>,
    query_id: String,
    deadline: Instant,
    no_launch: bool,
}

impl Coordinator {
    pub(super) fn application_operation(
        &mut self,
        caller: &str,
        initiator: &str,
        payload: Value,
        key: String,
        target: LifecycleTarget,
    ) -> ModuleResult<Value> {
        let mut next = self.shared.borrow().value.clone();
        let (session, stream) =
            super::operations::checked_target(&next, &target, caller, initiator)?;
        if stream.is_some() {
            return Err("session_target_required".into());
        }
        let session_id = session.id.clone();
        let operation = payload["operation"].as_str().ok_or("invalid_request")?;
        let operation_id = payload["operation_id"]
            .as_str()
            .ok_or("invalid_operation_id")?;
        let response = if operation == "bind_application" {
            if session.owner.caller != caller || session.owner.initiator != initiator {
                return Err("permission_denied".into());
            }
            if session.phase != SessionPhase::Ready {
                return Err("invalid_session_state".into());
            }
            let launch_id = payload["launch_id"]
                .as_u64()
                .filter(|v| *v > 0)
                .ok_or("invalid_launch_id")?;
            let launch_operation = payload["application_operation_id"]
                .as_str()
                .filter(|v| valid_id(v))
                .ok_or("invalid_application_operation_id")?;
            if session
                .application_operation_id
                .as_deref()
                .is_some_and(|expected| expected != launch_operation)
            {
                return Err("application_operation_mismatch".into());
            }
            // The authenticated caller owns the launch record; payload cannot name an owner.
            let binding = ApplicationBinding {
                launch_id,
                operation_id: launch_operation.into(),
                owner: caller.into(),
            };
            if let Some(existing) = &session.application
                && existing != &binding
            {
                return Err("application_already_bound".into());
            }
            next.sessions.get_mut(&session_id).unwrap().application = Some(binding);
            json!({"status":"succeeded","operation_id":operation_id,"target":target,"launch_id":launch_id,"application_operation_id":launch_operation})
        } else if operation == "application_retired" {
            if session.phase != SessionPhase::Ending {
                return Err("invalid_session_state".into());
            }
            if session.application_retired {
                return Err("application_already_retired".into());
            }
            let no_launch = session.application.is_none();
            let binding = session.application.clone().or_else(|| {
                session
                    .application_operation_id
                    .as_ref()
                    .map(|operation_id| ApplicationBinding {
                        launch_id: 0,
                        operation_id: operation_id.clone(),
                        owner: session.owner.caller.clone(),
                    })
            });
            if let Some(binding) = binding {
                if self.application_jobs.len() >= 128 {
                    return Err("application_capacity_exhausted".into());
                }
                let job = ApplicationJob {
                    key: key.clone(),
                    target: target.clone(),
                    binding,
                    request: None,
                    result: None,
                    query_id: identifier()?,
                    deadline: Instant::now() + Duration::from_secs(5),
                    no_launch,
                };
                let response =
                    json!({"status":"pending","operation_id":operation_id,"target":target});
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
                self.application_jobs.insert(key, job);
                return Ok(response);
            }
            if session.application_ref.get("kind").and_then(Value::as_str) != Some("desktop") {
                return Err("application_retirement_unconfirmed".into());
            }
            let session = next.sessions.get_mut(&session_id).unwrap();
            session.application_retired = true;
            session.application_retirement_proof = Some(ApplicationRetirementProof::Desktop {
                target: target.clone(),
            });
            json!({"status":"succeeded","operation_id":operation_id,"target":target})
        } else {
            return Err("unsupported_operation".into());
        };
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

    pub(super) fn advance_applications(
        &mut self,
        send: &mut dyn FnMut(Command) -> ModuleResult<Response>,
    ) -> ModuleResult<()> {
        let keys: Vec<_> = self.application_jobs.keys().take(8).cloned().collect();
        for key in keys {
            let mut job = self.application_jobs.remove(&key).unwrap();
            if let Some((result_request, result)) = job.result.take() {
                if job.request == Some(result_request) {
                    match send(Command::ReleaseResult {
                        request: result_request,
                    }) {
                        Ok(Response::Ok) => {}
                        Ok(Response::Error { code, .. }) if code == "not_found" => {}
                        _ => {
                            job.result = Some((result_request, result));
                            self.application_jobs.insert(key, job);
                            continue;
                        }
                    }
                }
                self.finish_application(&job, Some((result_request, result)))?;
                continue;
            }
            if Instant::now() >= job.deadline {
                // Cancellation does not prove retirement. Keep the receipt fenced.
                if let Some(id) = job.request {
                    let _ = send(Command::Cancel { request: id });
                }
                if let Some(id) = job.request {
                    job.result = Some((id, Value::Null));
                    self.application_jobs.insert(key, job);
                } else {
                    self.finish_application(&job, None)?;
                }
                continue;
            }
            if let Some(id) = job.request {
                match send(Command::TakeResult { request: id }) {
                    Ok(Response::Pending) => {}
                    Ok(Response::Result { payload }) => job.result = Some((id, payload)),
                    _ => {
                        job.result = Some((id, Value::Null));
                    }
                }
            } else {
                let binding = &job.binding;
                let request = Command::Request {
                    service: "amaterasu.launch/1".into(),
                    payload: retirement_payload(binding, job.no_launch),
                    timeout_ms: 5000,
                    operation_id: format!("retirement-{}", job.query_id),
                    parent: None,
                };
                match send(request) {
                    Ok(Response::Accepted { id }) => job.request = Some(id),
                    _ => {
                        self.finish_application(&job, None)?;
                        continue;
                    }
                }
            }
            self.application_jobs.insert(key, job);
        }
        Ok(())
    }

    fn finish_application(
        &self,
        job: &ApplicationJob,
        result: Option<(u64, Value)>,
    ) -> ModuleResult<()> {
        let mut next = self.shared.borrow().value.clone();
        let (_, session_id, stream, generation) = super::operations::target_parts(&job.target)?;
        if stream.is_some() {
            return Err("session_target_required".into());
        }
        let session = next.sessions.get_mut(session_id).ok_or("unknown_session")?;
        let valid = job.request.is_some_and(|id| id > 0)
            && result.as_ref().is_some_and(|(result_id, r)| {
                Some(*result_id) == job.request
                    && r["ok"] == true
                    && r["retired"] == true
                    && r["owner"] == job.binding.owner
                    && if job.no_launch {
                        r["launch_id"].is_null() && r["never_admitted"] == true
                    } else {
                        r["launch_id"] == job.binding.launch_id
                    }
                    && r["operation_id"] == job.binding.operation_id
            })
            && session.host_epoch == next.host_epoch
            && session.generation == generation
            && session.phase == SessionPhase::Ending
            && if job.no_launch {
                session.application.is_none()
                    && session.application_operation_id.as_deref()
                        == Some(job.binding.operation_id.as_str())
                    && session.owner.caller == job.binding.owner
            } else {
                session.application.as_ref() == Some(&job.binding)
            };
        if valid {
            session.application_retired = true;
            session.application_retirement_proof = Some(ApplicationRetirementProof::Launch {
                target: job.target.clone(),
                binding: job.binding.clone(),
                no_launch: job.no_launch,
                service: "amaterasu.launch/1".into(),
                request_operation_id: format!("retirement-{}", job.query_id),
                broker_request: job.request.expect("validated broker request"),
                result_request: result.as_ref().expect("validated response").0,
                request_payload: retirement_payload(&job.binding, job.no_launch),
                response: result.clone().expect("validated response").1,
            });
            if !valid_retirement_proof(session, &job.target) {
                return Err("invalid_application_retirement_proof".into());
            }
        }
        let receipt = next
            .receipts
            .get_mut(&job.key)
            .ok_or("unknown_application_receipt")?;
        receipt.response = if valid {
            json!({"status":"succeeded","operation_id":receipt.request["operation_id"],"target":job.target})
        } else {
            json!({"status":"recovery_required","operation_id":receipt.request["operation_id"],"reason":"application_retirement_unconfirmed"})
        };
        save(&self.shared, next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    #[test]
    fn retirement_requires_exact_launch_service_confirmation() {
        let dir = std::env::temp_dir().join(format!(
            "amaterasu-application-{}-{}",
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
        let epoch = next.host_epoch.clone();
        next.sessions.insert(
            "session".into(),
            SessionRecord {
                paired_source_attested: false,
                provider_graph: None,
                host_epoch: epoch.clone(),
                id: "session".into(),
                generation: 1,
                phase: SessionPhase::Ready,
                owner: crate::action_authority::Principal {
                    caller: "nvhttp".into(),
                    initiator: "nvhttp".into(),
                },
                delegates: BTreeSet::new(),
                application_ref: json!({"kind":"application"}),
                settings_revision: "1".into(),
                application_retired: false,
                application_retirement_proof: None,
                application: None,
                application_operation_id: None,
                admission_ids: Default::default(),
                admission_targets: Default::default(),
                admission_phases: Default::default(),
                resource_generations: Default::default(),
                provider_settings: Default::default(),
                streams: BTreeMap::new(),
                obligations: vec![],
                required_consumers: None,
            },
        );
        save(&coordinator.shared, next).unwrap();
        let target = LifecycleTarget::Session {
            host_epoch: epoch,
            session_id: "session".into(),
            generation: 1,
        };
        let bind = json!({"operation":"bind_application","operation_id":"bind","launch_id":42,"application_operation_id":"launch"});
        assert_eq!(
            coordinator
                .application_operation(
                    "nvhttp",
                    "nvhttp",
                    bind,
                    receipt_key("nvhttp", "nvhttp", "bind"),
                    target.clone()
                )
                .unwrap()["status"],
            "succeeded"
        );
        let mut next = coordinator.shared.borrow().value.clone();
        next.sessions.get_mut("session").unwrap().phase = SessionPhase::Ending;
        save(&coordinator.shared, next).unwrap();
        let request =
            json!({"operation":"application_retired","operation_id":"retire","retired":true});
        assert_eq!(
            coordinator
                .application_operation(
                    "nvhttp",
                    "nvhttp",
                    request,
                    receipt_key("nvhttp", "nvhttp", "retire"),
                    target.clone()
                )
                .unwrap()["status"],
            "pending"
        );
        let mut calls = 0;
        let mut send = |command| {
            calls += 1;
            match command {
                Command::Request { payload, .. } => {
                    assert_eq!(payload["owner"], "nvhttp");
                    Ok(Response::Accepted { id: 8 })
                }
                Command::TakeResult { .. } => Ok(Response::Result {
                    payload: json!({"ok":true,"retired":true,"owner":"other","launch_id":42,"operation_id":"launch"}),
                }),
                Command::ReleaseResult { .. } => Ok(Response::Ok),
                _ => panic!("unexpected command"),
            }
        };
        for _ in 0..3 {
            coordinator.advance_applications(&mut send).unwrap();
        }
        assert_eq!(calls, 3);
        let journal = coordinator.shared.borrow();
        assert!(!journal.value.sessions["session"].application_retired);
        assert_eq!(
            journal.value.receipts[&receipt_key("nvhttp", "nvhttp", "retire")].response["status"],
            "recovery_required"
        );
        drop(journal);
        let request = json!({"operation":"application_retired","operation_id":"retry"});
        coordinator
            .application_operation(
                "nvhttp",
                "nvhttp",
                request,
                receipt_key("nvhttp", "nvhttp", "retry"),
                target.clone(),
            )
            .unwrap();
        let mut send = |command| match command {
            Command::Request { .. } => Ok(Response::Accepted { id: 9 }),
            Command::TakeResult { .. } => Ok(Response::Result {
                payload: json!({"ok":true,"retired":true,"owner":"nvhttp","launch_id":42,"operation_id":"launch"}),
            }),
            Command::ReleaseResult { .. } => Ok(Response::Ok),
            _ => panic!("unexpected command"),
        };
        for _ in 0..3 {
            coordinator.advance_applications(&mut send).unwrap();
        }
        let stored = coordinator.shared.borrow().value.clone();
        assert!(valid_retirement_proof(&stored.sessions["session"], &target));
        let mut altered = stored.clone();
        if let Some(ApplicationRetirementProof::Launch { response, .. }) = &mut altered
            .sessions
            .get_mut("session")
            .unwrap()
            .application_retirement_proof
        {
            response["launch_id"] = json!(43);
        } else {
            panic!("missing launch proof");
        }
        assert!(validate_journal(&altered).is_err());
        let mut missing_request = stored.clone();
        if let Some(ApplicationRetirementProof::Launch { broker_request, .. }) =
            &mut missing_request
                .sessions
                .get_mut("session")
                .unwrap()
                .application_retirement_proof
        {
            *broker_request = 0;
        } else {
            panic!("missing launch proof");
        }
        assert!(validate_journal(&missing_request).is_err());
        let mut wrong_result = stored.clone();
        if let Some(ApplicationRetirementProof::Launch { result_request, .. }) = &mut wrong_result
            .sessions
            .get_mut("session")
            .unwrap()
            .application_retirement_proof
        {
            *result_request += 1;
        } else {
            panic!("missing launch proof");
        }
        assert!(validate_journal(&wrong_result).is_err());
        let restored: Journal =
            serde_json::from_slice(&serde_json::to_vec(&stored).unwrap()).unwrap();
        assert!(validate_journal(&restored).is_ok());
        assert!(valid_retirement_proof(
            &restored.sessions["session"],
            &target
        ));
        let mut desktop = restored.sessions["session"].clone();
        desktop.application = None;
        desktop.application_operation_id = None;
        desktop.application_ref = json!({"kind":"desktop"});
        desktop.application_retirement_proof = Some(ApplicationRetirementProof::Desktop {
            target: target.clone(),
        });
        assert!(valid_retirement_proof(&desktop, &target));
        desktop.application_operation_id = Some("unsettled-launch".into());
        assert!(!valid_retirement_proof(&desktop, &target));
        desktop.application_retirement_proof = None;
        assert!(!valid_retirement_proof(&desktop, &target));
        drop(coordinator);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
