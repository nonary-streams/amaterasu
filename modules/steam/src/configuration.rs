//! Durable Steam product settings and their asynchronous configuration RPC lifecycle.
use crate::Provider;
use crate::settings::ProductSettings;
use amaterasu_module_sdk::{Command, ModuleResult, Response, RuntimeClient};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

const CONFIGURATION_SERVICE: &str = "amaterasu.configuration/1";
pub(super) const CONFIGURATION_TIMEOUT_MS: u64 = 30_000;

pub(super) struct ProductConfiguration {
    startup_id: String,
    settings: ProductSettings,
    revision: u64,
    ready: bool,
    pending: Option<PendingConfiguration>,
    shutdown_result_observed: Option<u64>,
    retained_result: Option<(u64, Response)>,
}

enum PendingConfigurationOperation {
    StartupLoad,
    Update {
        management_request: u64,
        expected_revision: u64,
        settings: ProductSettings,
    },
    ReconcileUpdate {
        management_request: u64,
        resolution: UpdateResolution,
    },
    ReconcileRuntime {
        attempt: u32,
    },
}

#[derive(Clone, Debug)]
enum UpdateResolution {
    Rejected(Value),
    Conflict { current_revision: Option<u64> },
    Unknown { reason: String },
}

enum PendingConfigurationCall {
    Submit {
        payload: Value,
        timeout_ms: u64,
        operation_id: String,
        parent: Option<u64>,
        first_sent: Option<Instant>,
    },
    Waiting {
        request: u64,
    },
}

struct PendingConfiguration {
    call: PendingConfigurationCall,
    operation: PendingConfigurationOperation,
    delay_ticks: u32,
}

impl ProductConfiguration {
    pub(super) fn new(startup_id: &str) -> Self {
        Self {
            startup_id: startup_id.to_owned(),
            settings: ProductSettings::default(),
            revision: 0,
            ready: false,
            shutdown_result_observed: None,
            retained_result: None,
            pending: Some(PendingConfiguration {
                call: PendingConfigurationCall::Submit {
                    payload: json!({"action":"product_get"}),
                    timeout_ms: CONFIGURATION_TIMEOUT_MS,
                    operation_id: format!("steam.product-settings.load.{startup_id}"),
                    parent: None,
                    first_sent: None,
                },
                operation: PendingConfigurationOperation::StartupLoad,
                delay_ticks: 0,
            }),
        }
    }

    pub(super) fn is_busy(&self) -> bool {
        self.pending.is_some()
    }

    /// Retire only work already submitted. Look up a lost admission without
    /// dispatching new work while the broker still retains that identity.
    /// Cancellation does not prove a delivered settings mutation has finished.
    pub(super) fn poll_shutdown(&mut self, runtime: &RuntimeClient) -> bool {
        self.poll_shutdown_with(&mut |command| runtime.send(command))
    }

    fn poll_shutdown_with(
        &mut self,
        send: &mut impl FnMut(Command) -> ModuleResult<Response>,
    ) -> bool {
        self.poll_shutdown_with_clock(send, &mut Instant::now)
    }

    fn poll_shutdown_with_clock(
        &mut self,
        send: &mut impl FnMut(Command) -> ModuleResult<Response>,
        clock: &mut impl FnMut() -> Instant,
    ) -> bool {
        let Some(pending) = self.pending.as_mut() else {
            return true;
        };
        match &pending.call {
            PendingConfigurationCall::Submit {
                first_sent: None, ..
            } => {
                self.pending = None;
                true
            }
            PendingConfigurationCall::Submit {
                payload,
                operation_id,
                parent,
                first_sent: Some(started),
                ..
            } => {
                if clock().saturating_duration_since(*started) >= Duration::from_secs(300) {
                    return false;
                }
                match send(Command::LookupRequest {
                    service: CONFIGURATION_SERVICE.into(),
                    payload: payload.clone(),
                    operation_id: operation_id.clone(),
                    parent: *parent,
                }) {
                    Ok(Response::Accepted { id }) => {
                        pending.call = PendingConfigurationCall::Waiting { request: id }
                    }
                    Ok(Response::Error { code, .. })
                        if code == "not_found"
                            && clock().saturating_duration_since(*started)
                                < Duration::from_secs(300) =>
                    {
                        self.pending = None;
                        return true;
                    }
                    _ => {}
                }
                false
            }
            PendingConfigurationCall::Waiting { request } => {
                let request = *request;
                if self.shutdown_result_observed != Some(request)
                    && !self
                        .retained_result
                        .as_ref()
                        .is_some_and(|(id, _)| *id == request)
                {
                    if !matches!(
                        send(Command::TakeResult { request }),
                        Ok(Response::Result { .. })
                    ) {
                        return false;
                    }
                    self.shutdown_result_observed = Some(request);
                }
                let released = match send(Command::ReleaseResult { request }) {
                    Ok(Response::Ok) => true,
                    Ok(Response::Error { code, .. }) if code == "not_found" => true,
                    _ => false,
                };
                if !released {
                    return false;
                }
                self.pending = None;
                self.shutdown_result_observed = None;
                self.retained_result = None;
                true
            }
        }
    }

    pub(super) fn is_ready(&self) -> bool {
        self.ready
    }

    pub(super) fn settings(&self) -> &ProductSettings {
        &self.settings
    }

    #[cfg(test)]
    pub(super) fn revision(&self) -> u64 {
        self.revision
    }

    pub(super) fn management_response(&self) -> Value {
        self.settings.management_response(self.revision)
    }

    pub(super) fn begin_update(
        &mut self,
        management_request: u64,
        settings: ProductSettings,
        expected_revision: u64,
        remaining_ms: u64,
    ) -> Result<(), Value> {
        let Some(timeout_ms) = remaining_ms.min(CONFIGURATION_TIMEOUT_MS).checked_sub(1) else {
            return Err(json!({"ok":false,"code":"deadline_exceeded"}));
        };
        if timeout_ms == 0 {
            return Err(json!({"ok":false,"code":"deadline_exceeded"}));
        }
        if self.pending.is_some() {
            return Err(json!({"ok":false,"code":"settings_busy"}));
        }

        settings.validate().map_err(
            |message| json!({"ok":false,"error":{"code":"invalid_request","message":message}}),
        )?;
        let settings = ProductSettings::for_host(settings);
        self.pending = Some(PendingConfiguration {
            call: PendingConfigurationCall::Submit {
                payload: settings.product_update_payload(expected_revision),
                timeout_ms,
                operation_id: format!("steam.product-settings.update.{management_request}"),
                parent: Some(management_request),
                first_sent: None,
            },
            operation: PendingConfigurationOperation::Update {
                management_request,
                expected_revision,
                settings,
            },
            delay_ticks: 0,
        });
        Ok(())
    }

    /// Advances one configuration lifecycle step. Steam request polling remains
    /// the caller's responsibility, so discovery/status traffic stays responsive.
    pub(super) fn advance(
        &mut self,
        runtime: &RuntimeClient,
        provider: &mut Provider,
    ) -> ModuleResult<()> {
        let Some(pending) = self.pending.take() else {
            return Ok(());
        };
        if pending.delay_ticks > 0 {
            self.pending = Some(PendingConfiguration {
                delay_ticks: pending.delay_ticks - 1,
                ..pending
            });
            return Ok(());
        }
        match pending.call {
            PendingConfigurationCall::Submit {
                payload,
                timeout_ms,
                operation_id,
                parent,
                first_sent,
            } => {
                let call = PendingConfigurationCall::Submit {
                    payload: payload.clone(),
                    timeout_ms,
                    operation_id: operation_id.clone(),
                    parent,
                    first_sent: Some(first_sent.unwrap_or_else(Instant::now)),
                };
                let retry_delay = background_retry_delay(&pending.operation);
                if first_sent.is_some_and(|started| started.elapsed() >= Duration::from_secs(300)) {
                    // The broker may have expired its deduplication record;
                    // replaying now could dispatch a second settings mutation.
                    self.pending = Some(PendingConfiguration {
                        call,
                        operation: pending.operation,
                        delay_ticks: retry_delay,
                    });
                    return Ok(());
                }
                match runtime.send(Command::Request {
                    service: CONFIGURATION_SERVICE.into(),
                    payload,
                    timeout_ms,
                    operation_id,
                    parent,
                }) {
                    Err(_) => {
                        self.pending = Some(PendingConfiguration {
                            call,
                            operation: pending.operation,
                            delay_ticks: retry_delay,
                        });
                        Ok(())
                    }
                    Ok(Response::Accepted { id }) => {
                        self.pending = Some(PendingConfiguration {
                            call: PendingConfigurationCall::Waiting { request: id },
                            operation: pending.operation,
                            delay_ticks: 0,
                        });
                        Ok(())
                    }
                    Ok(Response::Error { code, message }) => {
                        if first_sent.is_some() {
                            // A rejection of the retry cannot prove that the
                            // earlier attempt never reached the provider.
                            self.pending = Some(PendingConfiguration {
                                call,
                                operation: pending.operation,
                                delay_ticks: retry_delay,
                            });
                            Ok(())
                        } else {
                            self.handle_submission_failure(
                                runtime,
                                pending.operation,
                                code,
                                message,
                            )
                        }
                    }
                    Ok(response @ Response::Result { .. }) => {
                        self.consume_result(runtime, provider, pending.operation, response)
                    }
                    Ok(_) => {
                        // An unexpected reply does not prove admission failed.
                        self.pending = Some(PendingConfiguration {
                            call,
                            operation: pending.operation,
                            delay_ticks: retry_delay,
                        });
                        Ok(())
                    }
                }
            }
            PendingConfigurationCall::Waiting { request } => {
                let response = if let Some((id, response)) = &self.retained_result
                    && *id == request
                {
                    response.clone()
                } else {
                    match runtime.send(Command::TakeResult { request }) {
                        Ok(Response::Pending) => {
                            self.pending = Some(PendingConfiguration {
                                call: PendingConfigurationCall::Waiting { request },
                                operation: pending.operation,
                                delay_ticks: 0,
                            });
                            return Ok(());
                        }
                        Err(_) => {
                            let delay_ticks = background_retry_delay(&pending.operation);
                            self.pending = Some(PendingConfiguration {
                                call: PendingConfigurationCall::Waiting { request },
                                operation: pending.operation,
                                delay_ticks,
                            });
                            return Ok(());
                        }
                        Ok(response) => response,
                    }
                };
                if matches!(
                    pending.operation,
                    PendingConfigurationOperation::Update { .. }
                ) && matches!(response, Response::Error { .. })
                {
                    // A broker error (including cancellation or deadline) does
                    // not establish that an already delivered update stopped.
                    self.pending = Some(PendingConfiguration {
                        call: PendingConfigurationCall::Waiting { request },
                        operation: pending.operation,
                        delay_ticks: 1,
                    });
                    return Ok(());
                }
                let Some(released_response) =
                    release_terminal_child_result(request, response.clone(), |request| {
                        runtime.send(Command::ReleaseResult { request })
                    })
                else {
                    if matches!(response, Response::Result { .. }) {
                        self.retained_result = Some((request, response));
                    }
                    let delay_ticks = background_retry_delay(&pending.operation);
                    self.pending = Some(PendingConfiguration {
                        call: PendingConfigurationCall::Waiting { request },
                        operation: pending.operation,
                        delay_ticks,
                    });
                    return Ok(());
                };
                self.retained_result = None;
                self.consume_result(runtime, provider, pending.operation, released_response)
            }
        }
    }

    fn consume_result(
        &mut self,
        runtime: &RuntimeClient,
        provider: &mut Provider,
        operation: PendingConfigurationOperation,
        response: Response,
    ) -> ModuleResult<()> {
        match operation {
            PendingConfigurationOperation::StartupLoad => match response {
                Response::Result { payload }
                    if payload.get("ok").and_then(Value::as_bool) == Some(true) =>
                {
                    let (settings, revision) = ProductSettings::from_product_document(&payload)
                        .map_err(|error| format!("invalid Steam product settings: {error}"))?;
                    self.apply_settings(provider, settings, revision);
                    Ok(())
                }
                Response::Error { code, message } => Err(format!(
                    "Steam product settings unavailable: {code}: {message}"
                )),
                _ => Err("Steam product settings could not be loaded".into()),
            },
            PendingConfigurationOperation::Update {
                management_request,
                expected_revision,
                settings,
            } => match response {
                Response::Result { payload } => self.consume_update_payload(
                    runtime,
                    provider,
                    management_request,
                    expected_revision,
                    settings,
                    payload,
                ),
                Response::Error { code, message } => self.schedule_reconciliation(
                    management_request,
                    UpdateResolution::Unknown {
                        reason: format!("configuration update result was {code}: {message}"),
                    },
                ),
                response => self.schedule_reconciliation(
                    management_request,
                    UpdateResolution::Unknown {
                        reason: format!("unexpected configuration update result: {response:?}"),
                    },
                ),
            },
            PendingConfigurationOperation::ReconcileUpdate {
                management_request,
                resolution,
            } => match response {
                Response::Result { payload }
                    if payload.get("ok").and_then(Value::as_bool) == Some(true) =>
                {
                    match ProductSettings::from_product_document(&payload) {
                        Ok((settings, revision)) => {
                            self.apply_settings(provider, settings, revision);
                            self.complete_reconciliation(
                                runtime,
                                management_request,
                                resolution,
                                None,
                            )
                        }
                        Err(error) => self.complete_reconciliation(
                            runtime,
                            management_request,
                            resolution,
                            Some(format!("invalid readback: {error}")),
                        ),
                    }
                }
                Response::Result { payload } => self.complete_reconciliation(
                    runtime,
                    management_request,
                    resolution,
                    Some(format!("configuration rejected readback: {payload}")),
                ),
                Response::Error { code, message } => self.complete_reconciliation(
                    runtime,
                    management_request,
                    resolution,
                    Some(format!("readback failed: {code}: {message}")),
                ),
                response => self.complete_reconciliation(
                    runtime,
                    management_request,
                    resolution,
                    Some(format!("unexpected readback result: {response:?}")),
                ),
            },
            PendingConfigurationOperation::ReconcileRuntime { attempt } => {
                if let Response::Result { payload } = response
                    && payload.get("ok").and_then(Value::as_bool) == Some(true)
                    && let Ok((settings, revision)) =
                        ProductSettings::from_product_document(&payload)
                {
                    self.apply_settings(provider, settings, revision);
                    return Ok(());
                }
                self.schedule_background_retry(attempt)
            }
        }
    }

    fn handle_submission_failure(
        &mut self,
        runtime: &RuntimeClient,
        operation: PendingConfigurationOperation,
        code: String,
        message: String,
    ) -> ModuleResult<()> {
        match operation {
            PendingConfigurationOperation::StartupLoad => Err(format!(
                "Steam product settings unavailable: {code}: {message}"
            )),
            PendingConfigurationOperation::Update {
                management_request, ..
            } => complete_request(
                runtime,
                management_request,
                product_failure(&code, &message, None),
            ),
            PendingConfigurationOperation::ReconcileUpdate {
                management_request,
                resolution,
            } => self.complete_reconciliation(
                runtime,
                management_request,
                resolution,
                Some(format!("readback request failed: {code}: {message}")),
            ),
            PendingConfigurationOperation::ReconcileRuntime { attempt } => {
                self.schedule_background_retry(attempt)
            }
        }
    }

    fn consume_update_payload(
        &mut self,
        runtime: &RuntimeClient,
        provider: &mut Provider,
        management_request: u64,
        expected_revision: u64,
        settings: ProductSettings,
        payload: Value,
    ) -> ModuleResult<()> {
        if payload.get("ok").and_then(Value::as_bool) == Some(true) {
            match ProductSettings::from_product_document(&payload) {
                Ok((actual, revision))
                    if expected_revision.checked_add(1) == Some(revision) && actual == settings =>
                {
                    self.apply_settings(provider, actual, revision);
                    return complete_request(
                        runtime,
                        management_request,
                        self.settings.management_response(self.revision),
                    );
                }
                Ok(_) => {
                    return self.schedule_reconciliation(
                        management_request,
                        UpdateResolution::Unknown {
                            reason: "configuration acknowledged a different settings document"
                                .into(),
                        },
                    );
                }
                Err(error) => {
                    return self.schedule_reconciliation(
                        management_request,
                        UpdateResolution::Unknown {
                            reason: format!("configuration update response was malformed: {error}"),
                        },
                    );
                }
            }
        }

        let resolution = parse_update_failure(&payload);
        match resolution {
            UpdateResolution::Rejected(response) => {
                complete_request(runtime, management_request, response)
            }
            resolution => self.schedule_reconciliation(management_request, resolution),
        }
    }

    fn schedule_reconciliation(
        &mut self,
        management_request: u64,
        resolution: UpdateResolution,
    ) -> ModuleResult<()> {
        self.pending = Some(PendingConfiguration {
            call: PendingConfigurationCall::Submit {
                payload: json!({"action":"product_get"}),
                timeout_ms: CONFIGURATION_TIMEOUT_MS,
                operation_id: format!("steam.product-settings.reconcile.{management_request}"),
                parent: None,
                first_sent: None,
            },
            operation: PendingConfigurationOperation::ReconcileUpdate {
                management_request,
                resolution,
            },
            delay_ticks: 0,
        });
        Ok(())
    }

    fn schedule_background_reconciliation(&mut self, attempt: u32, delay_ticks: u32) {
        self.pending = Some(PendingConfiguration {
            call: PendingConfigurationCall::Submit {
                payload: json!({"action":"product_get"}),
                timeout_ms: CONFIGURATION_TIMEOUT_MS,
                operation_id: format!(
                    "steam.product-settings.background-reconcile.{}.{}",
                    self.startup_id, attempt
                ),
                parent: None,
                first_sent: None,
            },
            operation: PendingConfigurationOperation::ReconcileRuntime { attempt },
            delay_ticks,
        });
    }

    fn schedule_background_retry(&mut self, failed_attempt: u32) -> ModuleResult<()> {
        self.schedule_background_reconciliation(
            failed_attempt.saturating_add(1),
            reconciliation_retry_delay(failed_attempt),
        );
        Ok(())
    }

    fn complete_reconciliation(
        &mut self,
        runtime: &RuntimeClient,
        management_request: u64,
        resolution: UpdateResolution,
        readback_failure: Option<String>,
    ) -> ModuleResult<()> {
        let retry_readback = readback_failure.is_some();
        let response = reconciliation_response(resolution, self.revision, readback_failure);
        let completion = complete_request(runtime, management_request, response);
        if retry_readback {
            self.schedule_background_reconciliation(0, reconciliation_retry_delay(0));
        }
        completion
    }

    fn apply_settings(
        &mut self,
        provider: &mut Provider,
        settings: ProductSettings,
        revision: u64,
    ) {
        let needs_initial_scan = settings.enabled && (!self.ready || !provider.enabled);
        provider.set_enabled(settings.enabled);
        if needs_initial_scan && !provider.scan_in_progress() {
            provider.start_scan();
        }
        self.settings = settings;
        self.revision = revision;
        self.ready = true;
    }
}

pub(super) fn release_terminal_child_result(
    request: u64,
    response: Response,
    mut release: impl FnMut(u64) -> ModuleResult<Response>,
) -> Option<Response> {
    if matches!(&response, Response::Pending) {
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

fn parse_update_failure(payload: &Value) -> UpdateResolution {
    let error = payload.get("error");
    let code = error
        .and_then(|value| value.get("code"))
        .and_then(Value::as_str);
    let message = error
        .and_then(|value| value.get("message"))
        .and_then(Value::as_str)
        .unwrap_or("configuration update response was malformed");
    let current_revision = error
        .and_then(|value| value.get("current_revision"))
        .and_then(Value::as_u64);
    match code {
        Some("revision_conflict") => UpdateResolution::Conflict { current_revision },
        Some(code @ ("invalid_request" | "invalid_version" | "resource_exhausted")) => {
            UpdateResolution::Rejected(product_failure(code, message, current_revision))
        }
        Some(code) => UpdateResolution::Unknown {
            reason: format!("configuration update failed with {code}: {message}"),
        },
        None => UpdateResolution::Unknown {
            reason: format!("configuration update response was malformed: {payload}"),
        },
    }
}

fn product_failure(code: &str, message: &str, current_revision: Option<u64>) -> Value {
    let mut error = json!({"code":code,"message":message});
    if let (Some(revision), Some(error)) = (current_revision, error.as_object_mut()) {
        error.insert("current_revision".into(), json!(revision));
    }
    json!({"ok":false,"error":error})
}

fn reconciliation_response(
    resolution: UpdateResolution,
    authoritative_revision: u64,
    readback_failure: Option<String>,
) -> Value {
    match resolution {
        UpdateResolution::Rejected(response) => response,
        UpdateResolution::Conflict { current_revision } => product_failure(
            "revision_conflict",
            "product settings revision differs",
            if readback_failure.is_none() {
                Some(authoritative_revision)
            } else {
                current_revision
            },
        ),
        UpdateResolution::Unknown { reason } => {
            let message = match readback_failure {
                Some(failure) => format!("{reason}; {failure}"),
                None => format!(
                    "{reason}; durable settings were reloaded at revision {authoritative_revision}"
                ),
            };
            product_failure(
                "settings_outcome_unknown",
                &message,
                Some(authoritative_revision),
            )
        }
    }
}

fn reconciliation_retry_delay(failed_attempt: u32) -> u32 {
    1_u32 << failed_attempt.min(6)
}

fn background_retry_delay(operation: &PendingConfigurationOperation) -> u32 {
    match operation {
        PendingConfigurationOperation::ReconcileRuntime { attempt } => {
            reconciliation_retry_delay(*attempt)
        }
        _ => 0,
    }
}

fn complete_request(runtime: &RuntimeClient, request: u64, payload: Value) -> ModuleResult<()> {
    match runtime.send(Command::Complete { request, payload })? {
        Response::Ok => Ok(()),
        Response::Error { code, .. } if code == "request_ended" || code == "not_found" => Ok(()),
        response => Err(format!(
            "Steam configuration completion failed: {response:?}"
        )),
    }
}

#[cfg(test)]
mod startup_scan_tests {
    use super::*;

    #[test]
    fn startup_load_operation_identity_is_scoped_to_module_generation() {
        let first = ProductConfiguration::new("generation-one");
        let second = ProductConfiguration::new("generation-two");
        let operation_id = |configuration: &ProductConfiguration| {
            let Some(PendingConfiguration {
                call: PendingConfigurationCall::Submit { operation_id, .. },
                ..
            }) = configuration.pending.as_ref()
            else {
                panic!("startup load should be pending");
            };
            operation_id.clone()
        };
        assert_eq!(
            operation_id(&first),
            "steam.product-settings.load.generation-one"
        );
        assert_eq!(
            operation_id(&second),
            "steam.product-settings.load.generation-two"
        );
        assert_ne!(operation_id(&first), operation_id(&second));
    }

    #[test]
    fn background_reconciliation_identity_is_scoped_to_module_generation() {
        let mut first = ProductConfiguration::new("generation-one");
        first.schedule_background_reconciliation(0, 0);
        let mut second = ProductConfiguration::new("generation-two");
        second.schedule_background_reconciliation(0, 0);
        let operation_id = |configuration: &ProductConfiguration| {
            let Some(PendingConfiguration {
                call: PendingConfigurationCall::Submit { operation_id, .. },
                ..
            }) = configuration.pending.as_ref()
            else {
                panic!("background reconciliation should be pending");
            };
            operation_id.clone()
        };
        assert_eq!(
            operation_id(&first),
            "steam.product-settings.background-reconcile.generation-one.0"
        );
        assert_ne!(operation_id(&first), operation_id(&second));
    }

    unsafe extern "C" fn deduplicating_same_principal(
        context: *mut std::ffi::c_void,
        input: amaterasu_module_sdk::abi::BorrowedBytes,
        reply: amaterasu_module_sdk::abi::ReplySink,
    ) -> amaterasu_module_sdk::abi::Status {
        use std::collections::HashMap;
        use std::sync::Mutex;

        // SAFETY: the boxed request map stays alive for both synchronous calls.
        let requests = unsafe { &*context.cast::<Mutex<HashMap<String, u64>>>() };
        // SAFETY: RuntimeClient passes a valid command buffer for this call.
        let command: Command = serde_json::from_slice(unsafe { input.read().unwrap() }).unwrap();
        let Command::Request { operation_id, .. } = command else {
            panic!("expected product settings request");
        };
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
        let mut provider = Provider::new(Vec::new());
        let mut first = ProductConfiguration::new("rapid-reload-one");
        first.advance(&runtime, &mut provider).unwrap();
        let first_id = match &first.pending.as_ref().unwrap().call {
            PendingConfigurationCall::Waiting { request } => *request,
            _ => panic!("first load should be accepted"),
        };

        let mut second = ProductConfiguration::new("rapid-reload-two");
        second.advance(&runtime, &mut provider).unwrap();
        let second_id = match &second.pending.as_ref().unwrap().call {
            PendingConfigurationCall::Waiting { request } => *request,
            _ => panic!("second load should be accepted"),
        };
        assert_ne!(first_id, second_id);
        assert_eq!(requests.lock().unwrap().len(), 2);
    }

    unsafe extern "C" fn lost_then_rejected(
        context: *mut std::ffi::c_void,
        input: amaterasu_module_sdk::abi::BorrowedBytes,
        reply: amaterasu_module_sdk::abi::ReplySink,
    ) -> amaterasu_module_sdk::abi::Status {
        use std::sync::atomic::{AtomicUsize, Ordering};
        // SAFETY: the test retains this atomic for every synchronous call.
        let calls = unsafe { &*context.cast::<AtomicUsize>() };
        // SAFETY: the runtime client passes a valid synchronous command buffer.
        let command: Command = serde_json::from_slice(unsafe { input.read().unwrap() }).unwrap();
        match command {
            Command::Request { .. } if calls.fetch_add(1, Ordering::SeqCst) == 0 => {
                amaterasu_module_sdk::abi::STATUS_INTERNAL_ERROR
            }
            Command::Request { .. } => {
                let response =
                    serde_json::to_vec(&Response::error("unavailable", "retry rejected")).unwrap();
                // SAFETY: the reply sink synchronously copies these bytes.
                unsafe { reply.send(&response) };
                amaterasu_module_sdk::abi::STATUS_OK
            }
            _ => panic!("unexpected command"),
        }
    }

    #[test]
    fn retry_rejection_does_not_forget_lost_configuration_admission() {
        use std::sync::atomic::AtomicUsize;
        let calls = AtomicUsize::new(0);
        let endpoint = amaterasu_module_sdk::abi::HostApi {
            header: amaterasu_module_sdk::abi::AbiHeader::of::<amaterasu_module_sdk::abi::HostApi>(
            ),
            context: std::ptr::from_ref(&calls).cast_mut().cast(),
            invoke: lost_then_rejected,
        };
        // SAFETY: the endpoint context remains live for both synchronous calls.
        let runtime = unsafe { RuntimeClient::from_endpoint(endpoint) };
        let mut configuration = ProductConfiguration::new("test");
        let mut provider = Provider::new(Vec::new());
        configuration.advance(&runtime, &mut provider).unwrap();
        configuration.pending.as_mut().unwrap().delay_ticks = 0;
        configuration.advance(&runtime, &mut provider).unwrap();
        assert!(matches!(
            configuration.pending.as_ref().unwrap().call,
            PendingConfigurationCall::Submit {
                first_sent: Some(_),
                ..
            }
        ));
        assert!(configuration.poll_shutdown_with(&mut |command| {
            assert!(matches!(command, Command::LookupRequest { .. }));
            Ok(Response::error("not_found", "not admitted"))
        }));
    }

    unsafe extern "C" fn unexpected_admission_reply(
        _context: *mut std::ffi::c_void,
        _input: amaterasu_module_sdk::abi::BorrowedBytes,
        reply: amaterasu_module_sdk::abi::ReplySink,
    ) -> amaterasu_module_sdk::abi::Status {
        let bytes = serde_json::to_vec(&Response::Pending).unwrap();
        // SAFETY: the reply sink synchronously copies the response.
        unsafe { reply.send(&bytes) };
        amaterasu_module_sdk::abi::STATUS_OK
    }

    #[test]
    fn unexpected_admission_reply_retains_request_identity() {
        let endpoint = amaterasu_module_sdk::abi::HostApi {
            header: amaterasu_module_sdk::abi::AbiHeader::of::<amaterasu_module_sdk::abi::HostApi>(
            ),
            context: std::ptr::null_mut(),
            invoke: unexpected_admission_reply,
        };
        // SAFETY: the callback does not access its context.
        let runtime = unsafe { RuntimeClient::from_endpoint(endpoint) };
        let mut configuration = ProductConfiguration::new("test");
        let mut provider = Provider::new(Vec::new());
        configuration.advance(&runtime, &mut provider).unwrap();
        assert!(matches!(
            configuration.pending.as_ref().unwrap().call,
            PendingConfigurationCall::Submit {
                first_sent: Some(_),
                ..
            }
        ));
    }

    #[test]
    fn shutdown_releases_a_result_retained_by_normal_processing() {
        let mut configuration = ProductConfiguration::new("test");
        configuration.pending.as_mut().unwrap().call =
            PendingConfigurationCall::Waiting { request: 29 };
        configuration.retained_result = Some((
            29,
            Response::Result {
                payload: json!({"ok": true}),
            },
        ));
        assert!(configuration.poll_shutdown_with(&mut |command| {
            assert!(matches!(command, Command::ReleaseResult { request: 29 }));
            Ok(Response::error("not_found", "already released"))
        }));
    }

    unsafe extern "C" fn release_ack_lost(
        context: *mut std::ffi::c_void,
        input: amaterasu_module_sdk::abi::BorrowedBytes,
        reply: amaterasu_module_sdk::abi::ReplySink,
    ) -> amaterasu_module_sdk::abi::Status {
        use std::sync::atomic::{AtomicUsize, Ordering};
        // SAFETY: the test holds this context and command buffer during the call.
        let calls = unsafe { &*context.cast::<AtomicUsize>() };
        let command: Command = serde_json::from_slice(unsafe { input.read().unwrap() }).unwrap();
        let response = match command {
            Command::Request { .. } => Response::Accepted { id: 29 },
            Command::TakeResult { request: 29 } => Response::Result {
                payload: json!({"ok": false}),
            },
            Command::ReleaseResult { request: 29 } if calls.fetch_add(1, Ordering::SeqCst) == 0 => {
                return amaterasu_module_sdk::abi::STATUS_INTERNAL_ERROR;
            }
            Command::ReleaseResult { request: 29 } => {
                Response::error("not_found", "already released")
            }
            _ => panic!("unexpected command"),
        };
        let bytes = serde_json::to_vec(&response).unwrap();
        // SAFETY: the reply sink synchronously copies the response.
        unsafe { reply.send(&bytes) };
        amaterasu_module_sdk::abi::STATUS_OK
    }

    #[test]
    fn normal_advance_preserves_result_across_lost_release_acknowledgement() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let calls = AtomicUsize::new(0);
        let endpoint = amaterasu_module_sdk::abi::HostApi {
            header: amaterasu_module_sdk::abi::AbiHeader::of::<amaterasu_module_sdk::abi::HostApi>(
            ),
            context: std::ptr::from_ref(&calls).cast_mut().cast(),
            invoke: release_ack_lost,
        };
        // SAFETY: the endpoint context remains live throughout the test.
        let runtime = unsafe { RuntimeClient::from_endpoint(endpoint) };
        let mut configuration = ProductConfiguration::new("test");
        let mut provider = Provider::new(Vec::new());
        configuration.advance(&runtime, &mut provider).unwrap();
        configuration.advance(&runtime, &mut provider).unwrap();
        assert!(configuration.retained_result.is_some());
        configuration.pending.as_mut().unwrap().delay_ticks = 0;
        assert!(configuration.advance(&runtime, &mut provider).is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert!(configuration.retained_result.is_none());
    }

    unsafe extern "C" fn expired_update(
        _context: *mut std::ffi::c_void,
        input: amaterasu_module_sdk::abi::BorrowedBytes,
        reply: amaterasu_module_sdk::abi::ReplySink,
    ) -> amaterasu_module_sdk::abi::Status {
        // SAFETY: the runtime client supplies a live command buffer for this call.
        let command: Command = serde_json::from_slice(unsafe { input.read().unwrap() }).unwrap();
        assert!(matches!(command, Command::TakeResult { request: 31 }));
        let bytes = serde_json::to_vec(&Response::error("deadline_exceeded", "expired")).unwrap();
        // SAFETY: the reply sink synchronously copies the response.
        unsafe { reply.send(&bytes) };
        amaterasu_module_sdk::abi::STATUS_OK
    }

    #[test]
    fn expired_broker_result_keeps_delivered_update_fenced() {
        let endpoint = amaterasu_module_sdk::abi::HostApi {
            header: amaterasu_module_sdk::abi::AbiHeader::of::<amaterasu_module_sdk::abi::HostApi>(
            ),
            context: std::ptr::null_mut(),
            invoke: expired_update,
        };
        // SAFETY: the callback does not access its context.
        let runtime = unsafe { RuntimeClient::from_endpoint(endpoint) };
        let mut configuration = ProductConfiguration::new("test");
        configuration.pending = Some(PendingConfiguration {
            call: PendingConfigurationCall::Waiting { request: 31 },
            operation: PendingConfigurationOperation::Update {
                management_request: 7,
                expected_revision: 0,
                settings: ProductSettings::default(),
            },
            delay_ticks: 0,
        });
        let mut provider = Provider::new(Vec::new());
        configuration.advance(&runtime, &mut provider).unwrap();
        assert!(matches!(
            configuration.pending.as_ref().unwrap().call,
            PendingConfigurationCall::Waiting { request: 31 }
        ));
        assert!(!configuration.poll_shutdown(&runtime));
    }

    #[test]
    fn shutdown_fences_admission_after_broker_retention_window() {
        let mut configuration = ProductConfiguration::new("test");
        let PendingConfigurationCall::Submit { first_sent, .. } =
            &mut configuration.pending.as_mut().unwrap().call
        else {
            panic!()
        };
        *first_sent = Some(Instant::now() - Duration::from_secs(301));
        assert!(
            !configuration
                .poll_shutdown_with(&mut |_| panic!("expired identity must not be queried"))
        );
    }

    #[test]
    fn shutdown_rechecks_retention_after_missing_lookup_returns() {
        let now = Instant::now();
        let mut configuration = ProductConfiguration::new("test");
        let PendingConfigurationCall::Submit { first_sent, .. } =
            &mut configuration.pending.as_mut().unwrap().call
        else {
            panic!()
        };
        *first_sent = Some(now);
        let mut times = [
            now + Duration::from_secs(299),
            now + Duration::from_secs(300),
        ]
        .into_iter();
        assert!(!configuration.poll_shutdown_with_clock(
            &mut |command| {
                assert!(matches!(command, Command::LookupRequest { .. }));
                Ok(Response::error("not_found", "expired during lookup"))
            },
            &mut || times.next().unwrap(),
        ));
        assert!(configuration.pending.is_some());
    }

    #[test]
    fn shutdown_recovers_lost_admission_without_resubmitting_and_releases_result() {
        let mut configuration = ProductConfiguration::new("test");
        let pending = configuration.pending.as_mut().unwrap();
        let PendingConfigurationCall::Submit { first_sent, .. } = &mut pending.call else {
            panic!()
        };
        *first_sent = Some(Instant::now());
        assert!(!configuration.poll_shutdown_with(&mut |command| {
            assert!(matches!(command, Command::LookupRequest { .. }));
            Ok(Response::Accepted { id: 17 })
        }));
        assert!(!configuration.poll_shutdown_with(&mut |command| {
            assert!(matches!(command, Command::TakeResult { request: 17 }));
            Ok(Response::Pending)
        }));
        assert!(
            !configuration.poll_shutdown_with(&mut |command| match command {
                Command::TakeResult { request: 17 } => Ok(Response::Result {
                    payload: json!({"ok": true})
                }),
                Command::ReleaseResult { request: 17 } => Err("lost release".into()),
                _ => panic!("unexpected shutdown command"),
            })
        );
        assert!(configuration.poll_shutdown_with(&mut |command| {
            assert!(matches!(command, Command::ReleaseResult { request: 17 }));
            Ok(Response::error("not_found", "already released"))
        }));
    }

    #[test]
    fn shutdown_does_not_submit_an_unstarted_configuration_call() {
        let mut configuration = ProductConfiguration::new("test");
        assert!(configuration.poll_shutdown_with(&mut |_| panic!("unexpected broker call")));
    }

    #[test]
    fn enabled_provider_scans_after_settings_load_without_enabling_catalog_sync() {
        let mut configuration = ProductConfiguration::new("test");
        let mut provider = Provider::new(vec![std::path::PathBuf::from("/nonexistent-steam")]);
        let settings = ProductSettings {
            enabled: true,
            auto_sync: false,
            ..ProductSettings::default()
        };
        configuration.apply_settings(&mut provider, settings.clone(), 1);
        assert!(provider.scan_in_progress());
        assert!(!configuration.settings().auto_sync);

        provider.scan = None;
        configuration.apply_settings(&mut provider, settings.clone(), 2);
        assert!(!provider.scan_in_progress());
        configuration.apply_settings(
            &mut provider,
            ProductSettings {
                enabled: false,
                ..settings.clone()
            },
            3,
        );
        configuration.apply_settings(&mut provider, settings, 4);
        assert!(provider.scan_in_progress());
    }
}
