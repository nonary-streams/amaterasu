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

pub(super) struct ApplicationJob {
    key: String,
    target: LifecycleTarget,
    binding: ApplicationBinding,
    request: Option<u64>,
    result: Option<Value>,
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
            next.sessions
                .get_mut(&session_id)
                .unwrap()
                .application_retired = true;
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
            if let Some(result) = job.result.take() {
                if let Some(id) = job.request {
                    match send(Command::ReleaseResult { request: id }) {
                        Ok(Response::Ok) => {}
                        Ok(Response::Error { code, .. }) if code == "not_found" => {}
                        _ => {
                            job.result = Some(result);
                            self.application_jobs.insert(key, job);
                            continue;
                        }
                    }
                }
                self.finish_application(&job, Some(result))?;
                continue;
            }
            if Instant::now() >= job.deadline {
                // Cancellation does not prove retirement. Keep the receipt fenced.
                if let Some(id) = job.request {
                    let _ = send(Command::Cancel { request: id });
                }
                if job.request.is_some() {
                    job.result = Some(Value::Null);
                    self.application_jobs.insert(key, job);
                } else {
                    self.finish_application(&job, None)?;
                }
                continue;
            }
            if let Some(id) = job.request {
                match send(Command::TakeResult { request: id }) {
                    Ok(Response::Pending) => {}
                    Ok(Response::Result { payload }) => job.result = Some(payload),
                    _ => {
                        job.result = Some(Value::Null);
                    }
                }
            } else {
                let binding = &job.binding;
                let request = Command::Request {
                    service: "amaterasu.launch/1".into(),
                    payload: json!({"action":"verify_retirement","owner":binding.owner,"launch_id":if job.no_launch {None} else {Some(binding.launch_id)},"operation_id":binding.operation_id}),
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

    fn finish_application(&self, job: &ApplicationJob, result: Option<Value>) -> ModuleResult<()> {
        let mut next = self.shared.borrow().value.clone();
        let (_, session_id, stream, generation) = super::operations::target_parts(&job.target)?;
        if stream.is_some() {
            return Err("session_target_required".into());
        }
        let session = next.sessions.get_mut(session_id).ok_or("unknown_session")?;
        let valid = result.as_ref().is_some_and(|r| {
            r["ok"] == true
                && r["retired"] == true
                && r["owner"] == job.binding.owner
                && if job.no_launch {
                    r["launch_id"].is_null() && r["never_admitted"] == true
                } else {
                    r["launch_id"] == job.binding.launch_id
                }
                && r["operation_id"] == job.binding.operation_id
        }) && session.host_epoch == next.host_epoch
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
                application: None,
                application_operation_id: None,
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
        drop(coordinator);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
