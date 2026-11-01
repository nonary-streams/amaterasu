//! Durable Steam product settings and their asynchronous configuration RPC lifecycle.
use crate::Provider;
use crate::settings::ProductSettings;
use amaterasu_module_sdk::{Command, ModuleResult, Response, RuntimeClient};
use serde_json::{Value, json};

const CONFIGURATION_SERVICE: &str = "amaterasu.configuration/1";
pub(super) const CONFIGURATION_TIMEOUT_MS: u64 = 30_000;

pub(super) struct ProductConfiguration {
    settings: ProductSettings,
    revision: u64,
    ready: bool,
    pending: Option<PendingConfiguration>,
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
    pub(super) fn new() -> Self {
        Self {
            settings: ProductSettings::default(),
            revision: 0,
            ready: false,
            pending: Some(PendingConfiguration {
                call: PendingConfigurationCall::Submit {
                    payload: json!({"action":"product_get"}),
                    timeout_ms: CONFIGURATION_TIMEOUT_MS,
                    operation_id: "steam.product-settings.load".into(),
                    parent: None,
                },
                operation: PendingConfigurationOperation::StartupLoad,
                delay_ticks: 0,
            }),
        }
    }

    pub(super) fn is_busy(&self) -> bool {
        self.pending.is_some()
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
            } => {
                let call = PendingConfigurationCall::Submit {
                    payload: payload.clone(),
                    timeout_ms,
                    operation_id: operation_id.clone(),
                    parent,
                };
                let retry_delay = background_retry_delay(&pending.operation);
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
                        self.handle_submission_failure(runtime, pending.operation, code, message)
                    }
                    Ok(response) => {
                        self.consume_result(runtime, provider, pending.operation, response)
                    }
                }
            }
            PendingConfigurationCall::Waiting { request } => {
                let response = match runtime.send(Command::TakeResult { request }) {
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
                };
                let Some(response) = release_terminal_child_result(request, response, |request| {
                    runtime.send(Command::ReleaseResult { request })
                }) else {
                    let delay_ticks = background_retry_delay(&pending.operation);
                    self.pending = Some(PendingConfiguration {
                        call: PendingConfigurationCall::Waiting { request },
                        operation: pending.operation,
                        delay_ticks,
                    });
                    return Ok(());
                };
                self.consume_result(runtime, provider, pending.operation, response)
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
                operation_id: format!("steam.product-settings.background-reconcile.{attempt}"),
                parent: None,
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
    fn enabled_provider_scans_after_settings_load_without_enabling_catalog_sync() {
        let mut configuration = ProductConfiguration::new();
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
