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
    completion: Option<ApplicationCompletion>,
    query_id: String,
    deadline: Instant,
    no_launch: bool,
    recovery: Option<SessionRecoveryApplication>,
}

/// Persist a verification readback before releasing its broker ownership. A
/// committed completion retries only release, even after its session has ended.
enum ApplicationCompletion {
    Unconfirmed,
    Result { request: u64, payload: Value },
    Committed,
}

impl ApplicationCompletion {
    fn result(&self) -> Option<(u64, Value)> {
        match self {
            Self::Unconfirmed | Self::Committed => None,
            Self::Result { request, payload } => Some((*request, payload.clone())),
        }
    }
}

struct SessionRecoveryApplication {
    original_key: String,
    transition_id: String,
    authorized_epoch: String,
}

impl ApplicationJob {
    pub(super) fn for_session_recovery(
        key: String,
        target: LifecycleTarget,
        session: &SessionRecord,
        original_key: String,
        transition_id: String,
        authorized_epoch: String,
    ) -> ModuleResult<Self> {
        let no_launch = session.application.is_none();
        let binding = session
            .application
            .clone()
            .or_else(|| {
                session
                    .application_operation_id
                    .as_ref()
                    .map(|operation_id| ApplicationBinding {
                        launch_id: 0,
                        operation_id: operation_id.clone(),
                        owner: session.owner.caller.clone(),
                    })
            })
            .ok_or("application_retirement_unconfirmed")?;
        Ok(Self {
            key,
            target,
            binding,
            no_launch,
            request: None,
            result: None,
            completion: None,
            query_id: identifier()?,
            deadline: Instant::now() + Duration::from_secs(5),
            recovery: Some(SessionRecoveryApplication {
                original_key,
                transition_id,
                authorized_epoch,
            }),
        })
    }
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
                    completion: None,
                    query_id: identifier()?,
                    deadline: Instant::now() + Duration::from_secs(5),
                    no_launch,
                    recovery: None,
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
        let mut keys: Vec<_> = self.application_jobs.keys().cloned().collect();
        if !keys.is_empty() {
            let offset = self.application_cursor % keys.len();
            keys.rotate_left(offset);
            self.application_cursor = (offset + 8.min(keys.len())) % keys.len();
        }
        // Only a failed journal commit leaves an uncommitted completion here.
        // Resolve that global persistence fence before rotating to other work.
        if let Some(index) = keys.iter().position(|key| {
            matches!(
                self.application_jobs[key].completion,
                Some(ApplicationCompletion::Unconfirmed | ApplicationCompletion::Result { .. })
            )
        }) {
            let blocked = keys.remove(index);
            keys.insert(0, blocked);
        }
        for key in keys.into_iter().take(8) {
            let mut job = self.application_jobs.remove(&key).unwrap();
            if let Some(completion) = job.completion.take() {
                self.settle_application_completion(key, job, completion, send)?;
                continue;
            }
            if let Some((result_request, result)) = job.result.take() {
                self.settle_application_completion(
                    key,
                    job,
                    ApplicationCompletion::Result {
                        request: result_request,
                        payload: result,
                    },
                    send,
                )?;
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
                    self.settle_application_completion(
                        key,
                        job,
                        ApplicationCompletion::Unconfirmed,
                        send,
                    )?;
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
                        self.settle_application_completion(
                            key,
                            job,
                            ApplicationCompletion::Unconfirmed,
                            send,
                        )?;
                        continue;
                    }
                }
            }
            self.application_jobs.insert(key, job);
        }
        Ok(())
    }

    fn settle_application_completion(
        &mut self,
        key: String,
        mut job: ApplicationJob,
        completion: ApplicationCompletion,
        send: &mut dyn FnMut(Command) -> ModuleResult<Response>,
    ) -> ModuleResult<()> {
        if !matches!(completion, ApplicationCompletion::Committed)
            && let Err(error) = self.finish_application(&job, completion.result())
        {
            job.completion = Some(completion);
            self.application_jobs.insert(key, job);
            return Err(error);
        }
        if let Some(request) = job.request {
            match send(Command::ReleaseResult { request }) {
                Ok(Response::Ok) => {}
                Ok(Response::Error { code, .. }) if code == "not_found" => {}
                _ => {
                    job.completion = Some(ApplicationCompletion::Committed);
                    self.application_jobs.insert(key, job);
                }
            }
        }
        Ok(())
    }

    fn finish_application(
        &self,
        job: &ApplicationJob,
        result: Option<(u64, Value)>,
    ) -> ModuleResult<()> {
        let mut next = self.shared.borrow().value.clone();
        let (target_epoch, session_id, stream, generation) =
            super::operations::target_parts(&job.target)?;
        if stream.is_some() {
            return Err("session_target_required".into());
        }
        let recovery_authorized = job.recovery.as_ref().is_some_and(|context| {
            context.authorized_epoch == next.host_epoch
                && next
                    .receipts
                    .get(&context.original_key)
                    .is_some_and(|receipt| {
                        receipt.response["status"] == "pending"
                            && receipt.work.as_ref().is_some_and(|work| {
                                work.target == job.target
                                    && work.transition_id == context.transition_id
                                    && work.phase == LifecyclePhase::SessionRelease
                                    && work.waiting_retirement
                                    && work.recovery_authorized_epoch.as_deref()
                                        == Some(next.host_epoch.as_str())
                            })
                    })
        });
        let session = next.sessions.get_mut(session_id).ok_or("unknown_session")?;
        let valid = job.request.is_some_and(|id| id > 0)
            && result.as_ref().is_some_and(|(result_id, r)| {
                Some(*result_id) == job.request
                    && serde_json::to_vec(r).is_ok_and(|bytes| bytes.len() <= 4096)
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
            && session.host_epoch == target_epoch
            && (target_epoch == next.host_epoch || recovery_authorized)
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
        if valid && recovery_authorized {
            let context = job.recovery.as_ref().expect("validated recovery context");
            let original_operation =
                next.receipts[&context.original_key].request["operation_id"].clone();
            let owner = next.sessions[session_id].owner.clone();
            // Old verification requests cannot authorize native work. Once a
            // fresh exact owner query proves retirement, replace their pending
            // readback with an explicit supersession, preserving each identity.
            for (key, receipt) in &mut next.receipts {
                if key != &job.key
                    && receipt.caller == owner.caller
                    && receipt.initiator == owner.initiator
                    && receipt.work.is_none()
                    && receipt.wait_target.is_none()
                    && receipt.request["target"] == json!(job.target)
                    && receipt.response["status"] == "recovery_required"
                    && (receipt.request["operation"] == "application_retired"
                        || (receipt.request["operation"] == "recover_session"
                            && receipt.request["original_operation_id"] == original_operation))
                {
                    receipt.response["status"] = json!("rejected");
                    receipt.response["reason"] = json!("superseded_by_verified_retirement");
                }
            }
        }
        if !valid && recovery_authorized {
            let context = job.recovery.as_ref().expect("validated recovery context");
            next.sessions
                .get_mut(session_id)
                .expect("validated session")
                .phase = SessionPhase::RecoveryRequired;
            let original = next
                .receipts
                .get_mut(&context.original_key)
                .expect("validated original");
            original.response["status"] = json!("recovery_required");
            original.response["reason"] = json!("application_retirement_unconfirmed");
        }
        let receipt = next
            .receipts
            .get_mut(&job.key)
            .ok_or("unknown_application_receipt")?;
        receipt.response = if valid {
            json!({"status":"succeeded","operation_id":receipt.request["operation_id"],"target":job.target})
        } else {
            json!({"status":if job.recovery.is_some() {"rejected"} else {"recovery_required"},
                "operation_id":receipt.request["operation_id"],"reason":"application_retirement_unconfirmed"})
        };
        if job.recovery.is_some() {
            receipt.response["target"] = json!(job.target);
            if valid {
                receipt.response["reconciliation_started"] = json!(true);
            }
        }
        save(&self.shared, next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);

    fn pending_application() -> (std::path::PathBuf, Coordinator, LifecycleTarget, String) {
        let directory = std::env::temp_dir().join(format!(
            "amaterasu-application-persistence-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&directory).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let mut coordinator = Coordinator::open(&directory.join("journal.json")).unwrap();
        let mut next = coordinator.shared.borrow().value.clone();
        let target = LifecycleTarget::Session {
            host_epoch: next.host_epoch.clone(),
            session_id: "session".into(),
            generation: 1,
        };
        next.sessions.insert(
            "session".into(),
            serde_json::from_value(json!({
                "host_epoch": next.host_epoch, "id":"session", "generation":1,
                "phase":"ending", "owner":{"caller":"owner","initiator":"owner"},
                "delegates":[], "application_ref":{"kind":"application"},
                "settings_revision":"one", "application_retired":false, "streams":{},
                "application":{"owner":"owner","launch_id":42,"operation_id":"launch"}
            }))
            .unwrap(),
        );
        save(&coordinator.shared, next).unwrap();
        let key = receipt_key("owner", "owner", "retire");
        coordinator
            .application_operation(
                "owner",
                "owner",
                json!({"operation":"application_retired","operation_id":"retire"}),
                key.clone(),
                target.clone(),
            )
            .unwrap();
        (directory, coordinator, target, key)
    }

    fn additional_pending_application(
        coordinator: &mut Coordinator,
        index: u64,
    ) -> (LifecycleTarget, String) {
        let mut next = coordinator.shared.borrow().value.clone();
        let id = format!("session-{index}");
        let operation_id = format!("retire-{index:02}");
        let mut session = next.sessions["session"].clone();
        session.id = id.clone();
        session.phase = SessionPhase::Ending;
        session.application_retired = false;
        session.application_retirement_proof = None;
        session.application = Some(ApplicationBinding {
            owner: "owner".into(),
            launch_id: 42 + index,
            operation_id: format!("launch-{index}"),
        });
        let target = LifecycleTarget::Session {
            host_epoch: next.host_epoch.clone(),
            session_id: id.clone(),
            generation: session.generation,
        };
        next.sessions.insert(id, session);
        save(&coordinator.shared, next).unwrap();
        let key = receipt_key("owner", "owner", &operation_id);
        coordinator
            .application_operation(
                "owner",
                "owner",
                json!({"operation":"application_retired","operation_id":operation_id}),
                key.clone(),
                target.clone(),
            )
            .unwrap();
        (target, key)
    }

    fn retain_application_result(coordinator: &mut Coordinator, key: &str, request: u64) {
        let job = coordinator.application_jobs.get_mut(key).unwrap();
        job.request = Some(request);
        job.result = Some((
            request,
            json!({"ok":true,"retired":true,"owner":job.binding.owner,
                "launch_id":job.binding.launch_id,"operation_id":job.binding.operation_id}),
        ));
    }

    fn block_journal_replacement(directory: &Path) {
        std::fs::rename(
            directory.join("journal.json"),
            directory.join("journal.saved"),
        )
        .unwrap();
        std::fs::create_dir(directory.join("journal.json")).unwrap();
    }

    fn reopen_restored_journal(directory: &Path, coordinator: &mut Coordinator) {
        std::fs::remove_dir(directory.join("journal.json")).unwrap();
        std::fs::rename(
            directory.join("journal.saved"),
            directory.join("journal.json"),
        )
        .unwrap();
        // A failed filesystem replacement poisons the store. Reopen the same
        // path and verify its unchanged durable state before retrying the write.
        let parking_store = LifecycleStore::new(directory.join("parking.json")).unwrap();
        let poisoned = std::mem::replace(&mut coordinator.shared.borrow_mut().store, parking_store);
        drop(poisoned);
        let reopened = LifecycleStore::new(directory.join("journal.json")).unwrap();
        let durable: Journal = reopened.load().unwrap().unwrap();
        assert_eq!(
            serde_json::to_value(&durable).unwrap(),
            serde_json::to_value(&coordinator.shared.borrow().value).unwrap()
        );
        coordinator.shared.borrow_mut().store = reopened;
    }

    #[test]
    fn application_completion_is_durable_before_result_release() {
        for valid in [true, false] {
            let (directory, mut coordinator, target, key) = pending_application();
            let mut requests = 0;
            let mut releases = 0;
            let mut send = |command| match command {
                Command::Request { .. } => {
                    requests += 1;
                    Ok(Response::Accepted { id: 8 })
                }
                Command::TakeResult { request: 8 } => Ok(Response::Result {
                    payload: json!({"ok":true,"retired":true,
                        "owner":if valid {"owner"} else {"foreign"},
                        "launch_id":42,"operation_id":"launch"}),
                }),
                Command::ReleaseResult { request: 8 } => {
                    releases += 1;
                    Ok(if valid {
                        Response::Ok
                    } else {
                        Response::Error {
                            code: "not_found".into(),
                            message: "test result already released".into(),
                        }
                    })
                }
                _ => panic!("unexpected verification command"),
            };
            coordinator.advance_applications(&mut send).unwrap();
            coordinator.advance_applications(&mut send).unwrap();
            block_journal_replacement(&directory);
            assert_eq!(
                coordinator.advance_applications(&mut send).unwrap_err(),
                "journal_write_failed"
            );
            assert_eq!(requests, 1);
            assert_eq!(
                releases, 0,
                "the uncommitted proof must retain its broker result"
            );
            assert!(
                coordinator.application_jobs.contains_key(&key),
                "released verification result must retain its persistence owner"
            );
            assert!(!coordinator.shared.borrow().value.sessions["session"].application_retired);
            assert_eq!(
                coordinator.shared.borrow().value.receipts[&key].response["status"],
                "pending"
            );
            assert_eq!(
                coordinator
                    .advance_applications(&mut |_| panic!("settled result must not query again"))
                    .unwrap_err(),
                "journal_write_failed",
                "the poisoned store must remain fenced until reopening"
            );
            assert!(coordinator.application_jobs.contains_key(&key));
            reopen_restored_journal(&directory, &mut coordinator);
            let shared = coordinator.shared.clone();
            let mut retry_releases = 0;
            coordinator
                .advance_applications(&mut |command| match command {
                    Command::ReleaseResult { request: 8 } => {
                        retry_releases += 1;
                        let journal = shared.borrow();
                        assert_eq!(
                            valid_retirement_proof(&journal.value.sessions["session"], &target),
                            valid,
                            "the exact native proof must commit before result release"
                        );
                        assert_ne!(journal.value.receipts[&key].response["status"], "pending");
                        Ok(Response::Ok)
                    }
                    _ => panic!("terminal proof must only release its original result"),
                })
                .unwrap();
            assert_eq!(retry_releases, 1);
            assert!(!coordinator.application_jobs.contains_key(&key));
            let shared = coordinator.shared.borrow();
            assert_eq!(
                shared.value.receipts[&key].response["status"],
                if valid {
                    "succeeded"
                } else {
                    "recovery_required"
                }
            );
            assert_eq!(
                valid_retirement_proof(&shared.value.sessions["session"], &target),
                valid
            );
            drop(shared);
            drop(coordinator);
            std::fs::remove_dir_all(directory).unwrap();
        }
    }

    #[test]
    fn committed_application_proof_survives_release_failure_and_session_end() {
        let (directory, mut coordinator, target, key) = pending_application();
        let mut release_attempts = 0;
        let mut send = |command| match command {
            Command::Request { .. } => Ok(Response::Accepted { id: 8 }),
            Command::TakeResult { request: 8 } => Ok(Response::Result {
                payload: json!({"ok":true,"retired":true,"owner":"owner",
                    "launch_id":42,"operation_id":"launch"}),
            }),
            Command::ReleaseResult { request: 8 } => {
                release_attempts += 1;
                Ok(Response::Pending)
            }
            _ => panic!("unexpected verification command"),
        };
        for _ in 0..3 {
            coordinator.advance_applications(&mut send).unwrap();
        }
        assert_eq!(release_attempts, 1);
        assert!(coordinator.application_jobs.contains_key(&key));
        let mut ended = coordinator.shared.borrow().value.clone();
        assert!(valid_retirement_proof(&ended.sessions["session"], &target));
        assert_eq!(ended.receipts[&key].response["status"], "succeeded");
        ended.sessions.get_mut("session").unwrap().phase = SessionPhase::Ended;
        save(&coordinator.shared, ended).unwrap();
        let committed = serde_json::to_value(&coordinator.shared.borrow().value).unwrap();
        coordinator
            .advance_applications(&mut |command| match command {
                Command::ReleaseResult { request: 8 } => Ok(Response::Error {
                    code: "not_found".into(),
                    message: "test result was already released".into(),
                }),
                _ => panic!("committed proof must only release its original result"),
            })
            .unwrap();
        assert!(!coordinator.application_jobs.contains_key(&key));
        assert_eq!(
            serde_json::to_value(&coordinator.shared.borrow().value).unwrap(),
            committed,
            "a late result release must not rewrite a completed proof"
        );
        drop(coordinator);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn retained_application_results_do_not_starve_independent_completions() {
        for expired in [false, true] {
            let (directory, mut coordinator, _, first_key) = pending_application();
            let mut keys = vec![first_key];
            for index in 1..=8 {
                keys.push(additional_pending_application(&mut coordinator, index).1);
            }
            for (index, key) in keys.iter().enumerate() {
                if expired && index == 8 {
                    coordinator.application_jobs.get_mut(key).unwrap().deadline =
                        Instant::now() - Duration::from_secs(1);
                } else {
                    retain_application_result(&mut coordinator, key, 100 + index as u64);
                }
            }
            let mut release = |command| match command {
                Command::ReleaseResult { request } if (100..108).contains(&request) => {
                    Ok(Response::Pending)
                }
                Command::ReleaseResult { request: 108 } => Ok(Response::Ok),
                _ => panic!("retained proof must not submit another verification query"),
            };
            for _ in 0..3 {
                coordinator.advance_applications(&mut release).unwrap();
            }
            assert!(
                !coordinator.application_jobs.contains_key(&keys[8]),
                "eight held readbacks must not monopolize application completion"
            );
            assert_eq!(
                coordinator.shared.borrow().value.receipts[&keys[8]].response["status"],
                if expired {
                    "recovery_required"
                } else {
                    "succeeded"
                }
            );
            for (index, key) in keys.iter().enumerate().take(8) {
                let held = &coordinator.application_jobs[key];
                assert_eq!(held.request, Some(100 + index as u64));
                assert!(matches!(
                    held.completion,
                    Some(ApplicationCompletion::Committed)
                ));
            }
            let (_, tenth_key) = additional_pending_application(&mut coordinator, 9);
            retain_application_result(&mut coordinator, &tenth_key, 109);
            let mut release = |command| match command {
                Command::ReleaseResult { request: 100 | 109 } => Ok(Response::Ok),
                Command::ReleaseResult { request } if (101..108).contains(&request) => {
                    Ok(Response::Pending)
                }
                _ => panic!("removed keys and cursor wrap must preserve each request"),
            };
            for _ in 0..4 {
                coordinator.advance_applications(&mut release).unwrap();
            }
            assert!(!coordinator.application_jobs.contains_key(&keys[0]));
            assert!(!coordinator.application_jobs.contains_key(&tenth_key));
            assert_eq!(
                coordinator.shared.borrow().value.receipts[&tenth_key].response["status"],
                "succeeded"
            );
            assert_eq!(coordinator.application_jobs.len(), 7);
            for (index, key) in keys.iter().enumerate().take(8).skip(1) {
                let held = &coordinator.application_jobs[key];
                assert_eq!(held.request, Some(100 + index as u64));
                assert!(matches!(
                    held.completion,
                    Some(ApplicationCompletion::Committed)
                ));
            }
            drop(coordinator);
            std::fs::remove_dir_all(directory).unwrap();
        }
    }

    #[test]
    fn oversized_application_proof_is_rejected_without_blocking_an_independent_job() {
        let (directory, mut coordinator, target, key) = pending_application();
        let (independent_target, independent_key) =
            additional_pending_application(&mut coordinator, 1);
        retain_application_result(&mut coordinator, &key, 100);
        retain_application_result(&mut coordinator, &independent_key, 101);
        coordinator
            .application_jobs
            .get_mut(&key)
            .unwrap()
            .result
            .as_mut()
            .unwrap()
            .1["detail"] = json!("x".repeat(4096));
        let shared = coordinator.shared.clone();
        let mut released = BTreeSet::new();
        coordinator
            .advance_applications(&mut |command| match command {
                Command::ReleaseResult { request } if request == 100 || request == 101 => {
                    released.insert(request);
                    if request == 100 {
                        let journal = shared.borrow();
                        assert_eq!(
                            journal.value.receipts[&key].response["status"],
                            "recovery_required"
                        );
                        assert!(!valid_retirement_proof(
                            &journal.value.sessions["session"],
                            &target
                        ));
                    }
                    Ok(Response::Ok)
                }
                _ => panic!("retained proof must only release its original result"),
            })
            .unwrap();
        assert_eq!(released, BTreeSet::from([100, 101]));
        assert!(coordinator.application_jobs.is_empty());
        let journal = coordinator.shared.borrow();
        assert!(valid_retirement_proof(
            &journal.value.sessions["session-1"],
            &independent_target
        ));
        drop(journal);
        drop(coordinator);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn rotating_application_work_cannot_step_over_a_failed_journal_commit() {
        let (directory, mut coordinator, target, key) = pending_application();
        retain_application_result(&mut coordinator, &key, 100);
        for index in 1..=8 {
            let (_, independent_key) = additional_pending_application(&mut coordinator, index);
            coordinator
                .application_jobs
                .get_mut(&independent_key)
                .unwrap()
                .request = Some(100 + index);
        }
        block_journal_replacement(&directory);
        for _ in 0..2 {
            assert_eq!(
                coordinator
                    .advance_applications(&mut |_| panic!(
                        "failed persistence must fence all other broker work"
                    ))
                    .unwrap_err(),
                "journal_write_failed"
            );
            assert!(coordinator.application_jobs.contains_key(&key));
        }
        reopen_restored_journal(&directory, &mut coordinator);
        coordinator
            .advance_applications(&mut |command| match command {
                Command::ReleaseResult { request: 100 } => Ok(Response::Ok),
                Command::TakeResult { request } if (101..109).contains(&request) => {
                    Ok(Response::Pending)
                }
                _ => panic!("recovery must retain each original broker request"),
            })
            .unwrap();
        assert!(!coordinator.application_jobs.contains_key(&key));
        assert_eq!(coordinator.application_jobs.len(), 8);
        assert!(valid_retirement_proof(
            &coordinator.shared.borrow().value.sessions["session"],
            &target
        ));
        drop(coordinator);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn unconfirmed_application_completion_survives_failed_journal_write_without_redispatch() {
        for expired in [true, false] {
            let (directory, mut coordinator, target, key) = pending_application();
            if expired {
                coordinator.application_jobs.get_mut(&key).unwrap().deadline =
                    Instant::now() - Duration::from_secs(1);
            }
            block_journal_replacement(&directory);
            let mut requests = 0;
            assert_eq!(
                coordinator
                    .advance_applications(&mut |command| match command {
                        Command::Request { .. } if !expired => {
                            requests += 1;
                            Ok(Response::Error {
                                code: "unavailable".into(),
                                message: "test verification service is unavailable".into(),
                            })
                        }
                        _ => panic!("unexpected verification command"),
                    })
                    .unwrap_err(),
                "journal_write_failed"
            );
            assert_eq!(requests, usize::from(!expired));
            assert!(
                coordinator.application_jobs.contains_key(&key),
                "unconfirmed completion must retain its persistence owner"
            );
            assert_eq!(
                coordinator
                    .advance_applications(&mut |_| panic!(
                        "terminal rejection must not query again"
                    ))
                    .unwrap_err(),
                "journal_write_failed"
            );
            reopen_restored_journal(&directory, &mut coordinator);
            coordinator
                .advance_applications(&mut |_| panic!("terminal rejection must not query again"))
                .unwrap();
            assert!(!coordinator.application_jobs.contains_key(&key));
            let shared = coordinator.shared.borrow();
            assert_eq!(
                shared.value.receipts[&key].response["status"],
                "recovery_required"
            );
            assert!(!valid_retirement_proof(
                &shared.value.sessions["session"],
                &target
            ));
            drop(shared);
            drop(coordinator);
            std::fs::remove_dir_all(directory).unwrap();
        }
    }

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
                processing_decision: None,
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
                capture_binding_mode: Default::default(),
                prepared_plan_ids: Default::default(),
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
