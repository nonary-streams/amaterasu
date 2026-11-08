//! Product-owned Lutris settings and management-only durable updates.

use crate::settings::ProductSettings;
use amaterasu_module_sdk::{Command, ModuleResult, Response, RuntimeClient};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

const SERVICE: &str = "amaterasu.configuration/1";
const TIMEOUT_MS: u64 = 30_000;
const MAX_RETRY_TICKS: u32 = 64;
const BROKER_RESULT_RETENTION: Duration = Duration::from_secs(300);

#[derive(Clone)]
pub struct ProductConfiguration {
    settings: ProductSettings,
    revision: u64,
    ready: bool,
    pending: Option<Pending>,
    generation: String,
    operation_sequence: u64,
    shutdown_completion: Option<(u64, Value)>,
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
    Releasing {
        request: u64,
        response: Response,
    },
    Delay {
        ticks: u32,
        resume: Option<Box<Call>>,
    },
}

#[derive(Clone)]
struct Pending {
    operation: Operation,
    call: Call,
    retry_attempt: u32,
    first_attempt: Option<Instant>,
}

impl ProductConfiguration {
    pub fn new(generation: &str) -> Self {
        let mut configuration = Self {
            settings: ProductSettings::default(),
            revision: 0,
            ready: false,
            pending: None,
            generation: generation.to_owned(),
            operation_sequence: 0,
            shutdown_completion: None,
        };
        let operation_id = configuration.next_operation_id("load");
        configuration.pending = Some(Pending {
            operation: Operation::Load,
            call: Call::Submit {
                payload: json!({"action":"product_get"}),
                operation_id,
            },
            retry_attempt: 0,
            first_attempt: None,
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

    /// Retire the exact child configuration request before the module unloads.
    /// An uncertain submission is looked up by its original identity; broker
    /// expiry is never treated as proof that a delivered update stopped.
    pub fn poll_shutdown(
        &mut self,
        runtime: &RuntimeClient,
        settings_epoch: &mut u64,
    ) -> ModuleResult<bool> {
        self.poll_shutdown_with(settings_epoch, &mut |command| runtime.send(command))
    }

    fn poll_shutdown_with(
        &mut self,
        settings_epoch: &mut u64,
        send: &mut impl FnMut(Command) -> ModuleResult<Response>,
    ) -> ModuleResult<bool> {
        if !self.flush_shutdown_completion(send) {
            return Ok(false);
        }
        let Some(mut pending) = self.pending.take() else {
            return Ok(self.shutdown_completion.is_none());
        };
        match pending.call.clone() {
            Call::Delay {
                resume: Some(resume),
                ..
            } => {
                pending.call = *resume;
                self.pending = Some(pending);
                Ok(false)
            }
            Call::Delay { resume: None, .. } => {
                self.finish_shutdown_without_result(pending, "shutting_down");
                Ok(self.flush_shutdown_completion(send) && self.pending.is_none())
            }
            Call::Submit {
                payload,
                operation_id,
            } => {
                let Some(first_attempt) = pending.first_attempt else {
                    self.finish_shutdown_without_result(pending, "shutting_down");
                    return Ok(self.flush_shutdown_completion(send) && self.pending.is_none());
                };
                if Instant::now().saturating_duration_since(first_attempt)
                    >= BROKER_RESULT_RETENTION
                {
                    self.pending = Some(pending);
                    return Ok(false);
                }
                match send(Command::LookupRequest {
                    service: SERVICE.into(),
                    payload,
                    operation_id,
                    parent: None,
                }) {
                    Ok(Response::Accepted { id }) => {
                        pending.call = Call::Waiting { request: id };
                        self.pending = Some(pending);
                    }
                    Ok(Response::Error { code, .. })
                        if code == "not_found"
                            && Instant::now().saturating_duration_since(first_attempt)
                                < BROKER_RESULT_RETENTION =>
                    {
                        self.finish_shutdown_without_result(pending, "shutting_down");
                    }
                    _ => self.pending = Some(pending),
                }
                Ok(self.pending.is_none() && self.flush_shutdown_completion(send))
            }
            Call::Waiting { request } => {
                match send(Command::TakeResult { request }) {
                    Ok(Response::Pending) | Err(_) => {
                        pending.call = Call::Waiting { request };
                        self.pending = Some(pending);
                        Ok(false)
                    }
                    Ok(Response::Error { .. })
                        if matches!(&pending.operation, Operation::Update { .. }) =>
                    {
                        // Expiry and cancellation do not establish that a
                        // durable settings write has stopped.
                        pending.call = Call::Waiting { request };
                        self.pending = Some(pending);
                        Ok(false)
                    }
                    Ok(response) => {
                        pending.call = Call::Releasing { request, response };
                        self.pending = Some(pending);
                        Ok(false)
                    }
                }
            }
            Call::Releasing { request, response } => {
                match send(Command::ReleaseResult { request }) {
                    Ok(Response::Ok) => {
                        self.finish_shutdown_result(pending.operation, response, settings_epoch);
                        Ok(self.flush_shutdown_completion(send) && self.pending.is_none())
                    }
                    Ok(Response::Error { code, .. }) if code == "not_found" => {
                        self.finish_shutdown_result(pending.operation, response, settings_epoch);
                        Ok(self.flush_shutdown_completion(send) && self.pending.is_none())
                    }
                    _ => {
                        pending.call = Call::Releasing { request, response };
                        self.pending = Some(pending);
                        Ok(false)
                    }
                }
            }
        }
    }

    fn flush_shutdown_completion(
        &mut self,
        send: &mut impl FnMut(Command) -> ModuleResult<Response>,
    ) -> bool {
        let Some((request, payload)) = self.shutdown_completion.as_ref() else {
            return true;
        };
        match send(Command::Complete {
            request: *request,
            payload: payload.clone(),
        }) {
            Ok(Response::Ok) => {
                self.shutdown_completion = None;
                true
            }
            Ok(Response::Error { code, .. })
                if matches!(
                    code.as_str(),
                    "request_ended" | "not_found" | "deadline_exceeded" | "cancelled"
                ) =>
            {
                self.shutdown_completion = None;
                true
            }
            _ => false,
        }
    }

    fn finish_shutdown_without_result(&mut self, pending: Pending, code: &str) {
        match pending.operation {
            Operation::Load => {}
            Operation::Update {
                management_request, ..
            } => {
                self.shutdown_completion =
                    Some((management_request, json!({"ok":false,"code":code})));
            }
            Operation::Reconcile {
                management_request, ..
            } => {
                self.shutdown_completion = Some((
                    management_request,
                    json!({
                        "ok":false,"code":"update_outcome_unknown",
                        "message":"settings reconciliation was interrupted by shutdown",
                        "revision":self.revision
                    }),
                ));
            }
        }
    }

    fn finish_shutdown_result(
        &mut self,
        operation: Operation,
        response: Response,
        settings_epoch: &mut u64,
    ) {
        match operation {
            Operation::Load => {
                if let Response::Result { payload } = response {
                    let _ = self.consume_document(payload, settings_epoch);
                }
            }
            Operation::Update {
                management_request,
                expected_revision,
                settings,
            } => {
                let committed = match response {
                    Response::Result { payload }
                        if payload.get("ok").and_then(Value::as_bool) == Some(true) =>
                    {
                        ProductSettings::from_product_document(&payload)
                            .ok()
                            .filter(|(actual, revision)| {
                                expected_revision.checked_add(1) == Some(*revision)
                                    && *actual == settings
                            })
                    }
                    _ => None,
                };
                if let Some((actual, revision)) = committed {
                    self.apply(actual, revision, settings_epoch);
                    self.shutdown_completion =
                        Some((management_request, self.management_response()));
                } else {
                    self.shutdown_completion = Some((
                        management_request,
                        json!({
                            "ok":false,"code":"update_outcome_unknown",
                            "revision":self.revision
                        }),
                    ));
                }
            }
            Operation::Reconcile {
                management_request,
                resolution,
            } => {
                let document = match response {
                    Response::Result { payload } => self.consume_document(payload, settings_epoch),
                    Response::Error { code, message } => Err(format!("{code}: {message}")),
                    _ => Err("unexpected configuration response".into()),
                };
                let payload = match document {
                    Ok(()) => match resolution {
                        UpdateResolution::Conflict => json!({
                            "ok":false,
                            "error":{"code":"revision_conflict","current_revision":self.revision}
                        }),
                        UpdateResolution::Unknown(message) => json!({
                            "ok":false,"code":"update_outcome_unknown","message":message,
                            "revision":self.revision
                        }),
                    },
                    Err(error) => json!({
                        "ok":false,"code":"update_outcome_unknown",
                        "message":format!("settings reconciliation failed during shutdown: {error}"),
                        "revision":self.revision
                    }),
                };
                self.shutdown_completion = Some((management_request, payload));
            }
        }
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
            first_attempt: None,
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
            Call::Delay { ticks, resume } if ticks > 1 => {
                pending.call = Call::Delay {
                    ticks: ticks - 1,
                    resume,
                };
                self.pending = Some(pending);
                Ok(())
            }
            Call::Delay { resume, .. } => {
                if let Some(resume) = resume {
                    pending.call = *resume;
                } else {
                    self.prepare_retry(&mut pending);
                }
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
                pending.first_attempt.get_or_insert_with(Instant::now);
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
            Call::Releasing { request, response } => {
                match runtime.send(Command::ReleaseResult { request }) {
                    Ok(Response::Ok) => self.consume(
                        runtime,
                        pending.operation,
                        response,
                        settings_epoch,
                        pending.retry_attempt,
                    ),
                    Ok(Response::Error { code, .. }) if code == "not_found" => self.consume(
                        runtime,
                        pending.operation,
                        response,
                        settings_epoch,
                        pending.retry_attempt,
                    ),
                    _ => {
                        pending.call = Call::Releasing { request, response };
                        self.pending = Some(pending);
                        Ok(())
                    }
                }
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
            first_attempt: None,
        });
        Ok(())
    }

    fn schedule_load_retry(&mut self, retry_attempt: u32) {
        self.pending = Some(Pending {
            operation: Operation::Load,
            call: Call::Delay {
                ticks: retry_delay(retry_attempt),
                resume: None,
            },
            retry_attempt,
            first_attempt: None,
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
        pending.first_attempt = None;
    }

    fn next_operation_id(&mut self, purpose: &str) -> String {
        self.operation_sequence = self.operation_sequence.saturating_add(1);
        format!(
            "lutris.product-settings.{purpose}.{}.{}",
            self.generation, self.operation_sequence
        )
    }
}

fn with_backoff(mut pending: Pending) -> Pending {
    let resume = Some(Box::new(pending.call.clone()));
    pending.call = Call::Delay {
        ticks: retry_delay(pending.retry_attempt),
        resume,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn document(settings: &ProductSettings, revision: u64) -> Value {
        json!({
            "ok":true,
            "found":true,
            "version":crate::settings::PRODUCT_SETTINGS_VERSION,
            "revision":revision,
            "values":settings,
        })
    }

    fn pending_update(call: Call) -> ProductConfiguration {
        let desired = ProductSettings {
            include_steam: true,
            ..ProductSettings::default()
        };
        let mut configuration = ProductConfiguration::new("test");
        configuration.pending = Some(Pending {
            operation: Operation::Update {
                management_request: 45,
                expected_revision: 1,
                settings: desired,
            },
            call,
            retry_attempt: 0,
            first_attempt: Some(Instant::now()),
        });
        configuration
    }

    #[test]
    fn retry_backoff_keeps_the_exact_waiting_child_id() {
        let pending = Pending {
            operation: Operation::Load,
            call: Call::Waiting { request: 81 },
            retry_attempt: 1,
            first_attempt: Some(Instant::now()),
        };
        let delayed = with_backoff(pending);
        assert!(matches!(
            delayed.call,
            Call::Delay {
                resume: Some(resume),
                ..
            } if matches!(*resume, Call::Waiting { request: 81 })
        ));
    }

    #[test]
    fn shutdown_releases_the_admitted_startup_settings_read() {
        let settings = ProductSettings::default();
        let mut configuration = ProductConfiguration::new("test");
        configuration.pending.as_mut().unwrap().call = Call::Waiting { request: 16 };
        let mut settings_epoch = 0;
        assert!(
            !configuration
                .poll_shutdown_with(&mut settings_epoch, &mut |command| {
                    assert!(matches!(command, Command::TakeResult { request: 16 }));
                    Ok(Response::Result {
                        payload: document(&settings, 3),
                    })
                })
                .unwrap()
        );
        assert!(
            configuration
                .poll_shutdown_with(&mut settings_epoch, &mut |command| {
                    assert!(matches!(command, Command::ReleaseResult { request: 16 }));
                    Ok(Response::Ok)
                })
                .unwrap()
        );
        assert!(configuration.ready());
        assert_eq!(configuration.revision(), 3);
    }

    #[test]
    fn shutdown_waits_for_an_admitted_update_and_releases_its_result() {
        let desired = ProductSettings {
            include_steam: true,
            ..ProductSettings::default()
        };
        let mut configuration = pending_update(Call::Waiting { request: 17 });
        let mut settings_epoch = 0;
        assert!(
            !configuration
                .poll_shutdown_with(&mut settings_epoch, &mut |command| {
                    assert!(matches!(command, Command::TakeResult { request: 17 }));
                    Ok(Response::Pending)
                })
                .unwrap()
        );
        assert!(
            !configuration
                .poll_shutdown_with(&mut settings_epoch, &mut |command| {
                    assert!(matches!(command, Command::TakeResult { request: 17 }));
                    Ok(Response::Result {
                        payload: document(&desired, 2),
                    })
                })
                .unwrap()
        );
        assert!(
            configuration
                .poll_shutdown_with(&mut settings_epoch, &mut |command| {
                    match command {
                        Command::ReleaseResult { request: 17 } => Ok(Response::Ok),
                        Command::Complete {
                            request: 45,
                            payload,
                        } => {
                            assert_eq!(payload["revision"], 2);
                            Ok(Response::Ok)
                        }
                        other => panic!("unexpected shutdown command: {other:?}"),
                    }
                })
                .unwrap()
        );
        assert!(configuration.pending.is_none());
        assert_eq!(configuration.revision(), 2);
        assert!(configuration.settings().include_steam);
    }

    #[test]
    fn shutdown_looks_up_lost_update_admission_without_replaying_it() {
        let desired = ProductSettings {
            include_steam: true,
            ..ProductSettings::default()
        };
        let operation_id = "lutris.product-settings.update.8".to_owned();
        let mut configuration = pending_update(Call::Submit {
            payload: desired.product_update_payload(1),
            operation_id: operation_id.clone(),
        });
        let mut settings_epoch = 0;
        assert!(
            !configuration
                .poll_shutdown_with(&mut settings_epoch, &mut |command| {
                    assert!(matches!(command, Command::LookupRequest {
                        service, operation_id: ref found, ..
                    } if service == SERVICE && found == &operation_id));
                    Ok(Response::Accepted { id: 23 })
                })
                .unwrap()
        );
        assert!(matches!(
            configuration.pending.as_ref().unwrap().call,
            Call::Waiting { request: 23 }
        ));
    }

    unsafe extern "C" fn deduplicating_same_principal(
        context: *mut std::ffi::c_void,
        input: amaterasu_module_sdk::abi::BorrowedBytes,
        reply: amaterasu_module_sdk::abi::ReplySink,
    ) -> amaterasu_module_sdk::abi::Status {
        use std::collections::HashMap;
        use std::sync::Mutex;

        // SAFETY: the boxed request map stays live for both synchronous calls.
        let requests = unsafe { &*context.cast::<Mutex<HashMap<String, u64>>>() };
        // SAFETY: RuntimeClient passes a live command buffer for this call.
        let command: Command = serde_json::from_slice(unsafe { input.read().unwrap() }).unwrap();
        let Command::Request {
            service,
            payload,
            operation_id,
            parent,
            ..
        } = command
        else {
            panic!("expected product settings request");
        };
        assert_eq!(service, SERVICE);
        assert_eq!(payload, json!({"action":"product_get"}));
        assert_eq!(parent, None);
        let mut requests = requests.lock().unwrap();
        let next = requests.len() as u64 + 1;
        let id = *requests.entry(operation_id).or_insert(next);
        let response = serde_json::to_vec(&Response::Accepted { id }).unwrap();
        // SAFETY: the reply sink synchronously copies these response bytes.
        unsafe { reply.send(&response) };
        amaterasu_module_sdk::abi::STATUS_OK
    }

    #[test]
    fn same_principal_restarts_do_not_reuse_a_retained_startup_result() {
        use std::collections::HashMap;
        use std::sync::Mutex;

        let requests = Box::new(Mutex::new(HashMap::<String, u64>::new()));
        let endpoint = amaterasu_module_sdk::abi::HostApi {
            header: amaterasu_module_sdk::abi::AbiHeader::of::<amaterasu_module_sdk::abi::HostApi>(
            ),
            context: std::ptr::from_ref(&*requests).cast_mut().cast(),
            invoke: deduplicating_same_principal,
        };
        // SAFETY: the host context remains live through both starts.
        let runtime = unsafe { RuntimeClient::from_endpoint(endpoint) };
        let mut first_epoch = 0;
        let mut first = ProductConfiguration::new("rapid-reload-one");
        first.advance(&runtime, &mut first_epoch).unwrap();
        let first_id = match &first.pending.as_ref().unwrap().call {
            Call::Waiting { request } => *request,
            _ => panic!("first load should be accepted"),
        };

        let mut second_epoch = 0;
        let mut second = ProductConfiguration::new("rapid-reload-two");
        second.advance(&runtime, &mut second_epoch).unwrap();
        let second_id = match &second.pending.as_ref().unwrap().call {
            Call::Waiting { request } => *request,
            _ => panic!("second load should be accepted"),
        };
        assert_ne!(first_id, second_id);
        assert_eq!(requests.lock().unwrap().len(), 2);
    }
}
