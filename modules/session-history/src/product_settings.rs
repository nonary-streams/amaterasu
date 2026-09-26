//! Asynchronous CAS client for the session-history product policy.
use crate::model::HistorySettings;
use amaterasu_module_sdk::{Command, ModuleResult, Response};
use serde_json::{Value, json};
use std::collections::VecDeque;

const CONFIGURATION_SERVICE: &str = "amaterasu.configuration/1";
const PRODUCT_VERSION: u32 = 1;
const CONFIGURATION_TIMEOUT_MS: u64 = 30_000;
type Sender<'a> = &'a mut dyn FnMut(Command) -> ModuleResult<Response>;

enum Work {
    StartupProbe {
        candidate: Option<HistorySettings>,
    },
    Initialize {
        candidate: HistorySettings,
    },
    Update {
        management_request: u64,
        expected_revision: u64,
        desired: HistorySettings,
    },
    ReconcileStartup {
        candidate: Option<HistorySettings>,
    },
    ReconcileUpdate {
        management_request: u64,
        expected_revision: u64,
        desired: HistorySettings,
    },
}

enum ChildOutcome {
    Payload(Value),
    Failure,
}

enum Call {
    Submit {
        payload: Value,
        operation_id: String,
        parent: Option<u64>,
        retry_ticks: u8,
    },
    Waiting {
        request: u64,
    },
    Releasing {
        request: u64,
        outcome: ChildOutcome,
    },
}

struct Pending {
    work: Work,
    call: Call,
}

pub enum UpdateCompletion {
    Applied {
        request: u64,
        settings: HistorySettings,
        revision: u64,
    },
    Failed {
        request: u64,
        response: Value,
    },
}

pub struct ProductSettings {
    settings: HistorySettings,
    revision: u64,
    ready: bool,
    next_operation: u64,
    pending: Option<Pending>,
    completions: VecDeque<UpdateCompletion>,
}

impl ProductSettings {
    pub fn new(migration_candidate: Option<HistorySettings>) -> Self {
        Self {
            settings: HistorySettings::default(),
            revision: 0,
            ready: false,
            next_operation: 1,
            pending: Some(Pending {
                work: Work::StartupProbe {
                    candidate: migration_candidate,
                },
                call: Call::Submit {
                    payload: product_get(),
                    operation_id: "session-history.configuration.startup.0".into(),
                    parent: None,
                    retry_ticks: 0,
                },
            }),
            completions: VecDeque::new(),
        }
    }

    pub fn is_ready(&self) -> bool {
        self.ready
    }

    pub fn busy(&self) -> bool {
        self.pending.is_some()
    }

    pub fn settings(&self) -> (&HistorySettings, u64) {
        (&self.settings, self.revision)
    }

    pub fn begin_update(
        &mut self,
        request: u64,
        expected_revision: u64,
        desired: HistorySettings,
    ) -> Result<(), Value> {
        if !self.ready {
            return Err(failure(
                "configuration_unavailable",
                "session history settings are not loaded",
            ));
        }
        if self.pending.is_some() {
            return Err(failure(
                "settings_busy",
                "another session history configuration operation is pending",
            ));
        }
        desired
            .validate()
            .map_err(|message| failure("invalid_request", message))?;
        if expected_revision != self.revision {
            return Err(revision_conflict(self.revision));
        }
        self.pending = Some(Pending {
            work: Work::Update {
                management_request: request,
                expected_revision,
                desired: desired.clone(),
            },
            call: Call::Submit {
                payload: product_update(&desired, expected_revision),
                operation_id: self.next_operation_id("update"),
                parent: Some(request),
                retry_ticks: 0,
            },
        });
        Ok(())
    }

    pub fn take_completion(&mut self) -> Option<UpdateCompletion> {
        self.completions.pop_front()
    }

    pub fn advance(&mut self, send: Sender<'_>) -> ModuleResult<()> {
        let Some(pending) = self.pending.take() else {
            return Ok(());
        };
        match pending.call {
            Call::Submit {
                payload,
                operation_id,
                parent,
                retry_ticks,
            } => {
                if retry_ticks > 0 {
                    self.pending = Some(Pending {
                        work: pending.work,
                        call: Call::Submit {
                            payload,
                            operation_id,
                            parent,
                            retry_ticks: retry_ticks - 1,
                        },
                    });
                    return Ok(());
                }
                match send(Command::Request {
                    service: CONFIGURATION_SERVICE.into(),
                    payload: payload.clone(),
                    timeout_ms: CONFIGURATION_TIMEOUT_MS,
                    operation_id: operation_id.clone(),
                    parent,
                }) {
                    Ok(Response::Accepted { id }) => {
                        self.pending = Some(Pending {
                            work: pending.work,
                            call: Call::Waiting { request: id },
                        });
                    }
                    Ok(Response::Error { code, .. }) => {
                        self.on_submit_error(pending.work, &code);
                    }
                    Ok(_) => self.retry_work(pending.work, 20),
                    Err(_) => {
                        // A stable operation id recovers an accepted request if its
                        // acknowledgement was lost in transport.
                        self.pending = Some(Pending {
                            work: pending.work,
                            call: Call::Submit {
                                payload,
                                operation_id,
                                parent,
                                retry_ticks: 1,
                            },
                        });
                    }
                }
            }
            Call::Waiting { request } => match send(Command::TakeResult { request }) {
                Ok(Response::Pending) | Err(_) => {
                    self.pending = Some(Pending {
                        work: pending.work,
                        call: Call::Waiting { request },
                    });
                }
                Ok(Response::Error { code, .. }) if code == "not_found" => {
                    self.on_child_missing(pending.work);
                }
                Ok(Response::Result { payload }) => {
                    self.pending = Some(Pending {
                        work: pending.work,
                        call: Call::Releasing {
                            request,
                            outcome: ChildOutcome::Payload(payload),
                        },
                    });
                }
                Ok(_) => {
                    self.pending = Some(Pending {
                        work: pending.work,
                        call: Call::Releasing {
                            request,
                            outcome: ChildOutcome::Failure,
                        },
                    });
                }
            },
            Call::Releasing { request, outcome } => {
                match send(Command::ReleaseResult { request }) {
                    Ok(Response::Ok) => self.on_terminal(pending.work, outcome),
                    Ok(Response::Error { code, .. }) if code == "not_found" => {
                        self.on_terminal(pending.work, outcome)
                    }
                    Err(_) | Ok(_) => {
                        self.pending = Some(Pending {
                            work: pending.work,
                            call: Call::Releasing { request, outcome },
                        });
                    }
                }
            }
        }
        Ok(())
    }

    fn on_submit_error(&mut self, work: Work, code: &str) {
        match work {
            Work::StartupProbe { candidate } => self.retry_startup_probe(candidate, 20),
            Work::Initialize { candidate } if code == "revision_conflict" => {
                self.start_startup_reconciliation(Some(candidate))
            }
            Work::Initialize { candidate } => self.retry_migration(candidate, 20),
            Work::Update {
                management_request,
                expected_revision,
                desired,
            } if code == "revision_conflict" => {
                self.start_update_reconciliation(management_request, expected_revision, desired)
            }
            Work::Update {
                management_request, ..
            } => self.complete_failure(
                management_request,
                failure(code, "configuration service rejected the settings update"),
            ),
            Work::ReconcileStartup { candidate } => self.retry_startup_get_with(candidate, 20),
            Work::ReconcileUpdate {
                management_request,
                expected_revision,
                desired,
            } => {
                self.retry_update_reconciliation(management_request, expected_revision, desired, 20)
            }
        }
    }

    fn on_child_missing(&mut self, work: Work) {
        match work {
            Work::StartupProbe { candidate } => self.start_startup_reconciliation(candidate),
            Work::Initialize { candidate } => self.start_startup_reconciliation(Some(candidate)),
            Work::ReconcileStartup { candidate } => self.start_startup_reconciliation(candidate),
            Work::Update {
                management_request,
                expected_revision,
                desired,
            }
            | Work::ReconcileUpdate {
                management_request,
                expected_revision,
                desired,
            } => self.start_update_reconciliation(management_request, expected_revision, desired),
        }
    }

    fn on_terminal(&mut self, work: Work, outcome: ChildOutcome) {
        let ChildOutcome::Payload(payload) = outcome else {
            self.retry_work(work, 20);
            return;
        };
        match work {
            // The stable startup operation ID may reconnect to a result from the
            // previous process. Only the GET submitted after its release is current.
            Work::StartupProbe { candidate } => self.start_startup_reconciliation(candidate),
            Work::ReconcileStartup { candidate } => match read_document(&payload) {
                Ok(Document::Found(settings, revision)) => self.adopt(settings, revision),
                Ok(Document::Absent) => match candidate {
                    Some(candidate) => self.begin_migration(candidate),
                    None => self.retry_startup_get_with(None, 30),
                },
                Err(_) => self.retry_startup_get_with(candidate, 30),
            },
            Work::Initialize { candidate } => match read_document(&payload) {
                Ok(Document::Found(settings, revision))
                    if revision == 1 && settings == candidate =>
                {
                    self.adopt(settings, revision)
                }
                Ok(_) | Err(_) => self.start_startup_reconciliation(Some(candidate)),
            },
            Work::Update {
                management_request,
                expected_revision,
                desired,
            } => {
                if let Some(error) = product_error(&payload) {
                    if error["code"] == "revision_conflict" {
                        self.start_update_reconciliation(
                            management_request,
                            expected_revision,
                            desired,
                        );
                    } else {
                        self.complete_failure(management_request, error);
                    }
                    return;
                }
                match read_document(&payload) {
                    Ok(Document::Found(settings, revision))
                        if expected_revision.checked_add(1) == Some(revision)
                            && settings == desired =>
                    {
                        self.adopt(settings.clone(), revision);
                        self.completions.push_back(UpdateCompletion::Applied {
                            request: management_request,
                            settings,
                            revision,
                        });
                    }
                    _ => self.start_update_reconciliation(
                        management_request,
                        expected_revision,
                        desired,
                    ),
                }
            }
            Work::ReconcileUpdate {
                management_request,
                expected_revision,
                desired,
            } => match read_document(&payload) {
                Ok(Document::Found(settings, revision)) => {
                    if expected_revision.checked_add(1) == Some(revision) && settings == desired {
                        self.adopt(settings.clone(), revision);
                        self.completions.push_back(UpdateCompletion::Applied {
                            request: management_request,
                            settings,
                            revision,
                        });
                    } else {
                        self.adopt(settings, revision);
                        let response = if revision != expected_revision {
                            revision_conflict(revision)
                        } else {
                            failure(
                                "write_unresolved",
                                "the durable settings do not match the requested update",
                            )
                        };
                        self.complete_failure(management_request, response);
                    }
                }
                Ok(Document::Absent) => {
                    self.adopt(HistorySettings::default(), 0);
                    self.complete_failure(management_request, revision_conflict(0));
                }
                Err(_) => self.retry_update_reconciliation(
                    management_request,
                    expected_revision,
                    desired,
                    30,
                ),
            },
        }
    }

    fn adopt(&mut self, settings: HistorySettings, revision: u64) {
        self.settings = settings;
        self.revision = revision;
        self.ready = true;
    }

    fn begin_migration(&mut self, candidate: HistorySettings) {
        self.pending = Some(Pending {
            work: Work::Initialize {
                candidate: candidate.clone(),
            },
            call: Call::Submit {
                payload: product_update(&candidate, 0),
                operation_id: self.next_operation_id("migrate"),
                parent: None,
                retry_ticks: 0,
            },
        });
    }

    fn start_startup_reconciliation(&mut self, candidate: Option<HistorySettings>) {
        self.pending = Some(Pending {
            work: Work::ReconcileStartup { candidate },
            call: Call::Submit {
                payload: product_get(),
                operation_id: "session-history.configuration.startup.0".into(),
                parent: None,
                retry_ticks: 0,
            },
        });
    }

    fn start_update_reconciliation(
        &mut self,
        request: u64,
        expected_revision: u64,
        desired: HistorySettings,
    ) {
        self.pending = Some(Pending {
            work: Work::ReconcileUpdate {
                management_request: request,
                expected_revision,
                desired,
            },
            call: Call::Submit {
                payload: product_get(),
                operation_id: self.next_operation_id("update-reconcile"),
                parent: None,
                retry_ticks: 0,
            },
        });
    }

    fn retry_update_reconciliation(
        &mut self,
        request: u64,
        expected_revision: u64,
        desired: HistorySettings,
        retry_ticks: u8,
    ) {
        self.pending = Some(Pending {
            work: Work::ReconcileUpdate {
                management_request: request,
                expected_revision,
                desired,
            },
            call: Call::Submit {
                payload: product_get(),
                operation_id: self.next_operation_id("update-reconcile"),
                parent: None,
                retry_ticks,
            },
        });
    }

    fn retry_startup_get_with(&mut self, candidate: Option<HistorySettings>, retry_ticks: u8) {
        self.pending = Some(Pending {
            work: Work::ReconcileStartup { candidate },
            call: Call::Submit {
                payload: product_get(),
                operation_id: "session-history.configuration.startup.0".into(),
                parent: None,
                retry_ticks,
            },
        });
    }

    fn retry_migration(&mut self, candidate: HistorySettings, retry_ticks: u8) {
        self.pending = Some(Pending {
            work: Work::Initialize {
                candidate: candidate.clone(),
            },
            call: Call::Submit {
                payload: product_update(&candidate, 0),
                operation_id: self.next_operation_id("migrate"),
                parent: None,
                retry_ticks,
            },
        });
    }

    fn retry_work(&mut self, work: Work, retry_ticks: u8) {
        match work {
            Work::StartupProbe { candidate } => self.retry_startup_probe(candidate, retry_ticks),
            Work::Initialize { candidate } => self.retry_migration(candidate, retry_ticks),
            Work::Update {
                management_request,
                expected_revision,
                desired,
            } => {
                self.pending = Some(Pending {
                    work: Work::Update {
                        management_request,
                        expected_revision,
                        desired: desired.clone(),
                    },
                    call: Call::Submit {
                        payload: product_update(&desired, expected_revision),
                        operation_id: self.next_operation_id("update"),
                        parent: Some(management_request),
                        retry_ticks,
                    },
                })
            }
            Work::ReconcileStartup { candidate } => {
                self.retry_startup_get_with(candidate, retry_ticks)
            }
            Work::ReconcileUpdate {
                management_request,
                expected_revision,
                desired,
            } => self.retry_update_reconciliation(
                management_request,
                expected_revision,
                desired,
                retry_ticks,
            ),
        }
    }

    fn retry_startup_probe(&mut self, candidate: Option<HistorySettings>, retry_ticks: u8) {
        self.pending = Some(Pending {
            work: Work::StartupProbe { candidate },
            call: Call::Submit {
                payload: product_get(),
                operation_id: "session-history.configuration.startup.0".into(),
                parent: None,
                retry_ticks,
            },
        });
    }

    fn complete_failure(&mut self, request: u64, response: Value) {
        self.completions
            .push_back(UpdateCompletion::Failed { request, response });
    }

    fn next_operation_id(&mut self, purpose: &str) -> String {
        let id = self.next_operation;
        self.next_operation = self.next_operation.saturating_add(1);
        format!("session-history.configuration.{purpose}.{id}")
    }
}

enum Document {
    Absent,
    Found(HistorySettings, u64),
}

fn product_get() -> Value {
    json!({"action":"product_get"})
}

fn product_update(settings: &HistorySettings, expected_revision: u64) -> Value {
    json!({
        "action":"product_update",
        "expected_revision":expected_revision,
        "version":PRODUCT_VERSION,
        "values":{
            "enabled":settings.enabled,
            "ttl_days":settings.ttl_days,
            "db_size_limit_mb":settings.db_size_limit_mb,
        },
    })
}

fn read_document(payload: &Value) -> Result<Document, &'static str> {
    if payload.get("ok").and_then(Value::as_bool) != Some(true) {
        return Err("configuration request failed");
    }
    let revision = payload
        .get("revision")
        .and_then(Value::as_u64)
        .ok_or("configuration response has no revision")?;
    match payload.get("found").and_then(Value::as_bool) {
        Some(false)
            if revision == 0
                && payload.get("version").is_some_and(Value::is_null)
                && payload.get("values") == Some(&json!({})) =>
        {
            Ok(Document::Absent)
        }
        Some(true)
            if payload.get("version").and_then(Value::as_u64) == Some(PRODUCT_VERSION as u64)
                && revision > 0 =>
        {
            let values = payload
                .get("values")
                .and_then(Value::as_object)
                .ok_or("configuration values are not an object")?;
            if values
                .keys()
                .any(|key| !matches!(key.as_str(), "enabled" | "ttl_days" | "db_size_limit_mb"))
            {
                return Err("configuration document has unknown session-history fields");
            }
            let settings: HistorySettings = serde_json::from_value(Value::Object(values.clone()))
                .map_err(|_| "configuration settings are malformed")?;
            settings.validate()?;
            Ok(Document::Found(settings, revision))
        }
        _ => Err("configuration product document is malformed or unsupported"),
    }
}

fn product_error(payload: &Value) -> Option<Value> {
    if payload.get("ok").and_then(Value::as_bool) != Some(false) {
        return None;
    }
    let error = payload.get("error")?;
    let code = error.get("code")?.as_str()?;
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("configuration service rejected the request");
    Some(failure(code, message))
}

fn failure(code: &str, message: &str) -> Value {
    json!({"ok":false,"code":code,"message":message})
}

fn revision_conflict(revision: u64) -> Value {
    json!({
        "ok":false,
        "code":"revision_conflict",
        "message":"history settings changed since the supplied revision",
        "revision":revision,
    })
}

#[cfg(test)]
mod tests;
