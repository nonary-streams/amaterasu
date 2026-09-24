//! Product-owned Lutris settings and management-only durable updates.

use crate::settings::ProductSettings;
use amaterasu_module_sdk::{Command, ModuleResult, Response, RuntimeClient};
use serde_json::{Value, json};

const SERVICE: &str = "amaterasu.configuration/1";
const TIMEOUT_MS: u64 = 30_000;
const MAX_RETRY_TICKS: u32 = 64;

#[derive(Clone)]
pub struct ProductConfiguration {
    settings: ProductSettings,
    revision: u64,
    ready: bool,
    pending: Option<Pending>,
    operation_sequence: u64,
}

#[derive(Clone)]
enum Operation {
    Load,
    Update {
        management_request: u64,
        expected_revision: u64,
        settings: ProductSettings,
    },
    Reconcile {
        management_request: u64,
        resolution: UpdateResolution,
    },
}

#[derive(Clone)]
enum UpdateResolution {
    Unknown(String),
    Conflict,
}

#[derive(Clone)]
enum Call {
    Submit {
        payload: Value,
        operation_id: String,
    },
    Waiting {
        request: u64,
    },
    Delay {
        ticks: u32,
    },
}

#[derive(Clone)]
struct Pending {
    operation: Operation,
    call: Call,
    retry_attempt: u32,
}

impl ProductConfiguration {
    pub fn new() -> Self {
        let mut configuration = Self {
            settings: ProductSettings::default(),
            revision: 0,
            ready: false,
            pending: None,
            operation_sequence: 0,
        };
        let operation_id = configuration.next_operation_id("load");
        configuration.pending = Some(Pending {
            operation: Operation::Load,
            call: Call::Submit {
                payload: json!({"action":"product_get"}),
                operation_id,
            },
            retry_attempt: 0,
        });
        configuration
    }

    pub fn settings(&self) -> &ProductSettings {
        &self.settings
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn ready(&self) -> bool {
        self.ready
    }

    pub fn busy(&self) -> bool {
        self.pending.is_some()
    }

    pub fn management_response(&self) -> Value {
        self.settings.management_response(self.revision)
    }

    pub fn begin_update(
        &mut self,
        management_request: u64,
        settings: ProductSettings,
        expected_revision: u64,
    ) -> Result<(), Value> {
        if !self.ready {
            return Err(json!({"ok":false,"code":"settings_loading"}));
        }
        if self.pending.is_some() {
            return Err(json!({"ok":false,"code":"settings_busy"}));
        }
        if expected_revision != self.revision {
            return Err(json!({
                "ok":false,
                "error":{"code":"revision_conflict","current_revision":self.revision}
            }));
        }
        settings.validate().map_err(
            |message| json!({"ok":false,"error":{"code":"invalid_request","message":message}}),
        )?;
        let operation_id = self.next_operation_id("update");
        self.pending = Some(Pending {
            operation: Operation::Update {
                management_request,
                expected_revision,
                settings: settings.clone(),
            },
            call: Call::Submit {
                payload: settings.product_update_payload(expected_revision),
                operation_id,
            },
            retry_attempt: 0,
        });
        Ok(())
    }

    pub fn advance(
        &mut self,
        runtime: &RuntimeClient,
        settings_epoch: &mut u64,
    ) -> ModuleResult<()> {
        let Some(mut pending) = self.pending.take() else {
            return Ok(());
        };
        match pending.call.clone() {
            Call::Delay { ticks } if ticks > 1 => {
                pending.call = Call::Delay { ticks: ticks - 1 };
                self.pending = Some(pending);
                Ok(())
            }
            Call::Delay { .. } => {
                self.prepare_retry(&mut pending);
                self.pending = Some(pending);
                Ok(())
            }
            Call::Submit {
                payload,
                operation_id,
            } => {
                let retry = Call::Submit {
                    payload: payload.clone(),
                    operation_id: operation_id.clone(),
                };
                match runtime.send(Command::Request {
                    service: SERVICE.into(),
                    payload,
                    timeout_ms: TIMEOUT_MS,
                    operation_id,
                    parent: None,
                }) {
                    Ok(Response::Accepted { id }) => {
                        pending.call = Call::Waiting { request: id };
                        self.pending = Some(pending);
                        Ok(())
                    }
                    Ok(response @ Response::Result { .. }) => self.consume(
                        runtime,
                        pending.operation,
                        response,
                        settings_epoch,
                        pending.retry_attempt,
                    ),
                    Ok(Response::Error { code, message }) => self.submission_failed(
                        runtime,
                        pending.operation,
                        code,
                        message,
                        pending.retry_attempt,
                    ),
                    Ok(response) => self.consume(
                        runtime,
                        pending.operation,
                        response,
                        settings_epoch,
                        pending.retry_attempt,
                    ),
                    Err(_) => {
                        pending.call = retry;
                        pending.retry_attempt = pending.retry_attempt.saturating_add(1);
                        self.pending = Some(with_backoff(pending));
                        Ok(())
                    }
                }
            }
            Call::Waiting { request } => {
                let response = match runtime.send(Command::TakeResult { request }) {
                    Ok(Response::Pending) => {
                        pending.call = Call::Waiting { request };
                        self.pending = Some(pending);
                        return Ok(());
                    }
                    Err(_) => {
                        pending.call = Call::Waiting { request };
                        pending.retry_attempt = pending.retry_attempt.saturating_add(1);
                        self.pending = Some(with_backoff(pending));
                        return Ok(());
                    }
                    Ok(response) => response,
                };
                let Some(response) = release_terminal_child_result(request, response, |request| {
                    runtime.send(Command::ReleaseResult { request })
                }) else {
                    pending.call = Call::Waiting { request };
                    pending.retry_attempt = pending.retry_attempt.saturating_add(1);
                    self.pending = Some(with_backoff(pending));
                    return Ok(());
                };
                self.consume(
                    runtime,
                    pending.operation,
                    response,
                    settings_epoch,
                    pending.retry_attempt,
                )
            }
        }
    }

    fn consume(
        &mut self,
        runtime: &RuntimeClient,
        operation: Operation,
        response: Response,
        settings_epoch: &mut u64,
        retry_attempt: u32,
    ) -> ModuleResult<()> {
        let Response::Result { payload } = response else {
            return self.submission_failed(
                runtime,
                operation,
                "unexpected_response".into(),
                "configuration service returned an unexpected response".into(),
                retry_attempt,
            );
        };
        match operation {
            Operation::Load => match self.consume_document(payload, settings_epoch) {
                Ok(()) => Ok(()),
                Err(_) => {
                    self.schedule_load_retry(retry_attempt.saturating_add(1));
                    Ok(())
                }
            },
            Operation::Update {
                management_request,
                expected_revision,
                settings,
            } => {
                if payload.get("ok").and_then(Value::as_bool) == Some(true) {
                    match ProductSettings::from_product_document(&payload) {
                        Ok((actual, revision))
                            if expected_revision.checked_add(1) == Some(revision)
                                && actual == settings =>
                        {
                            self.apply(actual, revision, settings_epoch);
                            complete(runtime, management_request, self.management_response())
                        }
                        _ => self.schedule_reconciliation(
                            management_request,
                            UpdateResolution::Unknown(
                                "configuration returned an inconsistent committed document".into(),
                            ),
                        ),
                    }
                } else {
                    let code = payload
                        .pointer("/error/code")
                        .and_then(Value::as_str)
                        .unwrap_or("configuration_rejected");
                    let resolution = if code == "revision_conflict" {
                        UpdateResolution::Conflict
                    } else {
                        UpdateResolution::Unknown(format!("configuration returned {code}"))
                    };
                    self.schedule_reconciliation(management_request, resolution)
                }
            }
            Operation::Reconcile {
                management_request,
                resolution,
            } => {
                let (response, readback_failed) = match self
                    .consume_document(payload, settings_epoch)
                {
                    Ok(()) => {
                        let response = match resolution {
                            UpdateResolution::Conflict => json!({
                                "ok":false,
                                "error":{"code":"revision_conflict","current_revision":self.revision}
                            }),
                            UpdateResolution::Unknown(message) => json!({
                                "ok":false,
                                "code":"update_outcome_unknown",
                                "message":message,
                                "revision":self.revision
                            }),
                        };
                        (response, false)
                    }
                    Err(error) => {
                        let reason = match resolution {
                            UpdateResolution::Conflict => {
                                format!("configuration readback failed: {error}")
                            }
                            UpdateResolution::Unknown(message) => {
                                format!("{message}; readback failed: {error}")
                            }
                        };
                        (
                            json!({"ok":false,"code":"update_outcome_unknown","message":reason}),
                            true,
                        )
                    }
                };
                complete(runtime, management_request, response)?;
                if readback_failed {
                    self.schedule_load_retry(0);
                }
                Ok(())
            }
        }
    }

    fn consume_document(&mut self, payload: Value, settings_epoch: &mut u64) -> Result<(), String> {
        let (settings, revision) = ProductSettings::from_product_document(&payload)?;
        self.apply(settings, revision, settings_epoch);
        Ok(())
    }

    fn apply(&mut self, settings: ProductSettings, revision: u64, settings_epoch: &mut u64) {
        if self.settings != settings {
            *settings_epoch = settings_epoch.saturating_add(1);
        }
        self.settings = settings;
        self.revision = revision;
        self.ready = true;
    }

    fn submission_failed(
        &mut self,
        runtime: &RuntimeClient,
        operation: Operation,
        code: String,
        message: String,
        retry_attempt: u32,
    ) -> ModuleResult<()> {
        match operation {
            Operation::Load => {
                self.schedule_load_retry(retry_attempt.saturating_add(1));
                Ok(())
            }
            Operation::Update {
                management_request, ..
            } => self.schedule_reconciliation(
                management_request,
                UpdateResolution::Unknown(format!("{code}: {message}")),
            ),
            Operation::Reconcile {
                management_request,
                resolution,
            } => {
                let message = match resolution {
                    UpdateResolution::Conflict => {
                        format!("revision conflict readback failed: {code}: {message}")
                    }
                    UpdateResolution::Unknown(reason) => {
                        format!("{reason}; readback failed: {code}: {message}")
                    }
                };
                complete(
                    runtime,
                    management_request,
                    json!({
                        "ok":false,"code":"update_outcome_unknown","message":message,
                        "revision":self.revision
                    }),
                )?;
                self.schedule_load_retry(0);
                Ok(())
            }
        }
    }

    fn schedule_reconciliation(
        &mut self,
        management_request: u64,
        resolution: UpdateResolution,
    ) -> ModuleResult<()> {
        let operation_id = self.next_operation_id("readback");
        self.pending = Some(Pending {
            operation: Operation::Reconcile {
                management_request,
                resolution,
            },
            call: Call::Submit {
                payload: json!({"action":"product_get"}),
                operation_id,
            },
            retry_attempt: 0,
        });
        Ok(())
    }

    fn schedule_load_retry(&mut self, retry_attempt: u32) {
        self.pending = Some(Pending {
            operation: Operation::Load,
            call: Call::Delay {
                ticks: retry_delay(retry_attempt),
            },
            retry_attempt,
        });
    }

    fn prepare_retry(&mut self, pending: &mut Pending) {
        pending.call = match &pending.operation {
            Operation::Load => Call::Submit {
                payload: json!({"action":"product_get"}),
                operation_id: self.next_operation_id("load-retry"),
            },
            Operation::Reconcile {
                management_request, ..
            } => Call::Submit {
                payload: json!({"action":"product_get"}),
                operation_id: self
                    .next_operation_id(&format!("readback-retry-{management_request}")),
            },
            Operation::Update { .. } => unreachable!("updates reconcile before retrying"),
        };
    }

    fn next_operation_id(&mut self, purpose: &str) -> String {
        self.operation_sequence = self.operation_sequence.saturating_add(1);
        format!(
            "lutris.product-settings.{purpose}.{}",
            self.operation_sequence
        )
    }
}

fn with_backoff(mut pending: Pending) -> Pending {
    pending.call = Call::Delay {
        ticks: retry_delay(pending.retry_attempt),
    };
    pending
}

fn retry_delay(attempt: u32) -> u32 {
    1_u32
        .checked_shl(attempt.min(6))
        .unwrap_or(MAX_RETRY_TICKS)
        .min(MAX_RETRY_TICKS)
}

pub(super) fn release_terminal_child_result(
    request: u64,
    response: Response,
    mut release: impl FnMut(u64) -> ModuleResult<Response>,
) -> Option<Response> {
    if matches!(response, Response::Pending) {
        return None;
    }
    if matches!(&response, Response::Error { code, .. } if code == "not_found") {
        return Some(response);
    }
    match release(request) {
        Ok(Response::Ok) => Some(response),
        Ok(Response::Error { code, .. }) if code == "not_found" => Some(response),
        _ => None,
    }
}

fn complete(runtime: &RuntimeClient, request: u64, payload: Value) -> ModuleResult<()> {
    match runtime.send(Command::Complete { request, payload })? {
        Response::Ok => Ok(()),
        Response::Error { code, .. } if code == "request_ended" || code == "not_found" => Ok(()),
        response => Err(format!("Lutris settings response failed: {response:?}")),
    }
}
