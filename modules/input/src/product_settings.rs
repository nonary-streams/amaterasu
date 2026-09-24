//! Durable input policy and asynchronous configuration RPC lifecycle.
use crate::controller::Controller;
use crate::model::DevicePolicy;
use crate::{CONFIGURATION_SERVICE, backend::Backend};
use amaterasu_module_sdk::{Command, ModuleResult, Response};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const PRODUCT_SETTINGS_VERSION: u32 = 1;
const CONFIGURATION_TIMEOUT_MS: u64 = 30_000;
type Sender<'a> = &'a mut dyn FnMut(Command) -> ModuleResult<Response>;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct InputPolicy {
    #[serde(default = "default_enabled")]
    pub keyboard: bool,
    #[serde(default = "default_enabled")]
    pub mouse: bool,
}

const fn default_enabled() -> bool {
    true
}

impl InputPolicy {
    fn device_policy(self) -> DevicePolicy {
        DevicePolicy {
            keyboard: self.keyboard,
            mouse: self.mouse,
        }
    }

    fn values(self) -> Value {
        json!({"keyboard":self.keyboard,"mouse":self.mouse})
    }

    fn update_payload(self, expected_revision: u64) -> Value {
        json!({
            "action":"product_update",
            "expected_revision":expected_revision,
            "version":PRODUCT_SETTINGS_VERSION,
            "values":self.values(),
        })
    }
}

impl Default for InputPolicy {
    fn default() -> Self {
        Self {
            keyboard: true,
            mouse: true,
        }
    }
}

enum Work {
    StartupLoad,
    Update {
        management_request: u64,
        expected_revision: u64,
        desired: InputPolicy,
    },
    ReconcileUpdate {
        management_request: u64,
        expected_revision: u64,
        desired: InputPolicy,
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

pub struct ProductSettings {
    policy: InputPolicy,
    revision: u64,
    ready: bool,
    pending: Option<Pending>,
}

impl ProductSettings {
    pub fn new() -> Self {
        Self {
            policy: InputPolicy::default(),
            revision: 0,
            ready: false,
            pending: Some(Pending {
                work: Work::StartupLoad,
                call: Call::Submit {
                    payload: json!({"action":"product_get"}),
                    operation_id: "input.product-settings.startup".into(),
                    parent: None,
                    retry_ticks: 0,
                },
            }),
        }
    }

    pub fn ready(&self) -> bool {
        self.ready
    }

    pub fn busy(&self) -> bool {
        self.pending.is_some()
    }

    pub fn get_response<B: Backend>(&self, controller: &Controller<B>) -> Value {
        if !self.ready {
            return error("settings_unavailable");
        }
        self.response(controller, true)
    }

    pub fn begin_update(
        &mut self,
        request: u64,
        expected_revision: u64,
        desired: InputPolicy,
        remaining_ms: u64,
    ) -> Result<(), Value> {
        if !self.ready {
            return Err(error("settings_unavailable"));
        }
        if self.pending.is_some() {
            return Err(error("settings_busy"));
        }
        if expected_revision != self.revision {
            return Err(json!({
                "ok":false,
                "error":{"code":"revision_conflict","current_revision":self.revision}
            }));
        }
        let timeout_ms = remaining_ms.min(CONFIGURATION_TIMEOUT_MS);
        if timeout_ms == 0 {
            return Err(error("deadline_exceeded"));
        }
        self.pending = Some(Pending {
            work: Work::Update {
                management_request: request,
                expected_revision,
                desired,
            },
            call: Call::Submit {
                payload: desired.update_payload(expected_revision),
                operation_id: format!("input.product-settings.update.{request}"),
                parent: Some(request),
                retry_ticks: 0,
            },
        });
        Ok(())
    }

    pub fn advance<B: Backend>(
        &mut self,
        send: Sender<'_>,
        controller: &mut Controller<B>,
    ) -> ModuleResult<()> {
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
                let timeout_ms = CONFIGURATION_TIMEOUT_MS;
                match send(Command::Request {
                    service: CONFIGURATION_SERVICE.into(),
                    payload: payload.clone(),
                    timeout_ms,
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
                        self.on_submit_error(
                            send,
                            pending.work,
                            payload,
                            operation_id,
                            parent,
                            code,
                        )?;
                    }
                    Ok(_response) => {
                        self.on_terminal(send, controller, pending.work, ChildOutcome::Failure)?;
                    }
                    Err(_) => {
                        // Repeating the same operation id retrieves the existing request if the
                        // transport accepted it before the local acknowledgement was lost.
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
                Ok(Response::Pending) => {
                    self.pending = Some(Pending {
                        work: pending.work,
                        call: Call::Waiting { request },
                    });
                }
                Err(_) => {
                    self.pending = Some(Pending {
                        work: pending.work,
                        call: Call::Waiting { request },
                    });
                }
                Ok(Response::Error { code, .. }) if code == "not_found" => {
                    self.on_child_missing(send, pending.work)?;
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
                Ok(Response::Error { .. }) => {
                    self.pending = Some(Pending {
                        work: pending.work,
                        call: Call::Releasing {
                            request,
                            outcome: ChildOutcome::Failure,
                        },
                    });
                }
                Ok(_response) => {
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
                    Ok(Response::Ok) => {
                        self.on_terminal(send, controller, pending.work, outcome)?;
                    }
                    Ok(Response::Error { code, .. }) if code == "not_found" => {
                        self.on_terminal(send, controller, pending.work, outcome)?;
                    }
                    _ => {
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

    fn on_submit_error(
        &mut self,
        send: Sender<'_>,
        work: Work,
        payload: Value,
        operation_id: String,
        parent: Option<u64>,
        code: String,
    ) -> ModuleResult<()> {
        match work {
            Work::StartupLoad => {
                self.pending = Some(Pending {
                    work: Work::StartupLoad,
                    call: Call::Submit {
                        payload,
                        operation_id,
                        parent,
                        retry_ticks: 30,
                    },
                });
            }
            Work::Update {
                management_request, ..
            } => complete(send, management_request, error(&safe_code(&code)))?,
            Work::ReconcileUpdate {
                management_request,
                expected_revision,
                desired,
            } => self.retry_reconciliation(management_request, expected_revision, desired, 10),
        }
        Ok(())
    }

    fn on_child_missing(&mut self, _send: Sender<'_>, work: Work) -> ModuleResult<()> {
        match work {
            Work::StartupLoad => self.retry_startup_load(10),
            Work::Update {
                management_request,
                expected_revision,
                desired,
            }
            | Work::ReconcileUpdate {
                management_request,
                expected_revision,
                desired,
            } => self.start_reconciliation(management_request, expected_revision, desired),
        }
        Ok(())
    }

    fn on_terminal<B: Backend>(
        &mut self,
        send: Sender<'_>,
        controller: &mut Controller<B>,
        work: Work,
        outcome: ChildOutcome,
    ) -> ModuleResult<()> {
        match work {
            Work::StartupLoad => match outcome {
                ChildOutcome::Payload(payload) => match read_document(&payload) {
                    Ok((policy, revision)) => {
                        self.policy = policy;
                        self.revision = revision;
                        self.ready = true;
                        let _ = controller
                            .set_device_policy(policy.device_policy(), std::time::Instant::now());
                    }
                    Err(_) => self.retry_startup_load(30),
                },
                ChildOutcome::Failure => self.retry_startup_load(30),
            },
            Work::Update {
                management_request,
                expected_revision,
                desired,
            } => match outcome {
                ChildOutcome::Payload(payload) if payload["ok"] == true => {
                    match read_document(&payload) {
                        Ok((actual, revision))
                            if expected_revision.checked_add(1) == Some(revision)
                                && actual == desired =>
                        {
                            self.apply_update(
                                send,
                                controller,
                                management_request,
                                actual,
                                revision,
                            )?;
                        }
                        _ => self.start_reconciliation(
                            management_request,
                            expected_revision,
                            desired,
                        ),
                    }
                }
                ChildOutcome::Payload(payload) => {
                    if let Some(failure) = product_failure(&payload) {
                        complete(send, management_request, failure)?;
                    } else {
                        self.start_reconciliation(management_request, expected_revision, desired);
                    }
                }
                ChildOutcome::Failure => {
                    self.start_reconciliation(management_request, expected_revision, desired)
                }
            },
            Work::ReconcileUpdate {
                management_request,
                expected_revision,
                desired,
            } => match outcome {
                ChildOutcome::Payload(payload) => match read_document(&payload) {
                    Ok((actual, revision))
                        if expected_revision.checked_add(1) == Some(revision)
                            && actual == desired =>
                    {
                        self.apply_update(send, controller, management_request, actual, revision)?;
                    }
                    Ok((actual, revision)) => {
                        // Reconciliation establishes the currently durable policy even when the
                        // write outcome is uncertain, but never reports the attempted update as
                        // successful unless its exact next revision is present.
                        self.policy = actual;
                        self.revision = revision;
                        self.ready = true;
                        let cleanup = controller
                            .set_device_policy(actual.device_policy(), std::time::Instant::now())
                            .is_err();
                        complete(
                            send,
                            management_request,
                            json!({"ok":false,"error":{"code":"write_unresolved"},"persisted":false,"revision":revision,"values":actual.values(),"applied":!cleanup,"cleanup_required":cleanup}),
                        )?;
                    }
                    Err(_) => self.retry_reconciliation(
                        management_request,
                        expected_revision,
                        desired,
                        10,
                    ),
                },
                ChildOutcome::Failure => {
                    self.retry_reconciliation(management_request, expected_revision, desired, 10)
                }
            },
        }
        Ok(())
    }

    fn apply_update<B: Backend>(
        &mut self,
        send: Sender<'_>,
        controller: &mut Controller<B>,
        request: u64,
        policy: InputPolicy,
        revision: u64,
    ) -> ModuleResult<()> {
        self.policy = policy;
        self.revision = revision;
        self.ready = true;
        let cleanup = controller
            .set_device_policy(policy.device_policy(), std::time::Instant::now())
            .is_err();
        let response = if cleanup {
            json!({
                "ok":false,
                "error":{"code":"cleanup_pending"},
                "persisted":true,
                "applied":false,
                "cleanup_required":true,
                "revision":revision,
                "values":policy.values(),
            })
        } else {
            settings_response(policy, revision, true)
        };
        complete(send, request, response)
    }

    fn response<B: Backend>(&self, controller: &Controller<B>, persisted: bool) -> Value {
        let cleanup_required = controller.cleanup_required();
        let applied =
            controller.device_policy() == self.policy.device_policy() && !cleanup_required;
        settings_response_with_state(
            self.policy,
            self.revision,
            persisted,
            applied,
            cleanup_required,
        )
    }

    fn retry_startup_load(&mut self, retry_ticks: u8) {
        self.pending = Some(Pending {
            work: Work::StartupLoad,
            call: Call::Submit {
                payload: json!({"action":"product_get"}),
                operation_id: "input.product-settings.startup".into(),
                parent: None,
                retry_ticks,
            },
        });
    }

    fn start_reconciliation(&mut self, request: u64, expected_revision: u64, desired: InputPolicy) {
        self.pending = Some(Pending {
            work: Work::ReconcileUpdate {
                management_request: request,
                expected_revision,
                desired,
            },
            call: Call::Submit {
                payload: json!({"action":"product_get"}),
                operation_id: format!("input.product-settings.reconcile.{request}"),
                parent: Some(request),
                retry_ticks: 0,
            },
        });
    }

    fn retry_reconciliation(
        &mut self,
        request: u64,
        expected_revision: u64,
        desired: InputPolicy,
        retry_ticks: u8,
    ) {
        self.pending = Some(Pending {
            work: Work::ReconcileUpdate {
                management_request: request,
                expected_revision,
                desired,
            },
            call: Call::Submit {
                payload: json!({"action":"product_get"}),
                operation_id: format!("input.product-settings.reconcile.{request}"),
                parent: Some(request),
                retry_ticks,
            },
        });
    }
}

fn read_document(payload: &Value) -> Result<(InputPolicy, u64), &'static str> {
    if payload["ok"] != true {
        return Err("configuration get was not successful");
    }
    let revision = payload["revision"].as_u64().ok_or("missing revision")?;
    match payload["found"].as_bool() {
        Some(false)
            if revision == 0 && payload["version"].is_null() && payload["values"] == json!({}) =>
        {
            Ok((InputPolicy::default(), 0))
        }
        Some(true) if payload["version"].as_u64() == Some(u64::from(PRODUCT_SETTINGS_VERSION)) => {
            let values = payload["values"].as_object().ok_or("values not object")?;
            if values
                .keys()
                .any(|key| !matches!(key.as_str(), "keyboard" | "mouse"))
            {
                return Err("unknown input policy value");
            }
            let policy: InputPolicy = serde_json::from_value(payload["values"].clone())
                .map_err(|_| "malformed input policy")?;
            if revision == 0 {
                return Err("found document has no revision");
            }
            Ok((policy, revision))
        }
        _ => Err("malformed configuration document"),
    }
}

fn product_failure(payload: &Value) -> Option<Value> {
    if payload["ok"] != false {
        return None;
    }
    let failure = payload.get("error")?.as_object()?;
    let code = failure.get("code")?.as_str()?;
    if failure
        .keys()
        .any(|key| !matches!(key.as_str(), "code" | "message" | "current_revision"))
        || code.is_empty()
        || code.len() > 64
        || !code
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    {
        return Some(error("configuration_unavailable"));
    }
    let current_revision = failure.get("current_revision").and_then(Value::as_u64);
    Some(
        match current_revision.filter(|_| code == "revision_conflict") {
            Some(current_revision) => {
                json!({"ok":false,"error":{"code":code,"current_revision":current_revision}})
            }
            None => json!({"ok":false,"error":{"code":code}}),
        },
    )
}

fn settings_response(policy: InputPolicy, revision: u64, applied: bool) -> Value {
    settings_response_with_state(policy, revision, true, applied, !applied)
}

fn settings_response_with_state(
    policy: InputPolicy,
    revision: u64,
    persisted: bool,
    applied: bool,
    cleanup_required: bool,
) -> Value {
    json!({
        "ok":true,
        "persisted":persisted,
        "applied":applied,
        "cleanup_required":cleanup_required,
        "revision":revision,
        "values":policy.values(),
    })
}

fn complete(send: Sender<'_>, request: u64, payload: Value) -> ModuleResult<()> {
    match send(Command::Complete { request, payload })? {
        Response::Ok => Ok(()),
        Response::Error { code, .. } if code == "request_ended" || code == "not_found" => Ok(()),
        _ => Err("input settings completion failed".into()),
    }
}

fn safe_code(code: &str) -> String {
    if !code.is_empty()
        && code.len() <= 64
        && code
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    {
        code.to_owned()
    } else {
        "configuration_unavailable".into()
    }
}

fn error(code: &str) -> Value {
    json!({"ok":false,"error":{"code":safe_code(code)}})
}

#[cfg(test)]
mod tests;
